//! Shared schema + handlers + helpers for the scenario tests.
//!
//! Every scenario test binary pulls this in with `mod common;`. Since each
//! `tests/*.rs` file compiles to its own binary, the handlers below are
//! re-registered per binary via `inventory` — no cross-binary collisions.

// Scenario tables are created at runtime via `extra_sql`, so the query macros
// can't see them at compile time. Runtime `sqlx::query` is the right tool.
// Test helpers panic to signal failure — that is precisely their job.
#![allow(dead_code, clippy::disallowed_methods, clippy::panic)]

use std::collections::HashMap;
use std::time::Duration;

use forge::prelude::*;
use forge_harness::{HarnessApp, HarnessError, HarnessSession, SseEvent};

/// Schema applied to every scenario-test database, after the forge system
/// schema and before the test body runs. Reactivity is enabled per table so
/// mutations fan out to SSE subscribers.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE harness_counters (
    id    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name  TEXT   NOT NULL UNIQUE,
    value BIGINT NOT NULL DEFAULT 0
);
SELECT forge_enable_reactivity('harness_counters');

CREATE TABLE harness_widgets (
    id   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL
);
SELECT forge_enable_reactivity('harness_widgets');

CREATE TABLE harness_notes (
    id       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id UUID NOT NULL,
    body     TEXT NOT NULL
);
SELECT forge_enable_reactivity('harness_notes');
"#;

/// Boot a harness app with the scenario schema applied.
pub async fn start_app(test_name: &str) -> HarnessApp {
    HarnessApp::builder(test_name)
        .extra_sql(SCHEMA_SQL)
        .start()
        .await
        .expect("harness boot")
}

/// Poll `check` every 50ms until it returns `Some`, or panic after `within`.
pub async fn poll_until<T, F, Fut>(within: Duration, what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = std::time::Instant::now() + within;
    loop {
        if let Some(v) = check().await {
            return v;
        }
        if std::time::Instant::now() >= deadline {
            panic!("poll_until timed out waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Drain SSE events for the full `window`, returning the most-recent `Update`
/// payload seen per wire target (`sub:<id>` for queries, `job:<id>`, `wf:<id>`).
/// Draining the whole window — rather than returning at the first match — is
/// what makes "subscription X moved but Y stayed silent" assertions robust to
/// event ordering. Panics on any `error` frame: in a scenario test a reactor
/// error is a regression, not an expected outcome.
pub async fn collect_updates(
    session: &HarnessSession,
    window: Duration,
) -> HashMap<String, serde_json::Value> {
    let mut seen: HashMap<String, serde_json::Value> = HashMap::new();
    let deadline = std::time::Instant::now() + window;
    loop {
        let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
            Some(r) => r,
            None => return seen,
        };
        match session.next_event(remaining).await {
            Ok(SseEvent::Update { target, payload }) => {
                seen.insert(target, payload);
            }
            Ok(SseEvent::Error {
                target,
                code,
                message,
            }) => {
                panic!("reactor pushed an error for `{target}`: {code} {message}");
            }
            Ok(_) => continue,
            Err(HarnessError::Timeout { .. }) => return seen,
            Err(e) => panic!("sse stream failed while collecting updates: {e}"),
        }
    }
}

/// Read job-update SSE pushes for subscription `id`, in arrival order, until
/// the job reaches a terminal status (`completed`, `failed`, `dead_letter`,
/// `cancelled`). Panics on timeout or stream error. The returned vector always
/// ends with the terminal payload, so a caller can assert on the whole
/// lifecycle — progress included — as well as the final state.
pub async fn drain_job_updates(
    session: &HarnessSession,
    id: &str,
    within: Duration,
) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + within;
    let mut seen: Vec<serde_json::Value> = Vec::new();
    loop {
        let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
            Some(r) => r,
            None => panic!(
                "job `{id}` never reached a terminal state within budget; \
                 saw {} update(s): {seen:?}",
                seen.len(),
            ),
        };
        match session.next_job_update(id, remaining).await {
            Ok(payload) => {
                let terminal = matches!(
                    payload.get("status").and_then(serde_json::Value::as_str),
                    Some("completed" | "failed" | "dead_letter" | "cancelled"),
                );
                seen.push(payload);
                if terminal {
                    return seen;
                }
            }
            Err(HarnessError::Timeout { .. }) => panic!(
                "timed out waiting for job `{id}` to reach a terminal state; \
                 saw {} update(s): {seen:?}",
                seen.len(),
            ),
            Err(e) => panic!("sse stream failed draining job `{id}`: {e}"),
        }
    }
}

/// Wait until a workflow subscription reports one of the `wanted` statuses,
/// checking the subscribe-time `snapshot` first. Checking the snapshot is what
/// avoids a deadlock: a workflow can reach a stable state — `waiting`,
/// `completed` — before the subscription exists, after which no further push
/// ever arrives. Panics on timeout or stream error.
pub async fn await_workflow_status(
    session: &HarnessSession,
    id: &str,
    snapshot: &serde_json::Value,
    wanted: &[&str],
    within: Duration,
) -> serde_json::Value {
    let has_wanted_status = |v: &serde_json::Value| {
        v.get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| wanted.contains(&s))
    };
    if has_wanted_status(snapshot) {
        return snapshot.clone();
    }
    let deadline = std::time::Instant::now() + within;
    loop {
        let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
            Some(r) => r,
            None => panic!("workflow `{id}` never reached any of {wanted:?} within budget"),
        };
        match session.next_workflow_update(id, remaining).await {
            Ok(payload) => {
                if has_wanted_status(&payload) {
                    return payload;
                }
            }
            Err(HarnessError::Timeout { .. }) => {
                panic!("timed out waiting for workflow `{id}` to reach one of {wanted:?}")
            }
            Err(e) => panic!("sse stream failed awaiting workflow `{id}`: {e}"),
        }
    }
}

/// A counter row, used as the canonical reactive entity in scenario tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Counter {
    pub name: String,
    pub value: i64,
}

/// Input for [`harness_bump_counter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BumpInput {
    pub name: String,
    pub by: i64,
}

/// List every counter, ordered by name. Public, reactive on `harness_counters`.
#[forge::query(auth = "none", tables("harness_counters"))]
pub async fn harness_list_counters(ctx: &QueryContext) -> Result<Vec<Counter>> {
    sqlx::query_as::<_, Counter>("SELECT name, value FROM harness_counters ORDER BY name")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}

/// Fetch one counter by name, or `None` if it doesn't exist yet. Public,
/// reactive on `harness_counters`. Two subscriptions to this query with
/// different `name` args must invalidate independently — that is what proves
/// the reactor pushes per-result, not per-table.
#[forge::query(auth = "none", tables("harness_counters"))]
pub async fn harness_get_counter(ctx: &QueryContext, name: String) -> Result<Option<Counter>> {
    sqlx::query_as::<_, Counter>("SELECT name, value FROM harness_counters WHERE name = $1")
        .bind(&name)
        .fetch_optional(ctx.db())
        .await
        .map_err(Into::into)
}

/// Upsert a counter, incrementing its value. Triggers reactivity on
/// `harness_counters`.
#[forge::mutation(auth = "none")]
pub async fn harness_bump_counter(ctx: &MutationContext, input: BumpInput) -> Result<Counter> {
    let mut conn = ctx.conn().await?;
    sqlx::query_as::<_, Counter>(
        "INSERT INTO harness_counters (name, value) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET value = harness_counters.value + EXCLUDED.value
         RETURNING name, value",
    )
    .bind(&input.name)
    .bind(input.by)
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

/// Insert a widget row. Touches `harness_widgets` only — used to prove a
/// mutation on an unrelated table does NOT invalidate a counter subscription.
#[forge::mutation(auth = "none")]
pub async fn harness_add_widget(ctx: &MutationContext, name: String) -> Result<Uuid> {
    let mut conn = ctx.conn().await?;
    let row: (Uuid,) =
        sqlx::query_as("INSERT INTO harness_widgets (name) VALUES ($1) RETURNING id")
            .bind(&name)
            .fetch_one(&mut conn)
            .await?;
    Ok(row.0)
}

/// RPC dispatch result for a job: the gateway returns `{"job_id": "..."}`
/// when a job handler is invoked by name.
#[derive(Debug, Clone, Deserialize)]
pub struct JobHandle {
    pub job_id: String,
}

/// Input for [`harness_run_job`] and [`harness_failing_job`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJobInput {
    /// Number of progress steps the job reports before finishing.
    pub steps: i64,
}

/// Output of [`harness_run_job`]. Echoes the step count, which proves the
/// handler body actually executed rather than just being enqueued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJobOutput {
    pub processed: i64,
}

/// A background job that reports incremental progress, then completes. The
/// per-step sleep keeps the run long enough for a subscriber to attach and
/// observe lifecycle pushes ahead of the terminal state.
#[forge::job(auth = "none")]
pub async fn harness_run_job(ctx: &JobContext, input: RunJobInput) -> Result<RunJobOutput> {
    let total = input.steps.max(1);
    for step in 1..=total {
        let percent = u8::try_from(step * 100 / total).unwrap_or(100);
        let _ = ctx.progress(percent, format!("step {step} of {total}"));
        tokio::time::sleep(Duration::from_millis(180)).await;
    }
    Ok(RunJobOutput { processed: total })
}

/// A job that always fails on its first and only attempt. `max_attempts = 1`
/// makes the failure terminal immediately — no exponential-backoff retry wait
/// — so the failure path stays fast and deterministic to assert on.
#[forge::job(auth = "none", retry(max_attempts = 1))]
pub async fn harness_failing_job(_ctx: &JobContext, input: RunJobInput) -> Result<RunJobOutput> {
    Err(ForgeError::internal(format!(
        "harness_failing_job fails by design (steps={})",
        input.steps,
    )))
}

/// RPC dispatch result for a workflow: the gateway returns
/// `{"workflow_id": "..."}` when a workflow is invoked by name.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowHandle {
    pub workflow_id: String,
}

/// Input shared by the harness workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineInput {
    /// Echoed into the output, so a test can prove the input round-tripped
    /// through dispatch, persistence, and resumption intact.
    pub label: String,
}

/// Output of the harness workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineOutput {
    pub label: String,
    pub steps_run: i64,
}

/// A three-step workflow that runs straight through to completion with no
/// external wait. Proves step recording, ordering, and terminal output.
#[forge::workflow(name = "harness_linear", version = "v1", active, auth = "none")]
pub async fn harness_linear(ctx: &WorkflowContext, input: PipelineInput) -> Result<PipelineOutput> {
    if !ctx.is_step_completed("prepare") {
        ctx.record_step_start("prepare").await?;
        ctx.record_step_complete("prepare", serde_json::json!({ "ok": true }))
            .await?;
    }
    if !ctx.is_step_completed("process") {
        ctx.record_step_start("process").await?;
        ctx.record_step_complete("process", serde_json::json!({ "ok": true }))
            .await?;
    }
    if !ctx.is_step_completed("finalize") {
        ctx.record_step_start("finalize").await?;
        ctx.record_step_complete("finalize", serde_json::json!({ "ok": true }))
            .await?;
    }
    Ok(PipelineOutput {
        label: input.label,
        steps_run: 3,
    })
}

/// A workflow that records a step, then blocks on an external event, then
/// takes a short durable sleep, then finalizes. Exercises `wait_for_event`
/// and `sleep` — the two ways a durable workflow suspends and resumes.
#[forge::workflow(
    name = "harness_gated",
    version = "v1",
    active,
    timeout = "5m",
    auth = "none"
)]
pub async fn harness_gated(ctx: &WorkflowContext, input: PipelineInput) -> Result<PipelineOutput> {
    if !ctx.is_step_completed("open") {
        ctx.record_step_start("open").await?;
        ctx.record_step_complete("open", serde_json::json!({ "ok": true }))
            .await?;
    }
    if !ctx.is_step_completed("await_gate") {
        ctx.record_step_start("await_gate").await?;
        let _gate: serde_json::Value = ctx
            .wait_for_event("harness_gate_opened", Some(Duration::from_secs(30)))
            .await?;
        ctx.record_step_complete("await_gate", serde_json::json!({ "opened": true }))
            .await?;
    }
    if !ctx.is_step_completed("settle") {
        ctx.record_step_start("settle").await?;
        ctx.sleep(Duration::from_millis(400)).await?;
        ctx.record_step_complete("settle", serde_json::json!({ "settled": true }))
            .await?;
    }
    Ok(PipelineOutput {
        label: input.label,
        steps_run: 3,
    })
}

/// Fire the `harness_gate_opened` event that unblocks a waiting
/// [`harness_gated`] run. `workflow_id` is the run id the workflow's RPC
/// dispatch returned; it is the event's correlation id.
#[forge::mutation(auth = "none")]
pub async fn harness_open_gate(ctx: &MutationContext, workflow_id: String) -> Result<bool> {
    sqlx::query(
        "INSERT INTO forge_workflow_events (id, event_name, correlation_id, payload)
         VALUES (gen_random_uuid(), 'harness_gate_opened', $1, '{\"opened\": true}'::jsonb)",
    )
    .bind(&workflow_id)
    .execute(ctx.conn().await?.as_mut())
    .await
    .map_err(ForgeError::Database)?;
    Ok(true)
}

/// A user-owned note. `owner_id` is the authenticated caller's UUID; the auth
/// scenarios assert one user's notes never appear in another user's results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Note {
    pub owner_id: Uuid,
    pub body: String,
}

/// List the calling user's notes, ordered by body. Private by default (no
/// `auth = "none"`), so an anonymous caller is rejected by the gateway before
/// the body runs. The SQL filters by `owner_id`, but the runtime `query_as`
/// call form keeps that WHERE clause invisible to the macro's structural scope
/// lint, so `scope = "global"` opts out — runtime isolation via `ctx.user_id()`
/// is real and is exactly what the auth scenarios assert.
#[forge::query(scope = "global", tables("harness_notes"))]
pub async fn harness_my_notes(ctx: &QueryContext) -> Result<Vec<Note>> {
    let owner = ctx.user_id()?;
    sqlx::query_as::<_, Note>(
        "SELECT owner_id, body FROM harness_notes WHERE owner_id = $1 ORDER BY body",
    )
    .bind(owner)
    .fetch_all(ctx.db())
    .await
    .map_err(Into::into)
}

/// Create a note owned by the calling user. Private by default (no
/// `auth = "none"`), so an anonymous caller is rejected before the body runs.
/// A bare INSERT has no WHERE clause for the macro's structural scope lint to
/// inspect, so `scope = "global"` opts out — the row is still scoped at
/// runtime by stamping `owner_id` with `ctx.user_id()`.
#[forge::mutation(scope = "global", tables("harness_notes"))]
pub async fn harness_create_note(ctx: &MutationContext, body: String) -> Result<Note> {
    let owner = ctx.user_id()?;
    let mut conn = ctx.conn().await?;
    sqlx::query_as::<_, Note>(
        "INSERT INTO harness_notes (owner_id, body) VALUES ($1, $2)
         RETURNING owner_id, body",
    )
    .bind(owner)
    .bind(&body)
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

/// Count every note across all owners. Gated on the `admin` role: an
/// authenticated non-admin is rejected with 403, an admin succeeds. The total
/// is intentionally not user-scoped, hence `scope = "global"`.
#[forge::query(require_role = "admin", scope = "global", tables("harness_notes"))]
pub async fn harness_admin_note_count(ctx: &QueryContext) -> Result<i64> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM harness_notes")
        .fetch_one(ctx.db())
        .await?;
    Ok(row.0)
}

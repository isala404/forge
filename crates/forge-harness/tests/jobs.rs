//! Background-job scenarios.
//!
//! A job dispatched over RPC must land on the worker, run to a terminal
//! state, and stream its lifecycle — progress, then completion or failure —
//! to every SSE subscriber. This is the browserless proxy for "kick off an
//! export, watch the progress bar fill, see it finish": same gateway, same
//! worker, same `job:` SSE wire frames a browser client would consume.

/// Sentinel test so `cargo test -p forge-harness` (without `--features
/// testcontainers`) doesn't silently report "0 tests passed" and lull a
/// contributor into thinking they ran the scenario suite. Always passes;
/// its job is to print the hint.
#[test]
fn ensure_testcontainers_feature_enabled() {
    eprintln!(
        "forge-harness job scenarios are gated on `--features testcontainers`. \
         Re-run with `cargo test -p forge-harness --features testcontainers` \
         to exercise the worker against a real Postgres."
    );
}

#[cfg(feature = "testcontainers")]
#[path = "common/mod.rs"]
mod common;

#[cfg(feature = "testcontainers")]
use std::time::Duration;

#[cfg(feature = "testcontainers")]
use common::{JobHandle, RunJobInput, drain_job_updates, start_app};

/// Worker poll (50ms) + a few hundred ms of job work + reactor round-trips.
/// Generous enough to absorb CI scheduling noise without masking a hang.
#[cfg(feature = "testcontainers")]
const JOB_BUDGET: Duration = Duration::from_secs(10);

/// The core loop: dispatch a job, subscribe, and watch the worker stream it
/// from a non-terminal state through to `completed`, carrying the handler's
/// output on the final frame.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn job_runs_and_streams_lifecycle() {
    let app = start_app("jobs_lifecycle").await;
    let session = app.open_session(None).await.expect("open sse session");

    let handle: JobHandle = app
        .client()
        .call("harness_run_job", RunJobInput { steps: 4 })
        .await
        .expect("dispatch job");

    session
        .subscribe_job("run", &handle.job_id)
        .await
        .expect("subscribe to job");

    let updates = drain_job_updates(&session, "run", JOB_BUDGET).await;

    // A running job pushes lifecycle transitions as the worker advances it —
    // the SSE stream must carry more than a single terminal blob.
    assert!(
        updates.len() >= 2,
        "expected lifecycle pushes before completion, saw {}: {updates:?}",
        updates.len(),
    );

    // The wire `progress` field (JobData renames progress_percent) must never
    // regress across the run.
    let mut last_pct = -1_i64;
    for update in &updates {
        if let Some(pct) = update.get("progress").and_then(serde_json::Value::as_i64) {
            assert!(
                pct >= last_pct,
                "progress went backwards: {last_pct} -> {pct} in {updates:?}",
            );
            last_pct = pct;
        }
    }

    let terminal = updates
        .last()
        .expect("drain always yields the terminal update");
    assert_eq!(
        terminal.get("status").and_then(serde_json::Value::as_str),
        Some("completed"),
        "job must finish in the completed state, saw: {terminal}",
    );
    let processed = terminal
        .get("output")
        .and_then(|output| output.get("processed"))
        .and_then(serde_json::Value::as_i64);
    assert_eq!(
        processed,
        Some(4),
        "completed output must reflect every step the job ran",
    );

    app.shutdown().await.expect("shutdown");
}

/// A handler that returns `Err` must drive the job to a terminal failure
/// state and stream the error message — not silently vanish.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn failing_job_streams_terminal_failure() {
    let app = start_app("jobs_failure").await;
    let session = app.open_session(None).await.expect("open sse session");

    let handle: JobHandle = app
        .client()
        .call("harness_failing_job", RunJobInput { steps: 1 })
        .await
        .expect("dispatch failing job");

    session
        .subscribe_job("fail", &handle.job_id)
        .await
        .expect("subscribe to job");

    let updates = drain_job_updates(&session, "fail", JOB_BUDGET).await;
    let terminal = updates
        .last()
        .expect("drain always yields the terminal update");

    let status = terminal
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        matches!(status, "failed" | "dead_letter"),
        "a job whose handler errors must end failed, got `{status}`: {terminal}",
    );
    let error = terminal
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        !error.is_empty(),
        "a failed job must stream a non-empty error message, saw: {terminal}",
    );

    app.shutdown().await.expect("shutdown");
}

/// `subscribe-job` hands back the job's current state synchronously, at
/// subscribe time — the equivalent of a store's first value.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn subscribe_job_returns_initial_snapshot() {
    let app = start_app("jobs_snapshot").await;
    let session = app.open_session(None).await.expect("open sse session");

    let handle: JobHandle = app
        .client()
        .call("harness_run_job", RunJobInput { steps: 2 })
        .await
        .expect("dispatch job");

    let snapshot = session
        .subscribe_job("run", &handle.job_id)
        .await
        .expect("subscribe to job");

    assert_eq!(
        snapshot.get("job_id").and_then(serde_json::Value::as_str),
        Some(handle.job_id.as_str()),
        "snapshot must identify the job it describes",
    );
    assert!(
        snapshot
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "snapshot must carry a status, got: {snapshot}",
    );

    app.shutdown().await.expect("shutdown");
}

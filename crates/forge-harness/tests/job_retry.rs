//! Job retry + dead-letter scenarios.
//!
//! The effectiveness audit found these completely uncovered: the existing
//! failing-job test uses `max_attempts = 1`, so the retry counter, backoff
//! delay, `ctx.is_retry()` branch, and dead-letter routing never ran at any
//! layer. A wrong attempt comparison or an unapplied backoff would ship green.
//! These drive the real worker end-to-end against Postgres.

// Asserting on `forge_jobs` uses runtime `sqlx::query_as` (no compile-time DB):
// the harness owns no .sqlx cache, same as `common/mod.rs`. Tests panic to fail.
#![allow(clippy::disallowed_methods)]

/// Sentinel so `cargo test -p forge-harness` without the feature doesn't report
/// "0 tests passed" and lull a contributor into thinking the suite ran.
#[test]
fn ensure_testcontainers_feature_enabled() {
    eprintln!(
        "forge-harness job-retry scenarios are gated on `--features testcontainers`. \
         Re-run with `cargo test -p forge-harness --features testcontainers`."
    );
}

#[cfg(feature = "testcontainers")]
#[path = "common/mod.rs"]
mod common;

#[cfg(feature = "testcontainers")]
mod scenarios {
    use std::time::{Duration, Instant};

    use uuid::Uuid;

    use super::common::{JobHandle, RetryJobOutput, RunJobInput, drain_job_updates, start_app};

    /// Worker poll (50ms) + a ~1s fixed retry backoff + reactor round-trips.
    const BUDGET: Duration = Duration::from_secs(15);

    /// A job that errors on its first attempt must be retried after the backoff
    /// and succeed on the second run, with the attempt counter advanced.
    #[tokio::test]
    async fn job_retries_once_then_succeeds() {
        let app = start_app("job_retry_succeeds").await;
        let session = app.open_session(None).await.expect("open sse session");

        let handle: JobHandle = app
            .client()
            .call("harness_retry_job", RunJobInput { steps: 0 })
            .await
            .expect("dispatch retry job");

        session
            .subscribe_job("r", &handle.job_id)
            .await
            .expect("subscribe to job");

        let started = Instant::now();
        let updates = drain_job_updates(&session, "r", BUDGET).await;
        let terminal = updates.last().expect("drain yields the terminal update");

        assert_eq!(
            terminal.get("status").and_then(serde_json::Value::as_str),
            Some("completed"),
            "a job that fails once then succeeds must end completed, saw: {terminal}",
        );
        // The fixed backoff is ~1s (±25% jitter → ≥0.75s). A 500ms floor cleanly
        // separates "backoff applied" from an instant (~ms) re-run, with margin
        // for jitter. This is what regressed when retry delays under 1s were
        // truncated to 0 (queue.rs num_seconds bug).
        assert!(
            started.elapsed() >= Duration::from_millis(500),
            "retry must wait out the backoff, not re-run instantly ({:?})",
            started.elapsed(),
        );

        let (status, attempts): (String, i32) =
            sqlx::query_as("SELECT status, attempts FROM forge_jobs WHERE id = $1")
                .bind(Uuid::parse_str(&handle.job_id).expect("job id is a uuid"))
                .fetch_one(app.pool())
                .await
                .expect("job row");
        assert_eq!(status, "completed");
        assert_eq!(
            attempts, 2,
            "the attempt counter must advance across the retry"
        );

        let output: RetryJobOutput = serde_json::from_value(
            terminal
                .get("output")
                .cloned()
                .expect("completed job carries output"),
        )
        .expect("output deserializes to RetryJobOutput");
        assert!(
            output.was_retry,
            "the second run must observe ctx.is_retry()"
        );
        assert_eq!(output.attempt, 2);

        app.shutdown().await.expect("shutdown");
    }

    /// A job that always fails must exhaust exactly `max_attempts` and land in
    /// `dead_letter` — not retry forever, and not dead-letter prematurely.
    #[tokio::test]
    async fn job_dead_letters_after_exhausting_attempts() {
        let app = start_app("job_dead_letter").await;
        let session = app.open_session(None).await.expect("open sse session");

        let handle: JobHandle = app
            .client()
            .call("harness_dead_letter_job", RunJobInput { steps: 0 })
            .await
            .expect("dispatch dead-letter job");

        session
            .subscribe_job("d", &handle.job_id)
            .await
            .expect("subscribe to job");

        let updates = drain_job_updates(&session, "d", BUDGET).await;
        let terminal = updates.last().expect("drain yields the terminal update");

        assert_eq!(
            terminal.get("status").and_then(serde_json::Value::as_str),
            Some("dead_letter"),
            "a job that always fails must dead-letter after max attempts, saw: {terminal}",
        );

        let (status, attempts): (String, i32) =
            sqlx::query_as("SELECT status, attempts FROM forge_jobs WHERE id = $1")
                .bind(Uuid::parse_str(&handle.job_id).expect("job id is a uuid"))
                .fetch_one(app.pool())
                .await
                .expect("job row");
        assert_eq!(status, "dead_letter");
        assert_eq!(
            attempts, 2,
            "must exhaust exactly max_attempts before dead-lettering"
        );

        let error = terminal
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        assert!(
            !error.is_empty(),
            "a dead-lettered job must stream a non-empty error, saw: {terminal}",
        );

        app.shutdown().await.expect("shutdown");
    }
}

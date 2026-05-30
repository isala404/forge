//! Transactional integrity.
//!
//! A mutation that writes a row and dispatches a job, then errors, must leave
//! NOTHING behind: both the data row and the buffered job (dispatched on the
//! transaction via the outbox path) roll back together. The audit found the
//! harness only ever committed on success — a commit-on-error or
//! job-written-outside-the-tx regression would silently corrupt data and pass.

// Asserting on harness_widgets / forge_jobs uses runtime sqlx (no compile-time
// DB); same rationale as common/mod.rs. Tests panic to fail.
#![allow(clippy::disallowed_methods)]

/// Sentinel so the suite isn't silently empty without `--features testcontainers`.
#[test]
fn ensure_testcontainers_feature_enabled() {
    eprintln!(
        "forge-harness tx-rollback scenario is gated on `--features testcontainers`. \
         Re-run with `cargo test -p forge-harness --features testcontainers`."
    );
}

#[cfg(feature = "testcontainers")]
#[path = "common/mod.rs"]
mod common;

#[cfg(feature = "testcontainers")]
mod scenarios {
    use super::common::start_app;

    /// The mutation errors after both the INSERT and the dispatch. Afterwards
    /// neither the widget nor the job may exist — proving `execute_transactional`
    /// rolls the whole unit back, outbox job included.
    #[tokio::test]
    async fn transactional_mutation_error_rolls_back_widget_and_job() {
        let app = start_app("tx_rollback").await;
        let name = "rollback-me-7a3f";

        let err = app
            .client()
            .expect_error("harness_tx_rollback", name)
            .await
            .expect("mutation must fail so the transaction unwinds");
        assert!(!err.code.is_empty(), "error envelope must carry a code");

        let widgets: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM harness_widgets WHERE name = $1")
                .bind(name)
                .fetch_one(app.pool())
                .await
                .expect("count widgets");
        assert_eq!(
            widgets.0, 0,
            "the widget INSERT must roll back when the mutation errors",
        );

        let jobs: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM forge_jobs WHERE job_type = 'harness_run_job'")
                .fetch_one(app.pool())
                .await
                .expect("count jobs");
        assert_eq!(
            jobs.0, 0,
            "the dispatched job must roll back with the transaction (outbox-on-tx)",
        );

        app.shutdown().await.expect("shutdown");
    }
}

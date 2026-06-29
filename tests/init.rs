#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forgelib::testing::TestDatabase;
use forgelib::{EnqueueOpts, Forge, ForgeError};

#[tokio::test]
async fn init_runs_migrations_and_is_idempotent() {
    let db = TestDatabase::new().await.unwrap();
    let _f1 = db.forge().await.unwrap();
    // Re-init must verify checksums of already-applied migrations without erroring.
    let _f2 = db.forge().await.unwrap();
}

#[tokio::test]
async fn init_requires_at_least_two_connections_to_migrate() {
    let db = TestDatabase::new().await.unwrap();
    // Init migrates at startup, holding a lock connection while a second runs the SQL, so
    // max_connections=1 would deadlock; fail loudly at init instead.
    let res = db.forge_with("max_connections = 1\n").await;
    assert!(matches!(res, Err(ForgeError::Config(_))));

    // Two is enough: one for the lock, one for the migration SQL.
    let ok = db.forge_with("max_connections = 2\n").await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn bad_connection_string_fails_at_init() {
    // Nothing listens on port 1 → connection refused, surfaced as Config.
    let cfg = "[postgres]\n\
        url = \"postgres://postgres:forge@127.0.0.1:1/forge_dev\"\n\
        acquire_timeout_secs = 2\n";
    assert!(matches!(
        Forge::init_from_str(cfg).await,
        Err(ForgeError::Config(_))
    ));
}

#[tokio::test]
async fn maintain_runs_cleanly() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    forge
        .queue()
        .enqueue("q", Bytes::from_static(b"x"), EnqueueOpts::new())
        .await
        .unwrap();
    forge.maintain().await.unwrap();
}

#[tokio::test]
async fn default_backend_report_is_all_postgres() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let report = forge.backend_report();
    assert_eq!(report.backends.len(), 8, "one entry per primitive");
    assert!(
        report.backends.iter().all(|b| b.provider == "postgres"),
        "default config powers every primitive with Postgres"
    );
    assert!(report.backends.iter().all(|b| b.durable));
    // Display renders one line per primitive plus a header.
    let rendered = report.to_string();
    assert!(rendered.contains("forge backend report:"));
    assert!(rendered.contains("blob"));
}

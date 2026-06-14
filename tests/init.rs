//! Facade init + migration behavior. Run with `cargo test --features pg-tests`.
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forge::testing::TestDatabase;
use forge::{EnqueueOpts, Forge, ForgeConfig, ForgeError};
use std::time::Duration;

#[tokio::test]
async fn init_runs_migrations_and_is_idempotent() {
    let db = TestDatabase::new().await.unwrap();
    let _f1 = db.forge().await.unwrap();
    // Re-init must verify checksums of already-applied migrations without erroring.
    let _f2 = db.forge().await.unwrap();
}

#[tokio::test]
async fn without_migrations_requires_the_schema_present() {
    let db = TestDatabase::new().await.unwrap();

    // Migrations disabled on a fresh db => init must refuse (schema missing).
    let res = Forge::init(ForgeConfig::new(db.url()).without_migrations()).await;
    assert!(
        matches!(res, Err(ForgeError::Config(_))),
        "missing schema must fail at init"
    );

    let _ = db.forge().await.unwrap();
    let ok = Forge::init(ForgeConfig::new(db.url()).without_migrations()).await;
    assert!(ok.is_ok(), "schema present => verify-only init succeeds");
}

#[tokio::test]
async fn migrations_require_at_least_two_connections() {
    let db = TestDatabase::new().await.unwrap();
    // Migrations hold a lock connection while a second runs the SQL, so max_connections=1
    // would deadlock — fail loudly at init instead.
    let res = Forge::init(ForgeConfig::new(db.url()).with_max_connections(1)).await;
    assert!(matches!(res, Err(ForgeError::Config(_))));

    // With migrations applied out of band, a single connection is fine.
    let _ = db.forge().await.unwrap();
    let ok = Forge::init(
        ForgeConfig::new(db.url())
            .with_max_connections(1)
            .without_migrations(),
    )
    .await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn bad_connection_string_fails_at_init() {
    // Nothing listens on port 1 → connection refused, surfaced as Config.
    let cfg = ForgeConfig::new("postgres://postgres:forge@127.0.0.1:1/forge_dev")
        .with_acquire_timeout(Duration::from_secs(2));
    assert!(matches!(Forge::init(cfg).await, Err(ForgeError::Config(_))));
}

#[tokio::test]
async fn incompatible_preexisting_table_fails_structural_check() {
    let db = TestDatabase::new().await.unwrap();

    // Simulate a user who already had a `forge_kv` table with an incompatible shape:
    // the `value` column is TEXT, not BYTEA. The `CREATE TABLE IF NOT EXISTS` migration
    // leaves it untouched and records itself as applied, so only the structural check
    // can catch the mismatch.
    db.execute_raw(
        "CREATE TABLE forge_kv (key TEXT PRIMARY KEY, value TEXT, expires_at TIMESTAMPTZ)",
    )
    .await
    .unwrap();

    let res = Forge::init(ForgeConfig::new(db.url())).await;
    match res {
        Err(ForgeError::Config(msg)) => {
            assert!(
                msg.contains("forge_kv") && msg.contains("value"),
                "expected a precise column-type mismatch message, got: {msg}"
            );
        }
        Err(other) => panic!("expected Config error for incompatible forge_kv, got {other:?}"),
        Ok(_) => panic!("expected Config error for incompatible forge_kv, got Ok"),
    }
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

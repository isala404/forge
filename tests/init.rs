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

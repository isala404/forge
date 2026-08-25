#![cfg(feature = "pg-tests")]
// Test assertions intentionally index the single-target report, and the dynamic advisory
// lock query has no application input and cannot use SQLx's offline macro cache here.
#![allow(
    clippy::disallowed_methods,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use forgelib::testing::TestDatabase;
use forgelib::{Forge, MigrationState};
use sqlx::{Connection, PgConnection};
use std::time::{Duration, Instant};

#[tokio::test]
async fn explicit_migration_gates_production_startup() {
    let database = TestDatabase::new().await.unwrap();
    let config = database.config_toml(
        "max_connections = 1\nauto_migrate = false\n[forge]\nenvironment = \"production\"\n",
    );

    let before = Forge::migration_status_from_str(&config).await.unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].state, MigrationState::Pending);
    assert!(Forge::init_from_str(&config).await.is_err());

    let migrated = Forge::migrate_from_str(&config).await.unwrap();
    assert_eq!(migrated[0].state, MigrationState::Applied);
    assert!(migrated[0].pending.is_empty());

    let validated = Forge::validate_schema_from_str(&config).await.unwrap();
    assert_eq!(validated[0].state, MigrationState::Applied);
    let forge = Forge::init_from_str(&config).await.unwrap();
    forge.close(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn unknown_migration_history_is_incompatible() {
    let database = TestDatabase::new().await.unwrap();
    let config = database.config_toml("auto_migrate = false\n");
    Forge::migrate_from_str(&config).await.unwrap();

    database
        .execute_raw(
            "INSERT INTO forge_system_migrations (version, checksum) VALUES ('v999_unknown', 'unknown')",
        )
        .await
        .unwrap();
    let incompatible = Forge::validate_schema_from_str(&config).await.unwrap();
    assert_eq!(incompatible[0].state, MigrationState::Incompatible);
    assert!(incompatible[0].message.contains("v999_unknown"));
    assert!(Forge::init_from_str(&config).await.is_err());
}

#[tokio::test]
async fn migration_lock_wait_is_bounded_and_identifies_the_holder() {
    let database = TestDatabase::new().await.unwrap();
    let mut holder = PgConnection::connect(database.url()).await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(0x0046_4F52_4745_i64)
        .execute(&mut holder)
        .await
        .unwrap();

    let config = database.config_toml("migration_lock_timeout_secs = 0.2\n");
    let started = Instant::now();
    let reports = Forge::migrate_from_str(&config).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(reports[0].state, MigrationState::Locked);
    assert!(reports[0].lock_holder.is_some());

    holder.close().await.unwrap();
}

#[tokio::test]
async fn validation_only_replicas_do_not_contend_on_the_migration_lock() {
    let database = TestDatabase::new().await.unwrap();
    database
        .forge()
        .await
        .unwrap()
        .close(Duration::from_secs(2))
        .await
        .unwrap();
    let mut holder = PgConnection::connect(database.url()).await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(0x0046_4F52_4745_i64)
        .execute(&mut holder)
        .await
        .unwrap();

    let config = database.config_toml("auto_migrate = false\nmax_connections = 2\n");
    let replicas = futures_util::future::join_all((0..4).map(|_| {
        let config = config.clone();
        async move { Forge::init_from_str(&config).await }
    }))
    .await;
    for replica in replicas {
        replica
            .unwrap()
            .close(Duration::from_secs(2))
            .await
            .unwrap();
    }

    holder.close().await.unwrap();
}

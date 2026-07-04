#[cfg(feature = "embedded")]
pub(crate) mod embedded;
mod migrations;
pub(crate) use migrations::MigrationRunner;

use crate::config::DatabaseConfig;
use crate::error::{ForgeError, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

/// Lowest supported `server_version_num` (PG 17): the supported floor. The SQL itself
/// only needs PG 14 features (`SKIP LOCKED`, advisory locks, partial indexes,
/// `gen_random_uuid`), but 17 is the oldest version we test and ship embedded.
const MIN_SERVER_VERSION_NUM: i32 = 170_000;

/// Returns a probed, ready pool, or a `ForgeError::Config` on any connection or version failure.
pub(crate) async fn connect(db: &DatabaseConfig) -> Result<sqlx::PgPool> {
    let mut opts = PgConnectOptions::from_str(&db.postgres)
        .map_err(|e| ForgeError::config(format!("invalid postgres connection string: {e}")))?
        .application_name("forge");

    // Server-side ceilings applied at connection startup (on every reconnect, no extra
    // round-trip, no query text, so the offline sqlx cache stays untouched). They bound
    // runtime statements; migrations override them inline with their own longer `SET LOCAL`
    // limits. A zero `Duration` opts out. These timeouts take milliseconds as a unitless
    // integer.
    let mut runtime_opts: Vec<(&str, String)> = Vec::new();
    if !db.statement_timeout.is_zero() {
        runtime_opts.push((
            "statement_timeout",
            db.statement_timeout.as_millis().to_string(),
        ));
    }
    if !db.lock_timeout.is_zero() {
        runtime_opts.push(("lock_timeout", db.lock_timeout.as_millis().to_string()));
    }
    if !db.idle_in_transaction_timeout.is_zero() {
        runtime_opts.push((
            "idle_in_transaction_session_timeout",
            db.idle_in_transaction_timeout.as_millis().to_string(),
        ));
    }
    if !runtime_opts.is_empty() {
        opts = opts.options(runtime_opts);
    }

    let pool = PgPoolOptions::new()
        .max_connections(db.max_connections)
        .acquire_timeout(db.acquire_timeout)
        .connect_with(opts)
        .await
        .map_err(|e| {
            ForgeError::config(format!(
                "could not connect to postgres (check host/credentials in FORGE_POSTGRES_URL): {}",
                conn_cause(&e)
            ))
        })?;

    ping(&pool).await?;
    verify_version(&pool).await?;
    Ok(pool)
}

/// `SELECT 1` round-trip so a dead server fails at init.
async fn ping(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
        .fetch_one(pool)
        .await
        .map_err(|e| ForgeError::config(format!("postgres ping failed: {}", conn_cause(&e))))?;
    Ok(())
}

async fn verify_version(pool: &sqlx::PgPool) -> Result<()> {
    let num: i32 =
        sqlx::query_scalar!(r#"SELECT current_setting('server_version_num')::int AS "v!""#)
            .fetch_one(pool)
            .await
            .map_err(|e| {
                ForgeError::config(format!(
                    "could not read postgres version: {}",
                    conn_cause(&e)
                ))
            })?;
    if num < MIN_SERVER_VERSION_NUM {
        return Err(ForgeError::config(format!(
            "Postgres server_version_num={num} is too old; Forge requires >= {MIN_SERVER_VERSION_NUM} (PG 17)"
        )));
    }
    Ok(())
}

/// Secret-safe rendering of a connection error (never echoes the DSN/password).
fn conn_cause(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(db) => db.message().to_string(),
        sqlx::Error::Io(io) => format!("io error: {}", io.kind()),
        sqlx::Error::PoolTimedOut => "connection pool timed out".to_string(),
        sqlx::Error::PoolClosed => "connection pool closed".to_string(),
        other => other.to_string(),
    }
}

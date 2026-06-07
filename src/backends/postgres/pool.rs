//! Pool construction and init-time probes. Every failure is a
//! [`ForgeError::Config`]: misconfiguration fails loudly in `Forge::init`, never lazily on first use.

use crate::config::ForgeConfig;
use crate::error::{ForgeError, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

/// Lowest supported `server_version_num` (PG 14): the floor for `SKIP LOCKED`,
/// advisory locks, partial indexes, and `gen_random_uuid`.
const MIN_SERVER_VERSION_NUM: i32 = 140_000;

/// Connect, build the pool, and probe the server, returning a ready pool or a precise `Config` error.
pub(crate) async fn connect(cfg: &ForgeConfig) -> Result<sqlx::PgPool> {
    let opts = PgConnectOptions::from_str(&cfg.postgres)
        .map_err(|e| ForgeError::config(format!("invalid postgres connection string: {e}")))?
        .application_name("forge");

    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(cfg.acquire_timeout)
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

/// Refuse to start against a Postgres older than [`MIN_SERVER_VERSION_NUM`].
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
            "Postgres server_version_num={num} is too old; Forge requires >= {MIN_SERVER_VERSION_NUM} (PG 14)"
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

//! PostgreSQL connection pool management.
//!
//! # Single-pool concurrency model
//!
//! Forge runs every workload (gateway requests, job/cron workers, the
//! workflow scheduler, the reactor, periodic cleanup, signals batch inserts)
//! against a single primary pool. There are no per-workload pool slots and
//! none are coming back: the previous multi-pool layout (`pools.jobs`,
//! `pools.observability`, `pools.analytics`) was removed in Phase 2 because
//! split pools made the connection budget impossible to reason about and
//! produced nasty starvation modes when one pool was full and another idle.
//!
//! The single pool is fairer in steady state but exposes one real risk: a
//! burst of slow background work can drain the budget that gateway requests
//! need. Doctrine still says one pool, so the framework manages contention
//! by throttling at the workload level instead:
//!
//! - **Job/cron worker**: capped at `worker.max_concurrent` running tasks
//!   ([`crate::jobs::worker::WorkerConfig::max_concurrent`]). Each task
//!   holds at most one connection at a time, so this directly bounds the
//!   slice of the pool that workers can take.
//! - **Reactor re-execution**: capped at the realtime semaphore
//!   ([`crate::realtime::reactor`] uses a `Semaphore` sized to the configured
//!   max). Live re-execution can't oversubscribe the pool beyond that.
//! - **Cleanup crons** (refresh-token purge, oauth-code purge, expired-job
//!   delete, invalidation purge, etc.) route through the same worker
//!   semaphore via cron-as-job, so they share the worker cap.
//! - **Workflow execution**: durable workflows run inside `$workflow_resume`
//!   jobs, inheriting the worker semaphore transitively. The workflow
//!   scheduler itself only polls and dispatches.
//! - **Gateway requests**: not throttled at the pool level. They take from
//!   whatever the workers and reactor have left.
//!
//! ## Persistent connection holders
//!
//! Several runtime components hold a connection for the process lifetime
//! rather than acquiring per-operation. These come out of the pool budget
//! the moment the runtime starts and never return until shutdown:
//!
//! - Change listener (`LISTEN forge_changes`), one connection per node.
//! - Workflow scheduler listener (`LISTEN forge_workflow_wakeup`), one per
//!   node when workflows are registered.
//! - Job worker wakeup listener (`LISTEN forge_jobs_available`), one per worker.
//! - Leader election lock holder, one per leader role this node owns. A node
//!   that wins both the cron and signals leader roles holds two.
//!
//! Plan for two to three persistent connections per node in steady state,
//! plus one per leader role this node may win.
//!
//! ## Sizing formula
//!
//! The recommended `database.pool_size` is:
//!
//! ```text
//! pool_size >= worker.max_concurrent
//!            + realtime.max_concurrent_reexecution
//!            + expected_concurrent_gateway_requests
//!            + ~6   // persistent listeners, leader holds, migrations, health check
//! ```
//!
//! For default worker (14: 8 default + 4 workflows + 2 cron) and reactor
//! (64) caps the internal baseline alone is 84 connections. The 100-connection
//! default covers the internal baseline plus ~16 concurrent gateway requests.
//! Larger deployments should set `pool_size` explicitly.
//!
//! Note that gateway's `max_connections` (default 512) is the request-level
//! concurrency cap, not the pool-level one. A real deployment rarely sees
//! 512 concurrent in-flight DB queries because most requests are cached or
//! short-lived. Size by *expected concurrent active queries*, not by the
//! gateway cap.
//!
//! ## What's NOT gated
//!
//! - Migration runner (one-shot at startup, takes one connection).
//! - Replica health monitor (one query every 15s, brief).
//! - Signals event batch flush (one task, batches via UNNEST so each insert
//!   is one connection-trip even for large batches).
//!
//! ## When the model breaks
//!
//! If a production load test shows gateway acquisition timeouts under job
//! load, the answer is to raise `pool_size` and/or lower
//! `worker.max_concurrent`, not to introduce per-workload pools. Tagged
//! semaphores at the acquire site will be added if a real workload is found
//! that can't be sized via the existing knobs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::log::LevelFilter;

use forge_core::config::DatabaseConfig;
use forge_core::error::{ForgeError, Result};

struct ReplicaEntry {
    pool: Arc<PgPool>,
    healthy: Arc<AtomicBool>,
}

/// Database connection wrapper with health-aware replica routing.
#[derive(Clone)]
pub struct Database {
    primary: Arc<PgPool>,
    replicas: Arc<Vec<ReplicaEntry>>,
    config: DatabaseConfig,
    replica_counter: Arc<AtomicUsize>,
}

impl Database {
    pub async fn from_config(config: &DatabaseConfig) -> Result<Self> {
        Self::from_config_with_service(config, "forge").await
    }

    /// Create a new database connection with a service name for tracing.
    ///
    /// The service name is set as PostgreSQL's `application_name`, visible in
    /// `pg_stat_activity` for correlating queries to the originating service.
    pub async fn from_config_with_service(
        config: &DatabaseConfig,
        service_name: &str,
    ) -> Result<Self> {
        if config.url.is_empty() {
            return Err(ForgeError::internal(
                "database.url cannot be empty. Provide a PostgreSQL connection URL.",
            ));
        }

        let primary = Self::create_pool_with_statement_timeout(
            &config.url,
            config.pool_size,
            config.min_pool_size,
            config.pool_timeout.as_secs(),
            config.statement_timeout.as_secs(),
            config.test_before_acquire,
            service_name,
        )
        .await
        .map_err(|e| ForgeError::internal_with("Failed to connect to primary", e))?;

        verify_postgres_version(&primary, "primary").await?;
        detect_pgbouncer(&primary).await?;

        let mut replicas = Vec::new();
        for replica_url in &config.replica_urls {
            let pool = Self::create_pool(
                replica_url,
                config.replica_pool_size.unwrap_or(config.pool_size / 2),
                config.pool_timeout.as_secs(),
                service_name,
            )
            .await
            .map_err(|e| ForgeError::internal_with("Failed to connect to replica", e))?;
            verify_postgres_version(&pool, "replica").await?;
            replicas.push(ReplicaEntry {
                pool: Arc::new(pool),
                healthy: Arc::new(AtomicBool::new(true)),
            });
        }

        Ok(Self {
            primary: Arc::new(primary),
            replicas: Arc::new(replicas),
            config: config.clone(),
            replica_counter: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn connect_options(url: &str, service_name: &str) -> sqlx::Result<PgConnectOptions> {
        let options: PgConnectOptions = url.parse()?;
        Ok(options
            .application_name(service_name)
            .log_statements(LevelFilter::Off)
            .log_slow_statements(LevelFilter::Warn, Duration::from_millis(500)))
    }

    fn connect_options_with_timeout(
        url: &str,
        service_name: &str,
        statement_timeout_secs: u64,
    ) -> sqlx::Result<PgConnectOptions> {
        let options: PgConnectOptions = url.parse()?;
        let mut opts = options
            .application_name(service_name)
            .log_statements(LevelFilter::Off)
            .log_slow_statements(LevelFilter::Warn, Duration::from_millis(500));
        if statement_timeout_secs > 0 {
            opts = opts.options([("statement_timeout", &format!("{}s", statement_timeout_secs))]);
        }
        Ok(opts)
    }

    async fn create_pool(
        url: &str,
        size: u32,
        timeout_secs: u64,
        service_name: &str,
    ) -> sqlx::Result<PgPool> {
        Self::create_pool_with_opts(url, size, 0, timeout_secs, true, service_name).await
    }

    async fn create_pool_with_opts(
        url: &str,
        size: u32,
        min_size: u32,
        timeout_secs: u64,
        test_before_acquire: bool,
        service_name: &str,
    ) -> sqlx::Result<PgPool> {
        Self::create_pool_with_statement_timeout(
            url,
            size,
            min_size,
            timeout_secs,
            0,
            test_before_acquire,
            service_name,
        )
        .await
    }

    async fn create_pool_with_statement_timeout(
        url: &str,
        size: u32,
        min_size: u32,
        timeout_secs: u64,
        statement_timeout_secs: u64,
        test_before_acquire: bool,
        service_name: &str,
    ) -> sqlx::Result<PgPool> {
        let options = if statement_timeout_secs > 0 {
            Self::connect_options_with_timeout(url, service_name, statement_timeout_secs)?
        } else {
            Self::connect_options(url, service_name)?
        };
        PgPoolOptions::new()
            .max_connections(size)
            .min_connections(min_size)
            .acquire_timeout(Duration::from_secs(timeout_secs))
            .test_before_acquire(test_before_acquire)
            .connect_with(options)
            .await
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// Get a pool for reads. Skips unhealthy replicas, falls back to primary.
    pub fn read_pool(&self) -> &PgPool {
        if !self.config.read_from_replica || self.replicas.is_empty() {
            return &self.primary;
        }

        let len = self.replicas.len();
        let start = self.replica_counter.fetch_add(1, Ordering::Relaxed) % len;

        for offset in 0..len {
            let idx = (start + offset) % len;
            if let Some(entry) = self.replicas.get(idx)
                && entry.healthy.load(Ordering::Relaxed)
            {
                return &entry.pool;
            }
        }

        &self.primary
    }

    /// Start background health monitoring for replicas. Returns None when no
    /// replicas are configured. The task exits when `shutdown_rx` receives a
    /// message or the broadcast channel closes, so callers can join the handle
    /// during graceful shutdown without leaking the loop.
    pub fn start_health_monitor(
        &self,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> Option<JoinHandle<()>> {
        if self.replicas.is_empty() {
            return None;
        }

        let replicas = Arc::clone(&self.replicas);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("Replica health monitor shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        for entry in replicas.iter() {
                            let ok = sqlx::query_scalar!("SELECT 1 as \"v!\"")
                                .fetch_one(entry.pool.as_ref())
                                .await
                                .is_ok();
                            let was_healthy = entry.healthy.swap(ok, Ordering::Relaxed);
                            if was_healthy && !ok {
                                tracing::warn!("Replica marked unhealthy");
                            } else if !was_healthy && ok {
                                tracing::info!("Replica recovered");
                            }
                        }
                    }
                }
            }
        });

        Some(handle)
    }

    /// Create a Database wrapper from an existing pool. Hidden from rustdoc:
    /// production code goes through `from_config`, this exists for in-crate
    /// tests and external integration harnesses that already own a `PgPool`.
    #[doc(hidden)]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            primary: Arc::new(pool),
            replicas: Arc::new(Vec::new()),
            config: DatabaseConfig::default(),
            replica_counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn health_check(&self) -> Result<()> {
        sqlx::query_scalar!("SELECT 1 as \"v!\"")
            .fetch_one(self.primary.as_ref())
            .await
            .map_err(|e| ForgeError::internal_with("Health check failed", e))?;
        Ok(())
    }

    pub async fn close(&self) {
        self.primary.close().await;
        for entry in self.replicas.iter() {
            entry.pool.close().await;
        }
    }
}

/// Minimum supported PostgreSQL major version.
///
/// Forge v0.9+ uses features (skip-locked semantics with `NOWAIT`, partitioned
/// table SET ACCESS METHOD, `pg_stat_statements.toplevel`, generated-column
/// stored arrays of UUID, NOTIFY queue introspection via
/// `pg_notification_queue_usage()`) that all require PostgreSQL 18 or newer.
/// We hard-fail at startup so deployments don't discover an incompatibility
/// at runtime under load.
pub const MIN_POSTGRES_MAJOR: i32 = 18;

/// Read the server's `server_version_num` and refuse to continue when the
/// major version is below [`MIN_POSTGRES_MAJOR`]. The check uses a temporary
/// connection from the supplied pool, so it works against both primary and
/// replica before they're handed to the rest of the system.
async fn verify_postgres_version(pool: &PgPool, role: &str) -> Result<()> {
    let row = sqlx::query_scalar!("SELECT current_setting('server_version_num')")
        .fetch_one(pool)
        .await
        .map_err(|e| {
            ForgeError::internal(format!(
                "Failed to read PostgreSQL server_version_num from {}: {}",
                role, e
            ))
        })?;

    let version_num: i32 = row
        .ok_or_else(|| {
            ForgeError::internal(format!(
                "PostgreSQL {} returned NULL for server_version_num",
                role
            ))
        })?
        .parse()
        .map_err(|e| {
            ForgeError::internal(format!(
                "Could not parse PostgreSQL server_version_num from {}: {}",
                role, e
            ))
        })?;

    let major = version_num / 10_000;
    if major < MIN_POSTGRES_MAJOR {
        return Err(ForgeError::internal(format!(
            "PostgreSQL {} is at version {} but Forge requires {} or newer. \
             See https://forge.dev/scale/hosting for supported versions.",
            role, major, MIN_POSTGRES_MAJOR
        )));
    }
    tracing::debug!(role, postgres_major = major, "PostgreSQL version verified");
    Ok(())
}

/// Detect PgBouncer proxies and refuse to start when one is found.
///
/// PgBouncer in transaction-pooling mode breaks two Forge primitives:
///   1. `pg_try_advisory_lock` — advisory locks are per-connection; with
///      transaction pooling the connection changes between calls so the lock
///      is silently lost after each transaction boundary.
///   2. `LISTEN/NOTIFY` — persistent listeners require a stable connection;
///      PgBouncer can reassign or close the underlying connection without
///      Forge's listener task knowing, causing silent change-event loss.
///
/// Session-pooling mode does keep the connection, but Forge cannot distinguish
/// it from transaction mode reliably at runtime. If you need connection pooling,
/// use `pgcat` in query-router mode or rely on SQLx's built-in pool, which
/// already amortises connection overhead without the above drawbacks.
///
/// Detection strategy:
///   - `pg_backend_pid()` always returns a positive integer on real Postgres.
///     PgBouncer returns 0 in transaction-pooling mode.
///   - `version()` on PgBouncer returns a string containing "PgBouncer".
///
/// Both checks run so a future PgBouncer version that fixes `pg_backend_pid`
/// can still be detected via the version string.
async fn detect_pgbouncer(pool: &PgPool) -> Result<()> {
    // Runtime query: this is a one-time startup introspection call, not a
    // user-data query. Using sqlx::query() here is intentional and
    // correct — we cannot use sqlx::query_scalar! because the .sqlx/ cache
    // is built in SQLX_OFFLINE mode and this query should not appear in the
    // cache (it is never called after startup).
    #[allow(clippy::disallowed_methods)]
    let (backend_pid, version_str): (i32, String) =
        sqlx::query_as("SELECT pg_backend_pid(), version()")
            .fetch_one(pool)
            .await
            .map_err(|e| {
                ForgeError::internal(format!(
                    "PgBouncer detection query failed: {}. \
                     Forge requires a direct PostgreSQL connection.",
                    e
                ))
            })?;
    let version_lower = version_str.to_lowercase();

    if backend_pid == 0 || version_lower.contains("pgbouncer") {
        return Err(ForgeError::config(
            "Forge detected a PgBouncer proxy in the connection path. \
             Forge requires direct PostgreSQL connections because it relies on \
             `pg_try_advisory_lock` (for leader election) and persistent `LISTEN/NOTIFY` \
             listeners (for real-time reactivity). Both break under PgBouncer's transaction \
             pooling mode. Connect directly to PostgreSQL, or use a session-level pooler \
             that preserves connection identity (e.g. pgcat in query-router mode).",
        ));
    }

    tracing::debug!(
        backend_pid,
        "Direct PostgreSQL connection confirmed (no PgBouncer)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_config_clone() {
        let config = DatabaseConfig::new("postgres://localhost/test");

        let cloned = config.clone();
        assert_eq!(cloned.url(), config.url());
        assert_eq!(cloned.pool_size, config.pool_size);
    }
}

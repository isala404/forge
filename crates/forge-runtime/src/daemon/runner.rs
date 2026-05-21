//! Daemon runner with restart logic and leader election.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::CircuitBreakerClient;
use forge_core::Result;
use forge_core::daemon::{DaemonContext, DaemonStatus};
use forge_core::function::{JobDispatch, KvHandle, WorkflowDispatch};
use futures_util::FutureExt;
use sqlx::PgPool;
use tokio::sync::{broadcast, watch};
use tracing::{Instrument, Span, field};
use uuid::Uuid;

use super::registry::DaemonRegistry;
use crate::pg::{LeaderConfig, LeaderElection};

/// Configuration for the daemon runner.
#[derive(Debug, Clone)]
pub struct DaemonRunnerConfig {
    /// How often to check daemon health.
    pub health_check_interval: Duration,
    /// How often to send heartbeats.
    pub heartbeat_interval: Duration,
}

impl Default for DaemonRunnerConfig {
    fn default() -> Self {
        Self {
            health_check_interval: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(10),
        }
    }
}

/// Runs all registered daemons as long-lived tasks with restart and leader-election support.
pub struct DaemonRunner {
    registry: Arc<DaemonRegistry>,
    pool: PgPool,
    http_client: CircuitBreakerClient,
    node_id: Uuid,
    config: DaemonRunnerConfig,
    shutdown_rx: broadcast::Receiver<()>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    kv: Option<Arc<dyn KvHandle>>,
}

impl DaemonRunner {
    pub fn new(
        registry: Arc<DaemonRegistry>,
        pool: PgPool,
        http_client: CircuitBreakerClient,
        node_id: Uuid,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Self {
        Self {
            registry,
            pool,
            http_client,
            node_id,
            config: DaemonRunnerConfig::default(),
            shutdown_rx,
            job_dispatch: None,
            workflow_dispatch: None,
            kv: None,
        }
    }

    pub fn with_job_dispatch(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        self.job_dispatch = Some(dispatcher);
        self
    }

    pub fn with_workflow_dispatch(mut self, dispatcher: Arc<dyn WorkflowDispatch>) -> Self {
        self.workflow_dispatch = Some(dispatcher);
        self
    }

    pub fn with_kv(mut self, kv: Arc<dyn KvHandle>) -> Self {
        self.kv = Some(kv);
        self
    }

    pub fn with_config(mut self, config: DaemonRunnerConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn run(mut self) -> Result<()> {
        let runner_span = tracing::info_span!(
            "daemon.runner",
            daemon.node_id = %self.node_id,
            daemon.count = self.registry.len(),
            daemon.uptime_ms = field::Empty,
        );
        let start_time = Instant::now();

        async {
            if self.registry.is_empty() {
                tracing::debug!("No daemons registered, daemon runner idle");
                // Wait for shutdown
                let _ = self.shutdown_rx.recv().await;
                Span::current().record("daemon.uptime_ms", start_time.elapsed().as_millis() as u64);
                return Ok(());
            }

            tracing::info!(count = self.registry.len(), "Daemon runner starting");

            let mut daemon_handles: HashMap<String, DaemonHandle> = HashMap::new();

            for (name, entry) in self.registry.daemons() {
                let info = &entry.info;

                let (shutdown_tx, shutdown_rx) = watch::channel(false);

                let handle = DaemonHandle {
                    name: name.to_string(),
                    instance_id: Uuid::new_v4(),
                    shutdown_tx,
                    restarts: 0,
                    status: DaemonStatus::Pending,
                };

                if let Err(e) = self.record_daemon_start(&handle).await {
                    tracing::debug!(daemon = name, error = %e, "Failed to record daemon start");
                }

                tracing::info!(
                    daemon.name = name,
                    daemon.instance_id = %handle.instance_id,
                    daemon.leader_elected = info.leader_elected,
                    "Starting daemon"
                );

                let daemon_entry = entry.clone();
                let pool = self.pool.clone();
                let http_client = self.http_client.clone();
                let daemon_name = name.to_string();
                let startup_delay = info.startup_delay;
                let restart_on_panic = info.restart_on_panic;
                let restart_delay = info.restart_delay;
                let max_restarts = info.max_restarts;
                let leader_elected = info.leader_elected;
                let node_id = self.node_id;
                let job_dispatch = self.job_dispatch.clone();
                let workflow_dispatch = self.workflow_dispatch.clone();
                let kv = self.kv.clone();

                let election = if leader_elected {
                    Some(Arc::new(LeaderElection::new(
                        pool.clone(),
                        forge_core::cluster::NodeId::from_uuid(node_id),
                        forge_core::cluster::LeaderRole::Daemon(name.to_string()),
                        LeaderConfig::default(),
                    )))
                } else {
                    None
                };

                tokio::spawn(async move {
                    run_daemon_loop(
                        daemon_name,
                        daemon_entry,
                        pool,
                        http_client,
                        shutdown_rx,
                        node_id,
                        startup_delay,
                        restart_on_panic,
                        restart_delay,
                        max_restarts,
                        election,
                        job_dispatch,
                        workflow_dispatch,
                        kv,
                    )
                    .await
                });

                daemon_handles.insert(name.to_string(), handle);
            }

            let _ = self.shutdown_rx.recv().await;
            tracing::info!("Daemon runner received shutdown signal");

            for (name, handle) in &daemon_handles {
                tracing::info!(daemon.name = name, "Signaling daemon to stop");
                let _ = handle.shutdown_tx.send(true);
            }

            tokio::time::sleep(Duration::from_secs(2)).await;

            for (name, handle) in &daemon_handles {
                if let Err(e) = self.record_daemon_stop(handle).await {
                    tracing::debug!(daemon = name, error = %e, "Failed to record daemon stop");
                }
            }

            Span::current().record("daemon.uptime_ms", start_time.elapsed().as_millis() as u64);
            tracing::info!(
                daemon.uptime_ms = start_time.elapsed().as_millis() as u64,
                "Daemon runner stopped"
            );
            Ok(())
        }
        .instrument(runner_span)
        .await
    }

    async fn record_daemon_start(&self, handle: &DaemonHandle) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO forge_daemons (name, node_id, instance_id, status, restarts, started_at, last_heartbeat)
            VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
            ON CONFLICT (name) DO UPDATE SET
                node_id = EXCLUDED.node_id,
                instance_id = EXCLUDED.instance_id,
                status = EXCLUDED.status,
                restarts = EXCLUDED.restarts,
                started_at = NOW(),
                last_heartbeat = NOW(),
                last_error = NULL
            "#,
            &handle.name,
            self.node_id,
            handle.instance_id,
            handle.status.as_str(),
            handle.restarts as i32,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    async fn record_daemon_stop(&self, handle: &DaemonHandle) -> Result<()> {
        sqlx::query!(
            r#"
            UPDATE forge_daemons
            SET status = 'stopped', last_heartbeat = NOW()
            WHERE name = $1 AND instance_id = $2
            "#,
            &handle.name,
            handle.instance_id,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }
}

struct DaemonHandle {
    name: String,
    instance_id: Uuid,
    shutdown_tx: watch::Sender<bool>,
    restarts: u32,
    status: DaemonStatus,
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon_loop(
    name: String,
    entry: Arc<super::registry::DaemonEntry>,
    pool: PgPool,
    http_client: CircuitBreakerClient,
    mut shutdown_rx: watch::Receiver<bool>,
    node_id: Uuid,
    startup_delay: Duration,
    restart_on_panic: bool,
    restart_delay: Duration,
    max_restarts: Option<u32>,
    election: Option<Arc<LeaderElection>>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    kv: Option<Arc<dyn KvHandle>>,
) {
    let leader_elected = election.is_some();
    let daemon_span = tracing::info_span!(
        "daemon.lifecycle",
        daemon.name = %name,
        daemon.node_id = %node_id,
        daemon.leader_elected = leader_elected,
        daemon.restart_count = field::Empty,
        daemon.uptime_ms = field::Empty,
        daemon.final_status = field::Empty,
        otel.name = %format!("daemon {}", name),
    );
    let daemon_start = Instant::now();

    async {
        let mut restarts = 0u32;

        if !startup_delay.is_zero() {
            tracing::debug!(delay_ms = startup_delay.as_millis() as u64, "Waiting startup delay");
            tokio::select! {
                _ = tokio::time::sleep(startup_delay) => {}
                _ = shutdown_rx.changed() => {
                    tracing::debug!("Shutdown during startup delay");
                    Span::current().record("daemon.final_status", "shutdown_during_startup");
                    return;
                }
            }
        }

        loop {
            if *shutdown_rx.borrow() {
                tracing::debug!("Daemon shutting down");
                Span::current().record("daemon.final_status", "shutdown");
                break;
            }

            // Acquire leadership via canonical LeaderElection (holds
            // advisory lock on a dedicated connection for the daemon's
            // lifetime, unlike the old pool-based acquire that released
            // the lock when the connection returned to the pool).
            if let Some(ref election) = election {
                match election.try_become_leader().await {
                    Ok(true) => {
                        tracing::info!("Acquired leadership");
                    }
                    Ok(false) => {
                        tracing::debug!("Waiting for leadership");
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                            _ = shutdown_rx.changed() => {
                                tracing::debug!("Shutdown while waiting for leadership");
                                Span::current().record("daemon.final_status", "shutdown_waiting_leadership");
                                return;
                            }
                        }
                        continue;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "Failed to check leadership");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                }
            }

            if let Err(e) = update_daemon_status(&pool, &name, DaemonStatus::Running).await {
                tracing::debug!(error = %e, "Failed to update daemon status");
            }

            let instance_id = Uuid::new_v4();
            let execution_start = Instant::now();

            let exec_span = tracing::info_span!(
                "daemon.execute",
                daemon.instance_id = %instance_id,
                daemon.execution_duration_ms = field::Empty,
                daemon.status = field::Empty,
            );

            // Hold handles for every background task this iteration spawns so
            // we can abort them when the iteration ends (clean handler return,
            // panic, error, max-restart, etc.). Without this the validate/refresh
            // loops below outlive the iteration and pile up on every restart.
            let mut iteration_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

            let result = async {
                tracing::info!(instance_id = %instance_id, "Daemon instance starting");

                let (daemon_shutdown_tx, daemon_shutdown_rx) = watch::channel(false);

                let shutdown_rx_clone = shutdown_rx.clone();
                let shutdown_tx_clone = daemon_shutdown_tx.clone();
                iteration_handles.push(tokio::spawn(async move {
                    let mut rx = shutdown_rx_clone;
                    while rx.changed().await.is_ok() {
                        if *rx.borrow() {
                            let _ = shutdown_tx_clone.send(true);
                            break;
                        }
                    }
                }));

                // For leader-elected daemons, two background tasks run for
                // the lifetime of this daemon iteration:
                //
                // 1. Lock validator (lock_validate_interval, default 1s):
                //    confirms the advisory lock is still held on the
                //    lock-owning connection. Catches out-of-band loss (PG
                //    backend termination, connection recycling) well within
                //    the lease window.
                //
                // 2. Lease refresher (check_interval, default 5s):
                //    calls refresh_lease() to extend forge_leaders.lease_until
                //    and updates forge_daemons.last_heartbeat. Without this
                //    the 60 s lease expires while the daemon runs, standbys
                //    see a zombie, and failover is delayed by up to the full
                //    lease_duration (the [03.7] issue).
                //
                // Both tasks signal daemon_shutdown_tx on failure so the
                // outer loop re-acquires leadership before the next iteration.
                if let Some(ref election) = election {
                    let election_clone = Arc::clone(election);
                    let shutdown_tx_clone = daemon_shutdown_tx.clone();
                    let validate_interval = election_clone.lock_validate_interval();
                    iteration_handles.push(tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(validate_interval).await;
                            if *shutdown_tx_clone.borrow() {
                                break;
                            }
                            if let Err(e) = election_clone.validate_lock_held().await {
                                tracing::warn!(
                                    error = %e,
                                    "Lost leadership during daemon execution; stopping iteration"
                                );
                                let _ = shutdown_tx_clone.send(true);
                                break;
                            }
                            // validate_lock_held dropped leadership locally on error above;
                            // also check the flag in case it was dropped without an error path.
                            if !election_clone.is_leader() {
                                tracing::warn!(
                                    "Leadership flag cleared during daemon execution; stopping iteration"
                                );
                                let _ = shutdown_tx_clone.send(true);
                                break;
                            }
                        }
                    }));

                    let election_clone = Arc::clone(election);
                    let shutdown_tx_clone = daemon_shutdown_tx.clone();
                    let pool_clone = pool.clone();
                    let name_clone = name.clone();
                    let refresh_interval = election_clone.check_interval();
                    iteration_handles.push(tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(refresh_interval).await;
                            if *shutdown_tx_clone.borrow() {
                                break;
                            }
                            if let Err(e) = election_clone.refresh_lease().await {
                                tracing::warn!(
                                    error = %e,
                                    "Lease refresh failed during daemon execution; stopping iteration"
                                );
                                let _ = shutdown_tx_clone.send(true);
                                break;
                            }
                            // Update forge_daemons.last_heartbeat so other
                            // nodes can see this daemon is alive. Failure is
                            // non-fatal: we log and continue; the lease
                            // itself is the authoritative liveness signal.
                            if let Err(e) =
                                update_daemon_heartbeat(&pool_clone, &name_clone).await
                            {
                                tracing::debug!(
                                    error = %e,
                                    "Failed to update daemon heartbeat"
                                );
                            }
                        }
                    }));
                }

                let mut ctx = DaemonContext::new(
                    name.clone(),
                    instance_id,
                    pool.clone(),
                    http_client.clone(),
                    daemon_shutdown_rx,
                );
                ctx.set_http_timeout(entry.info.http_timeout);
                if let Some(ref jd) = job_dispatch {
                    ctx = ctx.with_job_dispatch(jd.clone());
                }
                if let Some(ref wd) = workflow_dispatch {
                    ctx = ctx.with_workflow_dispatch(wd.clone());
                }
                if let Some(ref kv) = kv {
                    ctx = ctx.with_kv(Arc::clone(kv));
                }

                let result = std::panic::AssertUnwindSafe((entry.handler)(&ctx))
                    .catch_unwind()
                    .await;

                let exec_duration = execution_start.elapsed().as_millis() as u64;
                Span::current().record("daemon.execution_duration_ms", exec_duration);

                result
            }
            .instrument(exec_span)
            .await;

            // Stop the per-iteration background tasks (shutdown forwarder,
            // lock validator, lease refresher). Without this they keep
            // sleeping forever and accumulate across restarts.
            for handle in iteration_handles.drain(..) {
                handle.abort();
            }

            match result {
                Ok(Ok(())) => {
                    tracing::info!("Daemon completed gracefully");
                    Span::current().record("daemon.final_status", "completed");
                    if let Err(e) = update_daemon_status(&pool, &name, DaemonStatus::Stopped).await {
                        tracing::debug!(daemon = %name, error = %e, "Status update failed");
                    }
                    crate::signals::emit_server_execution(&name, "daemon", 0, true, None);
                    break;
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    let recorded = record_daemon_error(&pool, &name, &err_str).await.is_ok();
                    tracing::error!(error = %e, recorded, "Daemon failed");
                    crate::signals::emit_server_execution(
                        &name,
                        "daemon",
                        0,
                        false,
                        Some(err_str),
                    );
                }
                Err(_) => {
                    let recorded = record_daemon_error(&pool, &name, "Daemon panicked").await.is_ok();
                    tracing::error!(recorded, "Daemon panicked");
                    crate::signals::emit_server_execution(
                        &name,
                        "daemon",
                        0,
                        false,
                        Some("Daemon panicked".to_string()),
                    );
                }
            }

            if *shutdown_rx.borrow() {
                tracing::debug!("Daemon shutting down after failure");
                Span::current().record("daemon.final_status", "shutdown_after_failure");
                break;
            }

            if !restart_on_panic {
                tracing::warn!("Restart disabled, daemon stopping");
                Span::current().record("daemon.final_status", "failed_no_restart");
                if let Err(e) = update_daemon_status(&pool, &name, DaemonStatus::Failed).await {
                    tracing::debug!(daemon = %name, error = %e, "Status update failed");
                }
                break;
            }

            restarts += 1;
            Span::current().record("daemon.restart_count", restarts);

            if let Some(max) = max_restarts
                && restarts >= max
            {
                tracing::error!(restarts, max, "Max restarts exceeded");
                Span::current().record("daemon.final_status", "max_restarts_exceeded");
                if let Err(e) = update_daemon_status(&pool, &name, DaemonStatus::Failed).await {
                    tracing::debug!(daemon = %name, error = %e, "Status update failed");
                }
                break;
            }

            if let Err(e) = update_daemon_status(&pool, &name, DaemonStatus::Restarting).await {
                tracing::debug!(daemon = %name, error = %e, "Status update failed");
            }

            tracing::warn!(
                restarts,
                restart_delay_ms = restart_delay.as_millis() as u64,
                "Restarting daemon"
            );

            tokio::select! {
                _ = tokio::time::sleep(restart_delay) => {}
                _ = shutdown_rx.changed() => {
                    tracing::debug!("Shutdown during restart delay");
                    Span::current().record("daemon.final_status", "shutdown_during_restart");
                    break;
                }
            }
        }

        let uptime = daemon_start.elapsed().as_millis() as u64;
        Span::current().record("daemon.uptime_ms", uptime);
        Span::current().record("daemon.restart_count", restarts);

        if let Some(ref election) = election
            && let Err(e) = election.release_leadership().await
        {
            tracing::debug!(daemon = %name, error = %e, "Failed to release leadership");
        }

        tracing::info!(
            uptime_ms = uptime,
            restart_count = restarts,
            "Daemon lifecycle ended"
        );
    }
    .instrument(daemon_span)
    .await
}

async fn update_daemon_status(pool: &PgPool, name: &str, status: DaemonStatus) -> Result<()> {
    sqlx::query!(
        "UPDATE forge_daemons SET status = $1, last_heartbeat = NOW() WHERE name = $2",
        status.as_str(),
        name,
    )
    .execute(pool)
    .await
    .map_err(forge_core::ForgeError::Database)?;

    Ok(())
}

async fn update_daemon_heartbeat(pool: &PgPool, name: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE forge_daemons SET last_heartbeat = NOW() WHERE name = $1",
        name,
    )
    .execute(pool)
    .await
    .map_err(forge_core::ForgeError::Database)?;

    Ok(())
}

async fn record_daemon_error(pool: &PgPool, name: &str, error: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE forge_daemons SET status = 'failed', last_error = $1, last_heartbeat = NOW() WHERE name = $2",
        error,
        name,
    )
    .execute(pool)
    .await
    .map_err(forge_core::ForgeError::Database)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_documented_intervals() {
        let config = DaemonRunnerConfig::default();
        assert_eq!(config.health_check_interval, Duration::from_secs(30));
        assert_eq!(config.heartbeat_interval, Duration::from_secs(10));
    }

    #[test]
    fn daemon_status_serializes_to_documented_strings() {
        // The runner persists status via `as_str` so the SQL `status` column
        // stays in sync with the enum. Lock every variant.
        assert_eq!(DaemonStatus::Pending.as_str(), "pending");
        assert_eq!(DaemonStatus::Acquiring.as_str(), "acquiring");
        assert_eq!(DaemonStatus::Running.as_str(), "running");
        assert_eq!(DaemonStatus::Stopped.as_str(), "stopped");
        assert_eq!(DaemonStatus::Failed.as_str(), "failed");
        assert_eq!(DaemonStatus::Restarting.as_str(), "restarting");
    }
}

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        let db = base
            .isolated(test_name)
            .await
            .expect("Failed to create isolated db");
        let system_sql = crate::pg::migration::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    fn runner(pool: PgPool) -> DaemonRunner {
        let registry = Arc::new(DaemonRegistry::new());
        let http_client = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let (_tx, rx) = broadcast::channel::<()>(1);
        DaemonRunner::new(registry, pool, http_client, Uuid::new_v4(), rx)
    }

    fn handle(name: &str, status: DaemonStatus, restarts: u32) -> DaemonHandle {
        let (shutdown_tx, _) = watch::channel(false);
        DaemonHandle {
            name: name.to_string(),
            instance_id: Uuid::new_v4(),
            shutdown_tx,
            restarts,
            status,
        }
    }

    #[tokio::test]
    async fn record_daemon_start_inserts_new_row() {
        let db = setup_db("daemon_start_insert").await;
        let r = runner(db.pool().clone());
        let h = handle("ingester", DaemonStatus::Running, 0);

        r.record_daemon_start(&h).await.unwrap();

        let (status, restarts): (String, i32) =
            sqlx::query_as("SELECT status, restarts FROM forge_daemons WHERE name = $1")
                .bind("ingester")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(status, "running");
        assert_eq!(restarts, 0);
    }

    #[tokio::test]
    async fn record_daemon_start_upserts_existing_row_and_clears_last_error() {
        let db = setup_db("daemon_start_upsert").await;
        let r = runner(db.pool().clone());

        // First run: failure path leaves last_error populated.
        let h1 = handle("ingester", DaemonStatus::Running, 0);
        r.record_daemon_start(&h1).await.unwrap();
        record_daemon_error(db.pool(), "ingester", "boom")
            .await
            .unwrap();

        // Restart: same name, higher restarts. last_error must be cleared.
        let h2 = handle("ingester", DaemonStatus::Running, 3);
        r.record_daemon_start(&h2).await.unwrap();

        let (status, restarts, last_error): (String, i32, Option<String>) = sqlx::query_as(
            "SELECT status, restarts, last_error FROM forge_daemons WHERE name = $1",
        )
        .bind("ingester")
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(status, "running");
        assert_eq!(restarts, 3);
        assert_eq!(
            last_error, None,
            "restart must clear last_error so operators see only the current failure"
        );
    }

    #[tokio::test]
    async fn record_daemon_stop_only_touches_matching_instance() {
        let db = setup_db("daemon_stop_instance").await;
        let r = runner(db.pool().clone());
        let h = handle("ingester", DaemonStatus::Running, 0);
        r.record_daemon_start(&h).await.unwrap();

        // Wrong instance id — must not flip status to stopped.
        let wrong = DaemonHandle {
            name: h.name.clone(),
            instance_id: Uuid::new_v4(),
            shutdown_tx: watch::channel(false).0,
            restarts: 0,
            status: DaemonStatus::Stopped,
        };
        r.record_daemon_stop(&wrong).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM forge_daemons WHERE name = $1")
            .bind("ingester")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            status, "running",
            "stop for a different instance must not racily stop the live one"
        );

        // Correct instance id — flips to stopped.
        r.record_daemon_stop(&h).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM forge_daemons WHERE name = $1")
            .bind("ingester")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(status, "stopped");
    }

    #[tokio::test]
    async fn update_daemon_status_writes_all_documented_states() {
        let db = setup_db("daemon_status_states").await;
        let r = runner(db.pool().clone());
        r.record_daemon_start(&handle("d", DaemonStatus::Running, 0))
            .await
            .unwrap();

        for s in [
            DaemonStatus::Pending,
            DaemonStatus::Acquiring,
            DaemonStatus::Running,
            DaemonStatus::Stopped,
            DaemonStatus::Failed,
            DaemonStatus::Restarting,
        ] {
            update_daemon_status(db.pool(), "d", s).await.unwrap();
            let got: String =
                sqlx::query_scalar("SELECT status FROM forge_daemons WHERE name = $1")
                    .bind("d")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(got, s.as_str());
        }
    }

    #[tokio::test]
    async fn record_daemon_error_sets_failed_with_message() {
        let db = setup_db("daemon_error_failed").await;
        let r = runner(db.pool().clone());
        r.record_daemon_start(&handle("d", DaemonStatus::Running, 0))
            .await
            .unwrap();

        record_daemon_error(db.pool(), "d", "panic: index out of bounds")
            .await
            .unwrap();

        let (status, msg): (String, Option<String>) =
            sqlx::query_as("SELECT status, last_error FROM forge_daemons WHERE name = $1")
                .bind("d")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(msg.as_deref(), Some("panic: index out of bounds"));
    }
}

//! Daemon runner with restart logic and leader election.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::CircuitBreakerClient;
use forge_core::Result;
use forge_core::daemon::{DaemonContext, DaemonStatus};
use forge_core::function::{JobDispatch, WorkflowDispatch};
use futures_util::FutureExt;
use sqlx::PgPool;
use tokio::sync::{broadcast, watch};
use tracing::{Instrument, Span, field};
use uuid::Uuid;

use super::registry::DaemonRegistry;

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

/// Manages running all registered daemons.
pub struct DaemonRunner {
    registry: Arc<DaemonRegistry>,
    pool: PgPool,
    http_client: CircuitBreakerClient,
    node_id: Uuid,
    config: DaemonRunnerConfig,
    shutdown_rx: broadcast::Receiver<()>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
}

impl DaemonRunner {
    /// Create a new daemon runner.
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
        }
    }

    /// Set job dispatcher for daemon contexts.
    pub fn with_job_dispatch(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        self.job_dispatch = Some(dispatcher);
        self
    }

    /// Set workflow dispatcher for daemon contexts.
    pub fn with_workflow_dispatch(mut self, dispatcher: Arc<dyn WorkflowDispatch>) -> Self {
        self.workflow_dispatch = Some(dispatcher);
        self
    }

    /// Set custom configuration.
    pub fn with_config(mut self, config: DaemonRunnerConfig) -> Self {
        self.config = config;
        self
    }

    /// Run all registered daemons.
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

            // Create individual shutdown channels for each daemon
            let mut daemon_handles: HashMap<String, DaemonHandle> = HashMap::new();

            // Start each daemon
            for (name, entry) in self.registry.daemons() {
                let info = &entry.info;

                // Create shutdown channel for this daemon
                let (shutdown_tx, shutdown_rx) = watch::channel(false);

                let handle = DaemonHandle {
                    name: name.to_string(),
                    instance_id: Uuid::new_v4(),
                    shutdown_tx,
                    restarts: 0,
                    status: DaemonStatus::Pending,
                };

                // Record daemon in database
                if let Err(e) = self.record_daemon_start(&handle).await {
                    tracing::debug!(daemon = name, error = %e, "Failed to record daemon start");
                }

                tracing::info!(
                    daemon.name = name,
                    daemon.instance_id = %handle.instance_id,
                    daemon.leader_elected = info.leader_elected,
                    "Starting daemon"
                );

                // Spawn daemon task
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
                        leader_elected,
                        job_dispatch,
                        workflow_dispatch,
                    )
                    .await
                });

                daemon_handles.insert(name.to_string(), handle);
            }

            // Wait for shutdown signal
            let _ = self.shutdown_rx.recv().await;
            tracing::info!("Daemon runner received shutdown signal");

            // Signal all daemons to stop
            for (name, handle) in &daemon_handles {
                tracing::info!(daemon.name = name, "Signaling daemon to stop");
                let _ = handle.shutdown_tx.send(true);
            }

            // Give daemons time to clean up
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Update daemon status in database
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
    leader_elected: bool,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
) {
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

        // Apply startup delay
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
            // Check shutdown before attempting to run
            if *shutdown_rx.borrow() {
                tracing::debug!("Daemon shutting down");
                Span::current().record("daemon.final_status", "shutdown");
                break;
            }

            // Try to acquire leadership if required
            if leader_elected {
                match try_acquire_leadership(&pool, &name, node_id).await {
                    Ok(true) => {
                        tracing::info!("Acquired leadership");
                    }
                    Ok(false) => {
                        // Another node has leadership, wait and retry
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

            // Update status to running
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

            let result = async {
                tracing::info!(instance_id = %instance_id, "Daemon instance starting");

                // Create context with shutdown receiver
                let (daemon_shutdown_tx, daemon_shutdown_rx) = watch::channel(false);

                // Forward shutdown signal
                let shutdown_rx_clone = shutdown_rx.clone();
                let shutdown_tx_clone = daemon_shutdown_tx.clone();
                tokio::spawn(async move {
                    let mut rx = shutdown_rx_clone;
                    while rx.changed().await.is_ok() {
                        if *rx.borrow() {
                            let _ = shutdown_tx_clone.send(true);
                            break;
                        }
                    }
                });

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

                // Run the daemon
                let result = std::panic::AssertUnwindSafe((entry.handler)(&ctx))
                    .catch_unwind()
                    .await;

                let exec_duration = execution_start.elapsed().as_millis() as u64;
                Span::current().record("daemon.execution_duration_ms", exec_duration);

                result
            }
            .instrument(exec_span)
            .await;

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

            // Check shutdown again
            if *shutdown_rx.borrow() {
                tracing::debug!("Daemon shutting down after failure");
                Span::current().record("daemon.final_status", "shutdown_after_failure");
                break;
            }

            // Check restart policy
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

            // Check max restarts
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

            // Update status to restarting
            if let Err(e) = update_daemon_status(&pool, &name, DaemonStatus::Restarting).await {
                tracing::debug!(daemon = %name, error = %e, "Status update failed");
            }

            tracing::warn!(
                restarts,
                restart_delay_ms = restart_delay.as_millis() as u64,
                "Restarting daemon"
            );

            // Wait before restart
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

        // Release leadership if we held it
        if leader_elected
            && let Err(e) = release_leadership(&pool, &name, node_id).await
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

async fn try_acquire_leadership(pool: &PgPool, daemon_name: &str, node_id: Uuid) -> Result<bool> {
    // Use advisory lock for leader election
    // Hash the daemon name to get a consistent lock ID
    let lock_id = daemon_name
        .bytes()
        .fold(0i64, |acc, b| acc.wrapping_add(b as i64).wrapping_mul(31));

    let result = sqlx::query_scalar!(r#"SELECT pg_try_advisory_lock($1) as "acquired!""#, lock_id)
        .fetch_one(pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

    if result {
        // Update daemon record with our node_id
        sqlx::query!(
            "UPDATE forge_daemons SET node_id = $1 WHERE name = $2",
            node_id,
            daemon_name
        )
        .execute(pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;
    }

    Ok(result)
}

async fn release_leadership(pool: &PgPool, daemon_name: &str, _node_id: Uuid) -> Result<()> {
    let lock_id = daemon_name
        .bytes()
        .fold(0i64, |acc, b| acc.wrapping_add(b as i64).wrapping_mul(31));

    sqlx::query_scalar!("SELECT pg_advisory_unlock($1)", lock_id)
        .fetch_one(pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

    Ok(())
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
    fn test_default_config() {
        let config = DaemonRunnerConfig::default();
        assert_eq!(config.health_check_interval, Duration::from_secs(30));
        assert_eq!(config.heartbeat_interval, Duration::from_secs(10));
    }
}

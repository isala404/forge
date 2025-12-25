use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forge_core::cluster::NodeId;
use tokio::sync::watch;

/// Heartbeat loop configuration.
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// Interval between heartbeats.
    pub interval: Duration,
    /// Threshold for marking nodes as dead.
    pub dead_threshold: Duration,
    /// Whether to mark dead nodes.
    pub mark_dead_nodes: bool,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            dead_threshold: Duration::from_secs(15),
            mark_dead_nodes: true,
        }
    }
}

/// Heartbeat loop for cluster health.
pub struct HeartbeatLoop {
    pool: sqlx::PgPool,
    node_id: NodeId,
    config: HeartbeatConfig,
    running: Arc<AtomicBool>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl HeartbeatLoop {
    /// Create a new heartbeat loop.
    pub fn new(pool: sqlx::PgPool, node_id: NodeId, config: HeartbeatConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            pool,
            node_id,
            config,
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Check if the loop is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Get a shutdown receiver.
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Stop the heartbeat loop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        self.running.store(false, Ordering::SeqCst);
    }

    /// Run the heartbeat loop.
    pub async fn run(&self) {
        self.running.store(true, Ordering::SeqCst);
        let mut shutdown_rx = self.shutdown_rx.clone();

        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.interval) => {
                    // Update our heartbeat
                    if let Err(e) = self.send_heartbeat().await {
                        tracing::warn!("Failed to send heartbeat: {}", e);
                    }

                    // Mark dead nodes if enabled
                    if self.config.mark_dead_nodes {
                        if let Err(e) = self.mark_dead_nodes().await {
                            tracing::warn!("Failed to mark dead nodes: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Heartbeat loop shutting down");
                        break;
                    }
                }
            }
        }

        self.running.store(false, Ordering::SeqCst);
    }

    /// Send a heartbeat update.
    async fn send_heartbeat(&self) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_nodes
            SET last_heartbeat = NOW()
            WHERE id = $1
            "#,
        )
        .bind(self.node_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Mark stale nodes as dead.
    async fn mark_dead_nodes(&self) -> forge_core::Result<u64> {
        let threshold_secs = self.config.dead_threshold.as_secs() as f64;

        let result = sqlx::query(
            r#"
            UPDATE forge_nodes
            SET status = 'dead'
            WHERE status = 'active'
              AND last_heartbeat < NOW() - make_interval(secs => $1)
            "#,
        )
        .bind(threshold_secs)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let count = result.rows_affected();
        if count > 0 {
            tracing::info!("Marked {} nodes as dead", count);
        }

        Ok(count)
    }

    /// Update load metrics.
    pub async fn update_load(
        &self,
        current_connections: u32,
        current_jobs: u32,
        cpu_usage: f32,
        memory_usage: f32,
    ) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_nodes
            SET current_connections = $2,
                current_jobs = $3,
                cpu_usage = $4,
                memory_usage = $5,
                last_heartbeat = NOW()
            WHERE id = $1
            "#,
        )
        .bind(self.node_id.as_uuid())
        .bind(current_connections as i32)
        .bind(current_jobs as i32)
        .bind(cpu_usage)
        .bind(memory_usage)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_config_default() {
        let config = HeartbeatConfig::default();
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.dead_threshold, Duration::from_secs(15));
        assert!(config.mark_dead_nodes);
    }
}

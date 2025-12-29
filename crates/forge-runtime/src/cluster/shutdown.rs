use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forge_core::cluster::NodeStatus;
use tokio::sync::broadcast;

use super::leader::LeaderElection;
use super::registry::NodeRegistry;

/// Graceful shutdown configuration.
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// Timeout for waiting on in-flight requests.
    pub drain_timeout: Duration,
    /// How often to check for completion.
    pub poll_interval: Duration,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            drain_timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(100),
        }
    }
}

/// Graceful shutdown coordinator.
pub struct GracefulShutdown {
    registry: Arc<NodeRegistry>,
    #[allow(dead_code)]
    leader_election: Option<Arc<LeaderElection>>,
    config: ShutdownConfig,
    shutdown_requested: Arc<AtomicBool>,
    in_flight_count: Arc<AtomicU32>,
    shutdown_tx: broadcast::Sender<()>,
}

impl GracefulShutdown {
    /// Create a new graceful shutdown coordinator.
    pub fn new(
        registry: Arc<NodeRegistry>,
        leader_election: Option<Arc<LeaderElection>>,
        config: ShutdownConfig,
    ) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            registry,
            leader_election,
            config,
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            in_flight_count: Arc::new(AtomicU32::new(0)),
            shutdown_tx,
        }
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Get the current in-flight count.
    pub fn in_flight_count(&self) -> u32 {
        self.in_flight_count.load(Ordering::SeqCst)
    }

    /// Increment the in-flight counter.
    pub fn increment_in_flight(&self) {
        self.in_flight_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Decrement the in-flight counter.
    pub fn decrement_in_flight(&self) {
        self.in_flight_count.fetch_sub(1, Ordering::SeqCst);
    }

    /// Subscribe to shutdown notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Check if new work should be accepted.
    pub fn should_accept_work(&self) -> bool {
        !self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Perform graceful shutdown.
    pub async fn shutdown(&self) -> forge_core::Result<()> {
        // Mark shutdown as requested
        self.shutdown_requested.store(true, Ordering::SeqCst);

        // Notify all listeners
        let _ = self.shutdown_tx.send(());

        tracing::info!("Starting graceful shutdown");

        // 1. Set status to draining
        if let Err(e) = self.registry.set_status(NodeStatus::Draining).await {
            tracing::warn!("Failed to set draining status: {}", e);
        }

        // 2. Wait for in-flight requests with timeout
        let drain_result = self.wait_for_drain().await;
        match drain_result {
            DrainResult::Completed => {
                tracing::info!("All in-flight requests completed");
            }
            DrainResult::Timeout(remaining) => {
                tracing::warn!(
                    "Drain timeout reached with {} requests still in-flight",
                    remaining
                );
            }
        }

        // 3. Leader lock release is handled by LeaderElection::run() on shutdown signal

        // 4. Deregister from cluster
        if let Err(e) = self.registry.deregister().await {
            tracing::warn!("Failed to deregister from cluster: {}", e);
        }

        tracing::info!("Graceful shutdown complete");
        Ok(())
    }

    /// Wait for all in-flight requests to complete.
    async fn wait_for_drain(&self) -> DrainResult {
        let deadline = tokio::time::Instant::now() + self.config.drain_timeout;

        loop {
            let count = self.in_flight_count.load(Ordering::SeqCst);

            if count == 0 {
                return DrainResult::Completed;
            }

            if tokio::time::Instant::now() >= deadline {
                return DrainResult::Timeout(count);
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }
    }
}

/// Result of drain operation.
#[derive(Debug)]
enum DrainResult {
    /// All requests completed.
    Completed,
    /// Timeout reached with remaining requests.
    Timeout(u32),
}

/// RAII guard for tracking in-flight requests.
pub struct InFlightGuard {
    shutdown: Arc<GracefulShutdown>,
}

impl InFlightGuard {
    /// Create a new in-flight guard.
    /// Returns None if shutdown is in progress.
    pub fn try_new(shutdown: Arc<GracefulShutdown>) -> Option<Self> {
        if shutdown.should_accept_work() {
            shutdown.increment_in_flight();
            Some(Self { shutdown })
        } else {
            None
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.shutdown.decrement_in_flight();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_config_default() {
        let config = ShutdownConfig::default();
        assert_eq!(config.drain_timeout, Duration::from_secs(30));
        assert_eq!(config.poll_interval, Duration::from_millis(100));
    }
}

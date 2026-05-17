use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use forge_core::cluster::NodeStatus;
use tokio::sync::broadcast;

use super::registry::NodeRegistry;
use crate::pg::LeaderElection;

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

        // 3. Release leadership explicitly so another node can take over immediately
        if let Some(ref election) = self.leader_election {
            if let Err(e) = election.release_leadership().await {
                tracing::warn!("Failed to release leadership: {}", e);
            } else {
                tracing::debug!("Leadership released");
            }
        }

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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use forge_core::cluster::{NodeInfo, NodeRole};
    use sqlx::postgres::PgPoolOptions;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_shutdown() -> Arc<GracefulShutdown> {
        // `connect_lazy` never opens the socket, so we can build a NodeRegistry
        // without a live Postgres. None of the methods exercised below touch
        // the pool — they only read/write atomics and the broadcast channel.
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://localhost:1/never")
            .unwrap();
        let node = NodeInfo::new_local(
            "test-host".to_string(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            9081,
            9082,
            vec![NodeRole::Gateway],
            vec!["default".to_string()],
            "test".to_string(),
        );
        let registry = Arc::new(NodeRegistry::new(pool, node));
        Arc::new(GracefulShutdown::new(
            registry,
            None,
            ShutdownConfig::default(),
        ))
    }

    #[test]
    fn test_shutdown_config_default() {
        let config = ShutdownConfig::default();
        assert_eq!(config.drain_timeout, Duration::from_secs(30));
        assert_eq!(config.poll_interval, Duration::from_millis(100));
    }

    #[tokio::test]
    async fn fresh_shutdown_accepts_work_and_has_zero_in_flight() {
        let sd = make_shutdown();
        assert!(!sd.is_shutdown_requested());
        assert!(sd.should_accept_work());
        assert_eq!(sd.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn in_flight_counter_increments_and_decrements() {
        let sd = make_shutdown();
        sd.increment_in_flight();
        sd.increment_in_flight();
        assert_eq!(sd.in_flight_count(), 2);
        sd.decrement_in_flight();
        assert_eq!(sd.in_flight_count(), 1);
        sd.decrement_in_flight();
        assert_eq!(sd.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn in_flight_guard_tracks_counter_via_raii() {
        let sd = make_shutdown();
        {
            let _g1 = InFlightGuard::try_new(sd.clone()).expect("should admit work");
            let _g2 = InFlightGuard::try_new(sd.clone()).expect("should admit work");
            assert_eq!(sd.in_flight_count(), 2);
        }
        // Both guards dropped — counter back to zero.
        assert_eq!(sd.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn in_flight_guard_refuses_work_after_shutdown_flag_set() {
        let sd = make_shutdown();
        // Flip the flag directly — emulates state after `shutdown()` ran past
        // step 1 without needing the registry/DB calls.
        sd.shutdown_requested.store(true, Ordering::SeqCst);
        assert!(!sd.should_accept_work());
        assert!(InFlightGuard::try_new(sd.clone()).is_none());
        // Counter must not have been incremented by the refused attempt.
        assert_eq!(sd.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn subscribe_returns_independent_receivers() {
        let sd = make_shutdown();
        let mut r1 = sd.subscribe();
        let mut r2 = sd.subscribe();
        // Both should receive the same broadcast.
        sd.shutdown_tx.send(()).unwrap();
        assert!(r1.recv().await.is_ok());
        assert!(r2.recv().await.is_ok());
    }

    #[test]
    fn shutdown_config_clone_preserves_custom_values() {
        let original = ShutdownConfig {
            drain_timeout: Duration::from_millis(250),
            poll_interval: Duration::from_millis(5),
        };
        let cloned = original.clone();
        assert_eq!(cloned.drain_timeout, Duration::from_millis(250));
        assert_eq!(cloned.poll_interval, Duration::from_millis(5));
    }

    #[tokio::test]
    async fn late_subscribers_miss_messages_sent_before_subscription() {
        // Broadcast channel only delivers messages sent AFTER a receiver
        // subscribes — handlers that subscribe inside a request handler may
        // miss the shutdown signal if they raced the shutdown call.
        let sd = make_shutdown();
        // Hold an early receiver so send() has a subscriber and succeeds; the
        // late receiver still must not see this historical broadcast.
        let _early = sd.subscribe();
        sd.shutdown_tx.send(()).unwrap();

        let mut late = sd.subscribe();
        let recv_result =
            tokio::time::timeout(Duration::from_millis(20), late.recv()).await;
        assert!(
            recv_result.is_err(),
            "late subscriber should not receive historical broadcast: {recv_result:?}"
        );
    }

    #[tokio::test]
    async fn guard_admitted_before_shutdown_still_decrements_after_flag_set() {
        // Models a request that began serving before shutdown was requested;
        // when it finishes, the counter must come back to zero so the drain
        // loop can exit.
        let sd = make_shutdown();
        let guard = InFlightGuard::try_new(sd.clone()).expect("admit");
        assert_eq!(sd.in_flight_count(), 1);

        sd.shutdown_requested.store(true, Ordering::SeqCst);
        assert!(!sd.should_accept_work(), "no new work after flag set");

        drop(guard);
        assert_eq!(sd.in_flight_count(), 0, "RAII drop must decrement even mid-shutdown");
    }

    #[tokio::test]
    async fn concurrent_increments_and_decrements_keep_counter_consistent() {
        // Hammer the atomic from multiple tasks; the final balance should be
        // zero. Tests the SeqCst orderings on the counter under contention.
        let sd = make_shutdown();
        let mut handles = Vec::new();
        for _ in 0..16 {
            let s = sd.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..50 {
                    s.increment_in_flight();
                    s.decrement_in_flight();
                }
            }));
        }
        for h in handles {
            h.await.expect("task did not panic");
        }
        assert_eq!(sd.in_flight_count(), 0);
    }
}

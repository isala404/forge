use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use forge_core::cluster::NodeId;
use forge_core::config::cluster::ClusterConfig;
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
    /// Maximum heartbeat interval when cluster is stable.
    pub max_interval: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            dead_threshold: Duration::from_secs(15),
            mark_dead_nodes: true,
            max_interval: Duration::from_secs(60),
        }
    }
}

impl HeartbeatConfig {
    /// Create a HeartbeatConfig from the user-facing ClusterConfig.
    pub fn from_cluster_config(cluster: &ClusterConfig) -> Self {
        use forge_core::config::cluster::DiscoveryMethod;

        match cluster.discovery {
            DiscoveryMethod::Postgres => {
                tracing::debug!("Using PostgreSQL-based cluster discovery");
            }
            DiscoveryMethod::Dns => {
                tracing::info!(
                    dns_name = ?cluster.dns_name,
                    "Using DNS-based cluster discovery"
                );
            }
            DiscoveryMethod::Kubernetes => {
                tracing::info!(
                    dns_name = ?cluster.dns_name,
                    "Using Kubernetes-based cluster discovery (via headless service DNS)"
                );
            }
            DiscoveryMethod::Static => {
                tracing::info!(
                    seed_count = cluster.seed_nodes.len(),
                    "Using static seed node discovery"
                );
            }
        }

        Self {
            interval: *cluster.heartbeat_interval,
            dead_threshold: *cluster.dead_threshold,
            mark_dead_nodes: true,
            max_interval: Duration::from_secs(cluster.heartbeat_interval.as_secs() * 12),
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
    current_interval_ms: AtomicU64,
    stable_count: AtomicU32,
    last_active_count: AtomicU32,
}

impl HeartbeatLoop {
    /// Create a new heartbeat loop.
    pub fn new(pool: sqlx::PgPool, node_id: NodeId, config: HeartbeatConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let interval_ms = config.interval.as_millis() as u64;
        Self {
            pool,
            node_id,
            config,
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            shutdown_rx,
            current_interval_ms: AtomicU64::new(interval_ms),
            stable_count: AtomicU32::new(0),
            last_active_count: AtomicU32::new(0),
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
            let interval = self.current_interval();
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    // Update our heartbeat
                    let hb_start = std::time::Instant::now();
                    if let Err(e) = self.send_heartbeat().await {
                        tracing::debug!(error = %e, "Failed to send heartbeat");
                    }
                    super::metrics::record_heartbeat_latency(hb_start.elapsed().as_secs_f64());

                    // Adjust interval based on cluster stability
                    self.adjust_interval().await;

                    // Mark dead nodes if enabled
                    if self.config.mark_dead_nodes
                        && let Err(e) = self.mark_dead_nodes().await
                    {
                        tracing::debug!(error = %e, "Failed to mark dead nodes");
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::debug!("Heartbeat loop shutting down");
                        break;
                    }
                }
            }
        }

        self.running.store(false, Ordering::SeqCst);
    }

    /// Current adaptive interval.
    fn current_interval(&self) -> Duration {
        Duration::from_millis(self.current_interval_ms.load(Ordering::Relaxed))
    }

    /// Query the number of active nodes in the cluster.
    async fn active_node_count(&self) -> forge_core::Result<u32> {
        let row = sqlx::query_scalar!("SELECT COUNT(*) FROM forge_nodes WHERE status = 'active'")
            .fetch_one(&self.pool)
            .await
            .map_err(forge_core::ForgeError::Database)?;

        Ok(row.unwrap_or(0) as u32)
    }

    /// Adjust heartbeat interval based on cluster stability.
    async fn adjust_interval(&self) {
        let count = match self.active_node_count().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to query active node count");
                return;
            }
        };

        super::metrics::set_node_counts(count as i64, 0);

        let last = self.last_active_count.load(Ordering::Relaxed);
        let stable = self.stable_count.load(Ordering::Relaxed);
        let cur = self.current_interval_ms.load(Ordering::Relaxed);
        let base_ms = self.config.interval.as_millis() as u64;
        let max_ms = self.config.max_interval.as_millis() as u64;

        let (next_ms, next_stable) =
            next_adaptive_interval(count, last, stable, cur, base_ms, max_ms);
        self.current_interval_ms.store(next_ms, Ordering::Relaxed);
        self.stable_count.store(next_stable, Ordering::Relaxed);
        self.last_active_count.store(count, Ordering::Relaxed);
    }

    /// Send a heartbeat update.
    async fn send_heartbeat(&self) -> forge_core::Result<()> {
        sqlx::query!(
            r#"
            UPDATE forge_nodes
            SET last_heartbeat = NOW()
            WHERE id = $1
            "#,
            self.node_id.as_uuid(),
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    /// Mark stale nodes as dead. Threshold is the greater of the configured
    /// dead_threshold and 3x the current adaptive interval.
    async fn mark_dead_nodes(&self) -> forge_core::Result<u64> {
        let threshold_secs =
            dead_node_threshold(self.current_interval(), self.config.dead_threshold).as_secs_f64();

        let result = sqlx::query!(
            r#"
            UPDATE forge_nodes
            SET status = 'dead'
            WHERE status = 'active'
              AND last_heartbeat < NOW() - make_interval(secs => $1)
            "#,
            threshold_secs,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let count = result.rows_affected();
        if count > 0 {
            tracing::warn!(count, "Marked nodes as dead");
            // Update dead node count in metrics (we know `count` nodes just became dead)
            super::metrics::set_node_counts(
                self.last_active_count.load(Ordering::Relaxed) as i64,
                count as i64,
            );
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
        sqlx::query!(
            r#"
            UPDATE forge_nodes
            SET current_connections = $2,
                current_jobs = $3,
                cpu_usage = $4,
                memory_usage = $5,
                last_heartbeat = NOW()
            WHERE id = $1
            "#,
            self.node_id.as_uuid(),
            current_connections as i32,
            current_jobs as i32,
            cpu_usage as f64,
            memory_usage as f64,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }
}

/// Adaptive heartbeat policy. Given the current observation and prior state,
/// returns the new `(interval_ms, stable_count)`.
///
/// When membership stays unchanged for at least 3 consecutive observations the
/// interval doubles, clamped into `[base_ms, max_ms]`. Any change — including a
/// first observation, signalled by `last == 0` — resets the stable counter and
/// returns to `base_ms`.
fn next_adaptive_interval(
    observed: u32,
    last: u32,
    stable: u32,
    current_ms: u64,
    base_ms: u64,
    max_ms: u64,
) -> (u64, u32) {
    if last == 0 || observed != last {
        return (base_ms, 0);
    }
    let new_stable = stable.saturating_add(1);
    if new_stable >= 3 {
        let doubled = current_ms.saturating_mul(2).min(max_ms).max(base_ms);
        (doubled, new_stable)
    } else {
        (current_ms, new_stable)
    }
}

/// Compute the dead-node threshold: the greater of 3× the current adaptive
/// interval and the configured `dead_threshold`. Returned as a `Duration` so
/// callers can convert to whatever the SQL layer needs.
fn dead_node_threshold(current_interval: Duration, configured: Duration) -> Duration {
    let adaptive = current_interval.saturating_mul(3);
    adaptive.max(configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_config_default_matches_documented_values() {
        let config = HeartbeatConfig::default();
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.dead_threshold, Duration::from_secs(15));
        assert!(config.mark_dead_nodes);
        assert_eq!(config.max_interval, Duration::from_secs(60));
    }

    #[test]
    fn heartbeat_config_from_cluster_config_propagates_durations() {
        let mut cluster = ClusterConfig::default();
        cluster.heartbeat_interval = forge_core::config::DurationStr::new(Duration::from_secs(10));
        cluster.dead_threshold = forge_core::config::DurationStr::new(Duration::from_secs(30));

        let config = HeartbeatConfig::from_cluster_config(&cluster);
        assert_eq!(config.interval, Duration::from_secs(10));
        assert_eq!(config.dead_threshold, Duration::from_secs(30));
        assert!(config.mark_dead_nodes);
        // max_interval = heartbeat_interval * 12
        assert_eq!(config.max_interval, Duration::from_secs(120));
    }

    // --- next_adaptive_interval: pure policy under controlled inputs ---

    #[test]
    fn adaptive_interval_first_observation_seeds_base() {
        // last == 0 means "no prior observation". Always reset.
        let (next, stable) = next_adaptive_interval(5, 0, 0, 9_999, 5_000, 60_000);
        assert_eq!(next, 5_000);
        assert_eq!(stable, 0);
    }

    #[test]
    fn adaptive_interval_membership_change_resets() {
        let (next, stable) = next_adaptive_interval(7, 5, 9, 40_000, 5_000, 60_000);
        assert_eq!(next, 5_000);
        assert_eq!(stable, 0);
    }

    #[test]
    fn adaptive_interval_stable_under_threshold_holds_current() {
        // stable=0 -> 1, still under 3, interval unchanged.
        let (next, stable) = next_adaptive_interval(5, 5, 0, 5_000, 5_000, 60_000);
        assert_eq!(next, 5_000);
        assert_eq!(stable, 1);

        let (next, stable) = next_adaptive_interval(5, 5, 1, 5_000, 5_000, 60_000);
        assert_eq!(next, 5_000);
        assert_eq!(stable, 2);
    }

    #[test]
    fn adaptive_interval_doubles_at_third_stable_tick() {
        // stable goes 2 -> 3, triggers doubling: 5000 * 2 = 10000.
        let (next, stable) = next_adaptive_interval(5, 5, 2, 5_000, 5_000, 60_000);
        assert_eq!(next, 10_000);
        assert_eq!(stable, 3);
    }

    #[test]
    fn adaptive_interval_doubles_clamps_to_max() {
        // 40_000 * 2 = 80_000 > 60_000 max.
        let (next, stable) = next_adaptive_interval(5, 5, 5, 40_000, 5_000, 60_000);
        assert_eq!(next, 60_000);
        assert_eq!(stable, 6);
    }

    #[test]
    fn adaptive_interval_doubles_floor_to_base() {
        // current_ms below base (would only happen if config changed). Floor.
        let (next, _) = next_adaptive_interval(5, 5, 5, 1_000, 5_000, 60_000);
        assert_eq!(next, 5_000);
    }

    #[test]
    fn adaptive_interval_doubling_saturates_without_overflow() {
        // current_ms near u64::MAX shouldn't panic on the saturating mul.
        let (next, _) = next_adaptive_interval(5, 5, 5, u64::MAX - 1, 5_000, 60_000);
        assert_eq!(next, 60_000);
    }

    // --- dead_node_threshold: max of adaptive and configured ---

    #[test]
    fn dead_threshold_uses_adaptive_when_larger() {
        // adaptive = 30s*3 = 90s; configured = 15s. Pick 90s.
        let got = dead_node_threshold(Duration::from_secs(30), Duration::from_secs(15));
        assert_eq!(got, Duration::from_secs(90));
    }

    #[test]
    fn dead_threshold_uses_configured_when_larger() {
        // adaptive = 5s*3 = 15s; configured = 60s. Pick 60s.
        let got = dead_node_threshold(Duration::from_secs(5), Duration::from_secs(60));
        assert_eq!(got, Duration::from_secs(60));
    }

    #[test]
    fn dead_threshold_saturates_on_huge_interval() {
        // Should not panic on multiplication overflow.
        let got = dead_node_threshold(Duration::MAX, Duration::from_secs(60));
        assert!(got >= Duration::from_secs(60));
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

    async fn seed_node(pool: &sqlx::PgPool, id: NodeId, status: &str, heartbeat_age_secs: i64) {
        sqlx::query(
            r#"
            INSERT INTO forge_nodes (
                id, hostname, ip_address, http_port, grpc_port, status, last_heartbeat
            ) VALUES ($1, $2, $3, $4, $5, $6, NOW() - make_interval(secs => $7))
            "#,
        )
        .bind(id.as_uuid())
        .bind("test-host")
        .bind("127.0.0.1")
        .bind(8080_i32)
        .bind(8081_i32)
        .bind(status)
        .bind(heartbeat_age_secs as f64)
        .execute(pool)
        .await
        .unwrap();
    }

    fn loop_for(pool: sqlx::PgPool, node_id: NodeId) -> HeartbeatLoop {
        HeartbeatLoop::new(
            pool,
            node_id,
            HeartbeatConfig {
                interval: Duration::from_secs(1),
                dead_threshold: Duration::from_secs(10),
                mark_dead_nodes: true,
                max_interval: Duration::from_secs(60),
            },
        )
    }

    #[tokio::test]
    async fn send_heartbeat_bumps_last_heartbeat_to_now() {
        let db = setup_db("hb_send").await;
        let node = NodeId::new();
        seed_node(db.pool(), node, "active", 30).await;

        let hb = loop_for(db.pool().clone(), node);
        hb.send_heartbeat().await.unwrap();

        let age: f64 = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM (NOW() - last_heartbeat))::float8 FROM forge_nodes WHERE id = $1",
        )
        .bind(node.as_uuid())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(age < 2.0, "heartbeat should be fresh, got age = {age}");
    }

    #[tokio::test]
    async fn active_node_count_only_counts_active_status() {
        let db = setup_db("hb_count").await;
        let self_id = NodeId::new();
        seed_node(db.pool(), self_id, "active", 0).await;
        seed_node(db.pool(), NodeId::new(), "active", 0).await;
        seed_node(db.pool(), NodeId::new(), "dead", 0).await;
        seed_node(db.pool(), NodeId::new(), "starting", 0).await;

        let hb = loop_for(db.pool().clone(), self_id);
        let count = hb.active_node_count().await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn mark_dead_nodes_flips_stale_active_nodes() {
        let db = setup_db("hb_mark_dead").await;
        let self_id = NodeId::new();
        let stale = NodeId::new();
        let fresh = NodeId::new();
        seed_node(db.pool(), self_id, "active", 0).await;
        seed_node(db.pool(), stale, "active", 120).await; // far past 30s threshold
        seed_node(db.pool(), fresh, "active", 0).await;

        let hb = loop_for(db.pool().clone(), self_id);
        let marked = hb.mark_dead_nodes().await.unwrap();
        assert_eq!(marked, 1);

        let stale_status: String =
            sqlx::query_scalar("SELECT status FROM forge_nodes WHERE id = $1")
                .bind(stale.as_uuid())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(stale_status, "dead");

        let fresh_status: String =
            sqlx::query_scalar("SELECT status FROM forge_nodes WHERE id = $1")
                .bind(fresh.as_uuid())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(fresh_status, "active");
    }

    #[tokio::test]
    async fn mark_dead_nodes_does_not_revive_already_dead_nodes() {
        let db = setup_db("hb_no_revive").await;
        let self_id = NodeId::new();
        let already_dead = NodeId::new();
        seed_node(db.pool(), self_id, "active", 0).await;
        seed_node(db.pool(), already_dead, "dead", 120).await;

        let hb = loop_for(db.pool().clone(), self_id);
        let marked = hb.mark_dead_nodes().await.unwrap();
        assert_eq!(marked, 0, "dead nodes should not be re-touched");

        let status: String = sqlx::query_scalar("SELECT status FROM forge_nodes WHERE id = $1")
            .bind(already_dead.as_uuid())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(status, "dead");
    }

    #[tokio::test]
    async fn update_load_persists_metrics_and_refreshes_heartbeat() {
        let db = setup_db("hb_update_load").await;
        let node = NodeId::new();
        seed_node(db.pool(), node, "active", 30).await;

        let hb = loop_for(db.pool().clone(), node);
        hb.update_load(42, 7, 0.5, 0.25).await.unwrap();

        let (conns, jobs, cpu, mem, age): (i32, i32, Option<f64>, Option<f64>, f64) =
            sqlx::query_as(
                r#"
                SELECT current_connections, current_jobs, cpu_usage, memory_usage,
                       EXTRACT(EPOCH FROM (NOW() - last_heartbeat))::float8
                FROM forge_nodes WHERE id = $1
                "#,
            )
            .bind(node.as_uuid())
            .fetch_one(db.pool())
            .await
            .unwrap();

        assert_eq!(conns, 42);
        assert_eq!(jobs, 7);
        // f32 -> f64 cast introduces tiny drift; compare loosely.
        assert!((cpu.unwrap() - 0.5).abs() < 1e-5);
        assert!((mem.unwrap() - 0.25).abs() < 1e-5);
        assert!(age < 2.0);
    }
}

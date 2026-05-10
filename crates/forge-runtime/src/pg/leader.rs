use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use forge_core::cluster::{LeaderInfo, LeaderRole, NodeId};
use tokio::sync::{Mutex, watch};

/// Leader election configuration.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    /// How often standbys check leader health and leaders refresh leases.
    pub check_interval: Duration,
    /// Lease duration (leader must refresh before expiry).
    pub lease_duration: Duration,
}

impl Default for LeaderConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            lease_duration: Duration::from_secs(60),
        }
    }
}

/// Leader election using PostgreSQL advisory locks.
///
/// Advisory locks provide a simple, reliable way to elect a leader without
/// external coordination services. Key properties:
///
/// 1. **Mutual exclusion**: Only one session can hold a given lock ID at a time.
/// 2. **Automatic release**: If the connection dies, PostgreSQL releases the lock.
/// 3. **Non-blocking try**: `pg_try_advisory_lock` returns immediately with success/failure.
///
/// Each `LeaderRole` maps to a unique lock ID, allowing multiple independent
/// leader elections (e.g., separate leaders for cron scheduler and workflow timers).
///
/// The `is_leader` flag uses `SeqCst` ordering because:
/// - Multiple threads read this flag to decide whether to execute leader-only code
/// - We need visibility guarantees across threads immediately after acquiring/releasing
/// - The performance cost is negligible (leadership changes are rare)
pub struct LeaderElection {
    pool: sqlx::PgPool,
    node_id: NodeId,
    role: LeaderRole,
    config: LeaderConfig,
    /// Uses SeqCst for cross-thread visibility of leadership state changes.
    is_leader: Arc<AtomicBool>,
    lock_connection: Arc<Mutex<Option<sqlx::pool::PoolConnection<sqlx::Postgres>>>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl LeaderElection {
    /// Create a new leader election instance.
    pub fn new(
        pool: sqlx::PgPool,
        node_id: NodeId,
        role: LeaderRole,
        config: LeaderConfig,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            pool,
            node_id,
            role,
            config,
            is_leader: Arc::new(AtomicBool::new(false)),
            lock_connection: Arc::new(Mutex::new(None)),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Check if this node is the leader.
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::SeqCst)
    }

    /// Get a shutdown receiver.
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Stop the leader election.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Try to acquire leadership.
    pub async fn try_become_leader(&self) -> forge_core::Result<bool> {
        if self.is_leader() {
            return Ok(true);
        }

        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        // Try to acquire advisory lock (non-blocking)
        let acquired = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) as "acquired!""#,
            self.role.lock_id()
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        crate::cluster::metrics::record_leader_election_attempt(self.role.as_str(), acquired);

        if acquired {
            // Record leadership for visibility only. The advisory lock above is
            // the single source of truth; this row exists so operators can see
            // which node owns the role and when its lease expires.
            let lease_until =
                Utc::now() + chrono::Duration::seconds(self.config.lease_duration.as_secs() as i64);

            sqlx::query!(
                r#"
                INSERT INTO forge_leaders (role, node_id, acquired_at, lease_until)
                VALUES ($1, $2, NOW(), $3)
                ON CONFLICT (role) DO UPDATE SET
                    node_id = EXCLUDED.node_id,
                    acquired_at = NOW(),
                    lease_until = EXCLUDED.lease_until
                "#,
                self.role.as_str(),
                self.node_id.as_uuid(),
                lease_until,
            )
            .execute(&self.pool)
            .await
            .map_err(forge_core::ForgeError::Database)?;

            self.is_leader.store(true, Ordering::SeqCst);
            crate::cluster::metrics::set_is_leader(self.role.as_str(), true);
            *self.lock_connection.lock().await = Some(conn);
            tracing::info!(role = self.role.as_str(), "Acquired leadership");
        }

        Ok(acquired)
    }

    /// Refresh the leadership lease.
    pub async fn refresh_lease(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        let lease_until =
            Utc::now() + chrono::Duration::seconds(self.config.lease_duration.as_secs() as i64);

        sqlx::query!(
            r#"
            UPDATE forge_leaders
            SET lease_until = $3
            WHERE role = $1 AND node_id = $2
            "#,
            self.role.as_str(),
            self.node_id.as_uuid(),
            lease_until,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    /// Release leadership.
    pub async fn release_leadership(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        // Release the advisory lock on the same session that acquired it.
        let mut lock_connection = self.lock_connection.lock().await;
        if let Some(mut conn) = lock_connection.take() {
            sqlx::query_scalar!("SELECT pg_advisory_unlock($1)", self.role.lock_id())
                .fetch_one(&mut *conn)
                .await
                .map_err(forge_core::ForgeError::Database)?;
        } else {
            tracing::warn!(
                role = self.role.as_str(),
                "Leader lock connection missing during release"
            );
        }
        drop(lock_connection);

        // Clear leadership record
        sqlx::query!(
            r#"
            DELETE FROM forge_leaders
            WHERE role = $1 AND node_id = $2
            "#,
            self.role.as_str(),
            self.node_id.as_uuid(),
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        self.is_leader.store(false, Ordering::SeqCst);
        crate::cluster::metrics::set_is_leader(self.role.as_str(), false);
        tracing::info!(role = self.role.as_str(), "Released leadership");

        Ok(())
    }

    /// Check if the current leader is healthy.
    pub async fn check_leader_health(&self) -> forge_core::Result<bool> {
        let result = sqlx::query_scalar!(
            "SELECT lease_until FROM forge_leaders WHERE role = $1",
            self.role.as_str()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        match result {
            Some(lease_until) => Ok(lease_until > Utc::now()),
            None => Ok(false), // No leader
        }
    }

    /// Get current leader info.
    pub async fn get_leader(&self) -> forge_core::Result<Option<LeaderInfo>> {
        let row = sqlx::query!(
            r#"
            SELECT role, node_id, acquired_at, lease_until
            FROM forge_leaders
            WHERE role = $1
            "#,
            self.role.as_str(),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        match row {
            Some(row) => {
                let role = row.role.parse().unwrap_or_else(|_| {
                    tracing::warn!(role = %row.role, "Unknown leader role, defaulting to Scheduler");
                    LeaderRole::Scheduler
                });

                Ok(Some(LeaderInfo {
                    role,
                    node_id: NodeId::from_uuid(row.node_id),
                    acquired_at: row.acquired_at,
                    lease_until: row.lease_until,
                }))
            }
            None => Ok(None),
        }
    }

    /// Run the leader election loop.
    pub async fn run(&self) {
        let mut shutdown_rx = self.shutdown_rx.clone();

        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.check_interval) => {
                    if self.is_leader() {
                        // We're the leader, refresh lease
                        if let Err(e) = self.refresh_lease().await {
                            tracing::debug!(error = %e, "Failed to refresh lease");
                        }
                    } else {
                        // We're a standby, check if we should try to become leader
                        match self.check_leader_health().await {
                            Ok(false) => {
                                // No healthy leader, try to become one
                                if let Err(e) = self.try_become_leader().await {
                                    tracing::debug!(error = %e, "Failed to acquire leadership");
                                }
                            }
                            Ok(true) => {
                                // Leader is healthy, stay as standby
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "Failed to check leader health");
                            }
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::debug!("Leader election shutting down");
                        if let Err(e) = self.release_leadership().await {
                            tracing::debug!(error = %e, "Failed to release leadership");
                        }
                        break;
                    }
                }
            }
        }
    }
}

/// RAII guard for leader-only operations.
pub struct LeaderGuard<'a> {
    election: &'a LeaderElection,
}

impl<'a> LeaderGuard<'a> {
    /// Try to create a leader guard.
    /// Returns None if not the leader.
    pub fn try_new(election: &'a LeaderElection) -> Option<Self> {
        if election.is_leader() {
            Some(Self { election })
        } else {
            None
        }
    }

    /// Check if still leader.
    pub fn is_leader(&self) -> bool {
        self.election.is_leader()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leader_config_default() {
        let config = LeaderConfig::default();
        assert_eq!(config.check_interval, Duration::from_secs(5));
        assert_eq!(config.lease_duration, Duration::from_secs(60));
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use forge_core::cluster::{LeaderInfo, LeaderRole, NodeId};
use tokio::sync::watch;

/// Leader election configuration.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    /// How often standbys check leader health.
    pub check_interval: Duration,
    /// Lease duration (leader must refresh before expiry).
    pub lease_duration: Duration,
    /// Lease refresh interval.
    pub refresh_interval: Duration,
}

impl Default for LeaderConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            lease_duration: Duration::from_secs(60),
            refresh_interval: Duration::from_secs(30),
        }
    }
}

/// Leader election using PostgreSQL advisory locks.
pub struct LeaderElection {
    pool: sqlx::PgPool,
    node_id: NodeId,
    role: LeaderRole,
    config: LeaderConfig,
    is_leader: Arc<AtomicBool>,
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
        // Try to acquire advisory lock (non-blocking)
        let result: Option<(bool,)> = sqlx::query_as("SELECT pg_try_advisory_lock($1) as acquired")
            .bind(self.role.lock_id())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let acquired = result.map(|(v,)| v).unwrap_or(false);

        if acquired {
            // Record leadership in database for visibility
            let lease_until =
                Utc::now() + chrono::Duration::seconds(self.config.lease_duration.as_secs() as i64);

            sqlx::query(
                r#"
                INSERT INTO forge_leaders (role, node_id, acquired_at, lease_until)
                VALUES ($1, $2, NOW(), $3)
                ON CONFLICT (role) DO UPDATE SET
                    node_id = EXCLUDED.node_id,
                    acquired_at = NOW(),
                    lease_until = EXCLUDED.lease_until
                "#,
            )
            .bind(self.role.as_str())
            .bind(self.node_id.as_uuid())
            .bind(lease_until)
            .execute(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

            self.is_leader.store(true, Ordering::SeqCst);
            tracing::info!("Became {} leader", self.role.as_str());
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

        sqlx::query(
            r#"
            UPDATE forge_leaders
            SET lease_until = $3
            WHERE role = $1 AND node_id = $2
            "#,
        )
        .bind(self.role.as_str())
        .bind(self.node_id.as_uuid())
        .bind(lease_until)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Release leadership.
    pub async fn release_leadership(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        // Release the advisory lock
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(self.role.lock_id())
            .execute(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        // Clear leadership record
        sqlx::query(
            r#"
            DELETE FROM forge_leaders
            WHERE role = $1 AND node_id = $2
            "#,
        )
        .bind(self.role.as_str())
        .bind(self.node_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        self.is_leader.store(false, Ordering::SeqCst);
        tracing::info!("Released {} leadership", self.role.as_str());

        Ok(())
    }

    /// Check if the current leader is healthy.
    pub async fn check_leader_health(&self) -> forge_core::Result<bool> {
        let result: Option<(DateTime<Utc>,)> =
            sqlx::query_as("SELECT lease_until FROM forge_leaders WHERE role = $1")
                .bind(self.role.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        match result {
            Some((lease_until,)) => Ok(lease_until > Utc::now()),
            None => Ok(false), // No leader
        }
    }

    /// Get current leader info.
    pub async fn get_leader(&self) -> forge_core::Result<Option<LeaderInfo>> {
        let row = sqlx::query(
            r#"
            SELECT role, node_id, acquired_at, lease_until
            FROM forge_leaders
            WHERE role = $1
            "#,
        )
        .bind(self.role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        match row {
            Some(row) => {
                use sqlx::Row;
                let role_str: String = row.get("role");
                let role = LeaderRole::from_str(&role_str).unwrap_or(LeaderRole::Scheduler);

                Ok(Some(LeaderInfo {
                    role,
                    node_id: NodeId::from_uuid(row.get("node_id")),
                    acquired_at: row.get("acquired_at"),
                    lease_until: row.get("lease_until"),
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
                            tracing::warn!("Failed to refresh lease: {}", e);
                        }
                    } else {
                        // We're a standby, check if we should try to become leader
                        match self.check_leader_health().await {
                            Ok(false) => {
                                // No healthy leader, try to become one
                                if let Err(e) = self.try_become_leader().await {
                                    tracing::warn!("Failed to acquire leadership: {}", e);
                                }
                            }
                            Ok(true) => {
                                // Leader is healthy, stay as standby
                            }
                            Err(e) => {
                                tracing::warn!("Failed to check leader health: {}", e);
                            }
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Leader election shutting down");
                        if let Err(e) = self.release_leadership().await {
                            tracing::warn!("Failed to release leadership: {}", e);
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
        assert_eq!(config.refresh_interval, Duration::from_secs(30));
    }
}

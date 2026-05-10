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
    ///
    /// The advisory lock and the `forge_leaders` INSERT run on the same
    /// connection. If that connection dies between the lock acquire and the
    /// INSERT, PostgreSQL releases the lock and the INSERT fails together —
    /// no torn leader rows pointing at a node that holds nothing.
    pub async fn try_become_leader(&self) -> forge_core::Result<bool> {
        if self.is_leader() {
            return Ok(true);
        }

        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        let acquired = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) as "acquired!""#,
            self.role.lock_id()
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        crate::cluster::metrics::record_leader_election_attempt(self.role.as_str(), acquired);

        if acquired {
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
            .execute(&mut *conn)
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
    ///
    /// Validates the advisory lock is still held on the lock-owning connection
    /// before refreshing the lease. If PostgreSQL released the lock (backend
    /// terminated, sqlx reconnected, etc.) we drop leadership locally and
    /// surface an error — keeping the lease alive without the underlying lock
    /// would risk split brain.
    pub async fn refresh_lease(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        let mut lock_connection = self.lock_connection.lock().await;
        let conn = match lock_connection.as_mut() {
            Some(conn) => conn,
            None => {
                drop(lock_connection);
                self.drop_leadership_locally();
                return Err(forge_core::ForgeError::Cluster(
                    "Lock connection missing during lease refresh; dropped leadership".into(),
                ));
            }
        };

        // pg_locks splits a single-int8 advisory lock into classid (upper 32 bits)
        // and objid (lower 32 bits), both stored as oid but exposed as int4. The
        // signed-cast preserves the bit pattern that PostgreSQL stores internally.
        let lock_id = self.role.lock_id();
        let classid = (lock_id >> 32) as i32;
        let objid = (lock_id & 0xFFFF_FFFF) as i32;

        let still_held = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM pg_locks
                WHERE locktype = 'advisory'
                  AND classid::int = $1
                  AND objid::int = $2
                  AND pid = pg_backend_pid()
                  AND granted
            ) AS "held!"
            "#,
            classid,
            objid,
        )
        .fetch_one(&mut **conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if !still_held {
            *lock_connection = None;
            drop(lock_connection);
            self.drop_leadership_locally();
            tracing::error!(
                role = self.role.as_str(),
                "Advisory lock no longer held on leader connection; dropped leadership"
            );
            return Err(forge_core::ForgeError::Cluster(
                "Advisory lock no longer held; dropped leadership".into(),
            ));
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
        .execute(&mut **conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    fn drop_leadership_locally(&self) {
        self.is_leader.store(false, Ordering::SeqCst);
        crate::cluster::metrics::set_is_leader(self.role.as_str(), false);
    }

    /// Release leadership.
    pub async fn release_leadership(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        // Release the advisory lock on the same session that acquired it.
        // pg_advisory_unlock returns true iff this session held the lock and
        // released it. A false result means we lost the lock between acquire
        // and release without refresh_lease catching it (PG terminated the
        // backend, sqlx reconnected, etc.) — warn so the operator sees the
        // miss instead of silently swallowing it. Unlike refresh_lease, this
        // is a shutdown path: we keep going to clear the leader row and local
        // state, since the worst case is already a no-op (split brain is
        // resolved by the lock being gone).
        let mut lock_connection = self.lock_connection.lock().await;
        if let Some(mut conn) = lock_connection.take() {
            let released = sqlx::query_scalar!(
                "SELECT pg_advisory_unlock($1) as \"released!\"",
                self.role.lock_id()
            )
            .fetch_one(&mut *conn)
            .await
            .map_err(forge_core::ForgeError::Database)?;

            if !released {
                tracing::warn!(
                    role = self.role.as_str(),
                    "pg_advisory_unlock returned false during release; \
                     lock was not held by this session"
                );
            }
        } else {
            // Reachable when try_become_leader failed mid-way after setting
            // is_leader=true (shouldn't happen with current code) or after a
            // refresh_lease detected loss and cleared the slot.
            tracing::warn!(
                role = self.role.as_str(),
                "Leader lock connection missing during release"
            );
        }
        drop(lock_connection);

        // Clear leadership record. WHERE node_id = $2 makes this safe even
        // when the lock was lost out-of-band and another node has already
        // overwritten the row via ON CONFLICT — that node's row stays put.
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
        let system_sql = crate::migrations::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    #[tokio::test]
    async fn refresh_lease_drops_leadership_when_lock_lost() {
        let db = setup_db("leader_refresh_lock_lost").await;
        let election = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );

        assert!(election.try_become_leader().await.unwrap());
        assert!(election.is_leader());

        // Simulate a connection-level loss of the advisory lock by manually
        // unlocking on the same connection that holds it. This mirrors the
        // failure mode the audit calls out (PG terminated the backend, sqlx
        // reconnected, etc.).
        {
            let mut conn_guard = election.lock_connection.lock().await;
            let conn = conn_guard.as_mut().expect("lock connection present");
            sqlx::query_scalar!(
                "SELECT pg_advisory_unlock($1) as \"released!\"",
                LeaderRole::Scheduler.lock_id()
            )
            .fetch_one(&mut **conn)
            .await
            .unwrap();
        }

        let err = election.refresh_lease().await.unwrap_err();
        assert!(matches!(err, forge_core::ForgeError::Cluster(_)));
        assert!(!election.is_leader());
    }

    #[tokio::test]
    async fn refresh_lease_succeeds_while_lock_held() {
        let db = setup_db("leader_refresh_lock_held").await;
        let election = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );

        assert!(election.try_become_leader().await.unwrap());
        for _ in 0..3 {
            election.refresh_lease().await.expect("refresh succeeds");
            assert!(election.is_leader());
        }
    }

    #[tokio::test]
    async fn try_become_leader_records_row_on_lock_connection() {
        let db = setup_db("leader_row_atomic").await;
        let election = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );

        assert!(election.try_become_leader().await.unwrap());

        let info = election
            .get_leader()
            .await
            .unwrap()
            .expect("leader row exists after acquire");
        assert_eq!(info.role, LeaderRole::Scheduler);
        assert_eq!(info.node_id, election.node_id);
    }

    /// release_leadership tolerates the lock having already gone away on
    /// the held connection (e.g., a PG-side backend reset). It must still
    /// clear local state and remove the leader row instead of erroring out
    /// halfway through cleanup.
    #[tokio::test]
    async fn release_leadership_handles_lock_already_gone() {
        let db = setup_db("leader_release_lock_gone").await;
        let election = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );

        assert!(election.try_become_leader().await.unwrap());

        // Drop the lock on the held connection without going through
        // release_leadership, simulating an out-of-band loss.
        {
            let mut conn_guard = election.lock_connection.lock().await;
            let conn = conn_guard.as_mut().expect("lock connection present");
            let released = sqlx::query_scalar!(
                "SELECT pg_advisory_unlock($1) as \"released!\"",
                LeaderRole::Scheduler.lock_id()
            )
            .fetch_one(&mut **conn)
            .await
            .unwrap();
            assert!(released, "preflight unlock must succeed");
        }

        // release_leadership should not error on the second unlock returning
        // false; it should still clear local state and the leader row.
        election.release_leadership().await.expect(
            "release path must tolerate pg_advisory_unlock returning false",
        );
        assert!(!election.is_leader());
        assert!(
            election.get_leader().await.unwrap().is_none(),
            "leader row removed even when unlock returned false"
        );
    }
}

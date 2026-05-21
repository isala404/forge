use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use forge_core::cluster::{LeaderInfo, LeaderRole, NodeId};
use tokio::sync::{Mutex, watch};

use crate::pg::notify_bus::PgNotifyBus;

/// PG NOTIFY channel pinged when a leader voluntarily releases its slot.
/// Payload is the role string; subscribers filter by their own role.
pub const LEADER_RELEASED_CHANNEL: &str = "forge_leader_released";

/// Leader election configuration.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    /// How often standbys check leader health and leaders refresh the
    /// `forge_leaders` lease row.
    pub check_interval: Duration,
    /// Lease duration. The leader must refresh before expiry or standbys
    /// will assume the seat is vacant.
    pub lease_duration: Duration,
    /// How often the leader re-checks `pg_locks` to confirm it still holds
    /// the advisory lock on its lock-owning connection. Defaults to 1s so
    /// a long lease (60s) still detects an out-of-band lock loss within a
    /// second instead of waiting for the next refresh tick.
    pub lock_validate_interval: Duration,
    /// How often a lightweight `SELECT 1` is issued on the lock-owning
    /// connection to prevent firewalls, load-balancers, or PostgreSQL's own
    /// `tcp_keepalives_idle` from silently terminating an idle connection
    /// and thereby releasing the advisory lock without the process noticing.
    /// Should be well below the shortest idle-connection timeout in the
    /// network path (typical firewall idle timeout is 5–10 minutes; 30 s
    /// gives a comfortable margin).  Defaults to 30 s.
    pub keepalive_interval: Duration,
}

impl Default for LeaderConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            lease_duration: Duration::from_secs(60),
            lock_validate_interval: Duration::from_secs(1),
            keepalive_interval: Duration::from_secs(30),
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
    /// Cached result of the last successful `pg_locks` probe. Set to `None`
    /// when a keepalive failure invalidates the cache, forcing the next
    /// `validate_lock_held` to actually query `pg_locks`.
    last_lock_validated: Mutex<Option<std::time::Instant>>,
    /// Optional notify bus used to (a) emit a NOTIFY on
    /// `forge_leader_released` from the outgoing leader during voluntary
    /// shutdown so standbys take over immediately instead of waiting for
    /// `check_interval`, and (b) subscribe on standbys to wake the election
    /// loop without polling the lease table.
    notify_bus: Option<Arc<PgNotifyBus>>,
}

impl std::fmt::Debug for LeaderElection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaderElection")
            .field("role", &self.role)
            .field(
                "is_leader",
                &self.is_leader.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
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
            last_lock_validated: Mutex::new(None),
            notify_bus: None,
        }
    }

    /// Attach a [`PgNotifyBus`] so this election emits NOTIFY on
    /// `forge_leader_released` during voluntary release and subscribes to
    /// the same channel to wake standbys without waiting for the next
    /// `check_interval` tick.
    pub fn with_notify_bus(mut self, bus: Arc<PgNotifyBus>) -> Self {
        self.notify_bus = Some(bus);
        self
    }

    /// Check if this node is the leader.
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::SeqCst)
    }

    /// Get a shutdown receiver.
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// How often the leader validates the advisory lock is still held.
    pub fn lock_validate_interval(&self) -> Duration {
        self.config.lock_validate_interval
    }

    /// How often the leader refreshes its lease row in `forge_leaders`.
    ///
    /// Daemon runners use this cadence to call `refresh_lease()` so that
    /// standbys see a live lease and can distinguish a running leader from a
    /// zombie whose lease has simply expired.
    pub fn check_interval(&self) -> Duration {
        self.config.check_interval
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
    ///
    /// **Zombie leader preemption**: if `pg_try_advisory_lock` fails but the
    /// current leader's lease is stale (expired), the application process that
    /// owned the lock is presumed dead. A connection pooler may be keeping the
    /// PG backend alive, preventing automatic lock release. In that case we
    /// locate the lock-holding backend via `pg_locks` and call
    /// `pg_terminate_backend()` to evict it, then retry the lock acquisition
    /// once. `pg_terminate_backend` requires superuser or `pg_signal_backend`
    /// role; if the call is refused or the backend is already gone, we log and
    /// return `false` rather than erroring out — election will be retried on
    /// the next check interval tick.
    pub async fn try_become_leader(&self) -> forge_core::Result<bool> {
        if self.is_leader() {
            return Ok(true);
        }

        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        let mut acquired = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) as "acquired!""#,
            self.role.lock_id()
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        // If the lock was not acquired, check whether the current leader is a
        // zombie: its lease is expired (application dead) but its PG backend
        // is still alive (kept by a connection pooler). If so, forcibly evict
        // the backend and retry once.
        if !acquired {
            match self.try_preempt_zombie_leader(&mut conn).await {
                Ok(true) => {
                    acquired = true;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        role = self.role.as_str(),
                        error = %e,
                        "Zombie leader preemption attempt failed; will retry next election cycle"
                    );
                }
            }
        }

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

    /// Attempt to preempt a zombie leader by terminating its PG backend.
    ///
    /// A zombie leader is one whose `forge_leaders` lease has expired but whose
    /// PG backend is still alive (held open by a connection pooler), preventing
    /// automatic advisory lock release.
    ///
    /// Steps:
    /// 1. Check if there is a stale lease in `forge_leaders` for this role.
    /// 2. If stale, find the backend PID holding the advisory lock via `pg_locks`.
    /// 3. Call `pg_terminate_backend()` to evict it.
    /// 4. Retry `pg_try_advisory_lock` once on the provided connection.
    ///
    /// Returns `true` only if we successfully terminated the zombie backend
    /// **and** subsequently acquired the lock. Returns `false` if the lease is
    /// not yet stale, no lock-holding backend is found, termination was refused
    /// (insufficient privilege), or the retry still failed.
    async fn try_preempt_zombie_leader(
        &self,
        conn: &mut sqlx::pool::PoolConnection<sqlx::Postgres>,
    ) -> forge_core::Result<bool> {
        // 1. Check for a stale lease. We consider it stale when lease_until is
        //    in the past — the leader failed to refresh before its own deadline.
        let lease_expired = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM forge_leaders
                WHERE role = $1
                  AND lease_until < NOW()
            ) AS "expired!"
            "#,
            self.role.as_str(),
        )
        .fetch_one(&mut **conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if !lease_expired {
            return Ok(false);
        }

        // 2. Find the PID of the backend holding the advisory lock.
        //    pg_locks splits a single-int8 lock ID into classid (upper 32 bits)
        //    and objid (lower 32 bits); we use the same signed cast as
        //    validate_lock_held to match what PostgreSQL stores internally.
        let lock_id = self.role.lock_id();
        let classid = (lock_id >> 32) as i32;
        let objid = (lock_id & 0xFFFF_FFFF) as i32;

        let zombie_pid = sqlx::query_scalar!(
            r#"
            SELECT pid AS "pid?"
            FROM pg_locks
            WHERE locktype = 'advisory'
              AND classid::int = $1
              AND objid::int = $2
              AND granted
            LIMIT 1
            "#,
            classid,
            objid,
        )
        .fetch_one(&mut **conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let pid = match zombie_pid {
            Some(p) => p,
            None => {
                // Lock was released between our check and now — no zombie to
                // evict. Return false; the caller will retry next tick.
                tracing::debug!(
                    role = self.role.as_str(),
                    "Stale lease detected but no lock-holding backend found; \
                     lock may have already been released"
                );
                return Ok(false);
            }
        };

        // 3. Terminate the zombie backend. pg_terminate_backend returns true if
        //    the signal was sent, false if permission was denied or the backend
        //    was already gone. We warn rather than error on false so the caller
        //    can degrade gracefully.
        let terminated =
            sqlx::query_scalar!(r#"SELECT pg_terminate_backend($1) AS "terminated!""#, pid,)
                .fetch_one(&mut **conn)
                .await
                .map_err(forge_core::ForgeError::Database)?;

        if !terminated {
            tracing::warn!(
                role = self.role.as_str(),
                zombie_pid = pid,
                "Could not terminate zombie leader backend; \
                 may lack pg_signal_backend privilege or backend already exited. \
                 Leadership acquisition blocked until the connection pooler \
                 recycles the holding connection."
            );
            return Ok(false);
        }

        tracing::warn!(
            role = self.role.as_str(),
            zombie_pid = pid,
            "Terminated zombie leader backend with expired lease; retrying lock acquisition"
        );

        // 4. Retry the lock acquisition on the same connection. A small yield
        //    lets PG process the termination before we attempt the lock.
        tokio::task::yield_now().await;

        let acquired = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) AS "acquired!""#,
            self.role.lock_id(),
        )
        .fetch_one(&mut **conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(acquired)
    }

    /// Confirm the advisory lock is still held on the lock-owning connection.
    ///
    /// Runs on its own cadence (`lock_validate_interval`, default 1s) so a
    /// long lease (60s) still detects an out-of-band lock loss promptly. If
    /// PostgreSQL released the lock (backend terminated, sqlx reconnected,
    /// etc.) we drop leadership locally and surface an error: keeping the
    /// lease alive without the underlying lock would risk split brain.
    pub async fn validate_lock_held(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        {
            let cached = self.last_lock_validated.lock().await;
            if let Some(last) = *cached
                && last.elapsed() < self.config.lock_validate_interval
            {
                return Ok(());
            }
        }

        let mut lock_connection = self.lock_connection.lock().await;
        let conn = match lock_connection.as_mut() {
            Some(conn) => conn,
            None => {
                drop(lock_connection);
                self.drop_leadership_locally();
                return Err(forge_core::ForgeError::internal(
                    "Lock connection missing during validation; dropped leadership",
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
            self.invalidate_lock_cache().await;
            self.drop_leadership_locally();
            tracing::error!(
                role = self.role.as_str(),
                "Advisory lock no longer held on leader connection; dropped leadership"
            );
            return Err(forge_core::ForgeError::internal(
                "Advisory lock no longer held; dropped leadership",
            ));
        }

        *self.last_lock_validated.lock().await = Some(std::time::Instant::now());
        Ok(())
    }

    /// Send a lightweight keepalive ping on the lock-owning connection.
    ///
    /// Firewalls and load-balancers silently drop idle TCP connections after
    /// their idle-timeout (commonly 5–10 minutes). PostgreSQL may do the same
    /// via `tcp_keepalives_idle`. Either way the advisory lock is released
    /// without the process knowing, leading to silent leadership loss between
    /// `validate_lock_held` intervals.
    ///
    /// Issuing `SELECT 1` every 30 s keeps the connection active at the TCP
    /// level and ensures PostgreSQL doesn't reclaim the backend. This is a
    /// no-op for standbys (no lock connection) and is distinct from
    /// `validate_lock_held`: that method verifies the lock is still held;
    /// this method prevents the connection from going idle in the first place.
    pub async fn keepalive(&self) -> forge_core::Result<()> {
        if !self.is_leader() {
            return Ok(());
        }

        let mut lock_connection = self.lock_connection.lock().await;
        let conn = match lock_connection.as_mut() {
            Some(conn) => conn,
            None => return Ok(()),
        };

        use sqlx::Connection as _;
        conn.ping()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    /// Refresh the leadership lease.
    ///
    /// Validates the advisory lock first (`validate_lock_held`), then
    /// extends `forge_leaders.lease_until`. Both queries run against the
    /// same PG backend (the lock-owning connection), but the Mutex is
    /// released between them and re-acquired here, so they are *not* a
    /// single critical section. That's fine: the only racer is `validate`
    /// itself on a faster cadence, which is idempotent when the lock is
    /// held and drops leadership atomically when it isn't.
    pub async fn refresh_lease(&self) -> forge_core::Result<()> {
        self.validate_lock_held().await?;
        if !self.is_leader() {
            return Ok(());
        }

        let mut lock_connection = self.lock_connection.lock().await;
        let conn = match lock_connection.as_mut() {
            Some(conn) => conn,
            None => {
                drop(lock_connection);
                self.drop_leadership_locally();
                return Err(forge_core::ForgeError::internal(
                    "Lock connection missing during lease refresh; dropped leadership",
                ));
            }
        };

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

    /// Clear the cached `pg_locks` probe result so the next `validate_lock_held`
    /// actually queries the database.
    async fn invalidate_lock_cache(&self) {
        *self.last_lock_validated.lock().await = None;
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
            // Emit the released-notification on the same backend that still
            // holds the lock. Doing this before the unlock guarantees standbys
            // see the wake-up only when the lock is genuinely about to be
            // available; doing it on the lock-owning connection means PG flushes
            // the NOTIFY at commit on this very session. A failure here is
            // logged but not fatal: the worst case is that standbys fall back
            // to their normal `check_interval` timer.
            if let Err(e) = sqlx::query!(
                "SELECT pg_notify($1, $2)",
                LEADER_RELEASED_CHANNEL,
                self.role.as_str(),
            )
            .execute(&mut *conn)
            .await
            {
                tracing::warn!(
                    role = self.role.as_str(),
                    error = %e,
                    "Failed to emit leader-released NOTIFY; standbys will wait for next check tick",
                );
            }

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

            // Clear leadership record on the same connection that held the
            // advisory lock. Using the shared pool here would risk issuing the
            // DELETE on a different backend, which is fine for correctness but
            // would leave a window where the lock is gone but the row still
            // names us as leader. Running it on the same connection closes that
            // window entirely. WHERE node_id = $2 makes this safe even when the
            // lock was lost out-of-band and another node has already overwritten
            // the row via ON CONFLICT — that node's row stays put.
            sqlx::query!(
                r#"
            DELETE FROM forge_leaders
            WHERE role = $1 AND node_id = $2
            "#,
                self.role.as_str(),
                self.node_id.as_uuid(),
            )
            .execute(&mut *conn)
            .await
            .map_err(forge_core::ForgeError::Database)?;
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
                // The query filters by `self.role`, so any row returned must
                // belong to this role. If the stored string doesn't parse, the
                // table is corrupt — surface it instead of silently coercing.
                let role = row.role.parse::<LeaderRole>().map_err(|_| {
                    forge_core::ForgeError::internal(format!(
                        "forge_leaders row has unrecognised role string: {:?}",
                        row.role
                    ))
                })?;

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
    ///
    /// Three independent cadences:
    /// - `lock_validate_interval` (leader only): re-check `pg_locks` to confirm
    ///   the advisory lock is still held. Faster than `check_interval` so a
    ///   long lease detects an out-of-band lock loss within seconds.
    /// - `check_interval` (leader): refresh the lease row. Validates first
    ///   inside `refresh_lease`, so the validate is idempotent with the
    ///   faster timer above.
    /// - `check_interval` (standby): check whether the current leader's
    ///   lease is healthy and try to take over if not.
    pub async fn run(&self) {
        let mut shutdown_rx = self.shutdown_rx.clone();
        let mut validate_timer = tokio::time::interval(self.config.lock_validate_interval);
        validate_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut check_timer = tokio::time::interval(self.config.check_interval);
        check_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut keepalive_timer = tokio::time::interval(self.config.keepalive_interval);
        keepalive_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Fast-failover wakeup: when an outgoing leader emits NOTIFY on
        // `forge_leader_released`, standbys for the same role attempt
        // acquisition immediately instead of waiting for the next
        // `check_interval` tick. The forwarder collapses any number of
        // notifications into a single `Notify::notify_one` so a backlog
        // doesn't queue up multiple acquisition attempts.
        let release_wakeup = Arc::new(tokio::sync::Notify::new());
        let release_forwarder = if let Some(bus) = self.notify_bus.as_ref()
            && let Some(mut rx) = bus.subscribe(LEADER_RELEASED_CHANNEL)
        {
            let wakeup = release_wakeup.clone();
            let role = self.role.as_str().to_string();
            let mut forwarder_shutdown = self.shutdown_rx.clone();
            Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = forwarder_shutdown.changed() => {
                            if *forwarder_shutdown.borrow() {
                                return;
                            }
                        }
                        result = rx.recv() => {
                            match result {
                                Ok(payload) => {
                                    if payload == role {
                                        wakeup.notify_one();
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::debug!(
                                        missed = n,
                                        "Leader-released wakeup receiver lagged"
                                    );
                                    wakeup.notify_one();
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                            }
                        }
                    }
                }
            }))
        } else {
            None
        };

        loop {
            tokio::select! {
                _ = validate_timer.tick() => {
                    // validate_lock_held is a no-op for standbys, so we don't
                    // need an outer is_leader() guard here.
                    if let Err(e) = self.validate_lock_held().await {
                        tracing::debug!(error = %e, "Lock validation failed");
                    }
                }
                _ = keepalive_timer.tick() => {
                    if let Err(e) = self.keepalive().await {
                        tracing::warn!(error = %e, "Leader connection keepalive failed; validating lock");
                        // A keepalive failure means the lock-owning connection
                        // may be dead. Invalidate the cache and validate
                        // immediately rather than waiting for the next
                        // lock_validate_interval tick.
                        self.invalidate_lock_cache().await;
                        if let Err(ve) = self.validate_lock_held().await {
                            tracing::warn!(error = %ve, "Lock validation after keepalive failure dropped leadership");
                        }
                    }
                }
                _ = check_timer.tick() => {
                    if self.is_leader() {
                        if let Err(e) = self.refresh_lease().await {
                            tracing::debug!(error = %e, "Failed to refresh lease");
                        }
                    } else {
                        match self.check_leader_health().await {
                            Ok(false) => {
                                if let Err(e) = self.try_become_leader().await {
                                    tracing::debug!(error = %e, "Failed to acquire leadership");
                                }
                            }
                            Ok(true) => {}
                            Err(e) => {
                                tracing::debug!(error = %e, "Failed to check leader health");
                            }
                        }
                    }
                }
                _ = release_wakeup.notified() => {
                    // Outgoing leader announced release; jump straight to an
                    // acquisition attempt. `try_become_leader` is a no-op when
                    // we already hold the lock, so it's safe even if we
                    // happened to win between the NOTIFY and now.
                    if !self.is_leader()
                        && let Err(e) = self.try_become_leader().await
                    {
                        tracing::debug!(error = %e, "Failed to acquire leadership after release NOTIFY");
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

        if let Some(handle) = release_forwarder {
            handle.abort();
        }
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
        assert_eq!(config.lock_validate_interval, Duration::from_secs(1));
        assert_eq!(config.keepalive_interval, Duration::from_secs(30));
        assert!(
            config.lock_validate_interval < config.check_interval,
            "validate must run faster than check or it serves no purpose",
        );
        assert!(
            config.keepalive_interval < Duration::from_secs(5 * 60),
            "keepalive must fire well before typical firewall idle timeout (5 min)",
        );
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
        assert!(matches!(err, forge_core::ForgeError::Internal { .. }));
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
        election
            .release_leadership()
            .await
            .expect("release path must tolerate pg_advisory_unlock returning false");
        assert!(!election.is_leader());
        assert!(
            election.get_leader().await.unwrap().is_none(),
            "leader row removed even when unlock returned false"
        );
    }

    /// validate_lock_held detects an out-of-band lock loss and drops
    /// leadership without touching the lease row. The separate validate
    /// path is what lets the run loop catch a lost lock within
    /// `lock_validate_interval` even when `check_interval` is much larger.
    #[tokio::test]
    async fn validate_lock_held_drops_leadership_when_lock_lost() {
        let db = setup_db("leader_validate_lock_lost").await;
        let election = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );

        assert!(election.try_become_leader().await.unwrap());

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

        let err = election.validate_lock_held().await.unwrap_err();
        assert!(matches!(err, forge_core::ForgeError::Internal { .. }));
        assert!(!election.is_leader());
    }

    /// validate_lock_held is a no-op for standbys and an OK for held leaders.
    /// Calling it many times in a row must not require a lease refresh.
    #[tokio::test]
    async fn validate_lock_held_is_idempotent_when_held() {
        let db = setup_db("leader_validate_idempotent").await;
        let election = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );

        // Standby case: no error, no state change.
        election
            .validate_lock_held()
            .await
            .expect("standby validate must be a no-op");
        assert!(!election.is_leader());

        // Leader case: many validates between lease refreshes.
        assert!(election.try_become_leader().await.unwrap());
        for _ in 0..5 {
            election
                .validate_lock_held()
                .await
                .expect("validate must succeed while lock held");
            assert!(election.is_leader());
        }
    }

    /// try_become_leader skips zombie preemption when the lease is still valid.
    ///
    /// If another node holds the lock and its lease is current (not expired),
    /// we must not attempt termination — that would be a hostile preemption of
    /// a healthy leader. `try_become_leader` must return `false` without issuing
    /// any `pg_terminate_backend` call.
    #[tokio::test]
    async fn try_become_leader_does_not_preempt_healthy_leader() {
        let db = setup_db("leader_no_preempt_healthy").await;

        // Node A acquires leadership.
        let leader = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );
        assert!(leader.try_become_leader().await.unwrap());

        // Node B tries to acquire but must not succeed — leader A is healthy.
        let standby = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );
        let got = standby.try_become_leader().await.unwrap();
        assert!(!got, "standby must not preempt a healthy leader");
        assert!(!standby.is_leader());

        // A is still leader.
        assert!(leader.is_leader());
    }

    /// try_become_leader acquires leadership after preempting a zombie.
    ///
    /// Simulates a zombie leader: the `forge_leaders` lease is expired (the
    /// application process died without refreshing), but the PG backend that
    /// holds the advisory lock is still alive (connection pooler scenario).
    /// After the standby calls `try_become_leader`, it must terminate the
    /// zombie backend and take over.
    #[tokio::test]
    async fn try_become_leader_preempts_zombie_with_expired_lease() {
        let db = setup_db("leader_preempt_zombie").await;

        // Acquire leadership normally.
        let zombie = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );
        assert!(zombie.try_become_leader().await.unwrap());
        assert!(zombie.is_leader());

        // Artificially expire the lease so standbys see a stale leader.
        #[allow(clippy::disallowed_methods)]
        sqlx::query(
            "UPDATE forge_leaders SET lease_until = NOW() - INTERVAL '1 second' WHERE role = $1",
        )
        .bind(LeaderRole::Scheduler.as_str())
        .execute(db.pool())
        .await
        .unwrap();

        // The zombie's lock-holding connection is still alive (we haven't
        // dropped it), simulating a connection-pooler-kept backend.
        //
        // A standby now tries to acquire. It should detect the expired lease,
        // find the lock-holding PID, terminate it, and acquire the lock.
        let standby = LeaderElection::new(
            db.pool().clone(),
            NodeId::new(),
            LeaderRole::Scheduler,
            LeaderConfig::default(),
        );
        let got = standby.try_become_leader().await.unwrap();
        assert!(
            got,
            "standby must take over after terminating zombie backend"
        );
        assert!(standby.is_leader());
    }
}

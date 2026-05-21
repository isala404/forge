//! Process-wide PostgreSQL LISTEN/NOTIFY multiplexer.
//!
//! Reduces O(N) `PgListener` connections (one per subsystem) to a single
//! connection that fans out received notifications to per-channel broadcast
//! subscribers. Subsystems call [`PgNotifyBus::subscribe`] to obtain a
//! `broadcast::Receiver<String>` for their channel instead of managing their
//! own `PgListener` lifecycle and reconnection logic.
//!
//! # Reconnection
//!
//! The bus owns the only LISTEN connection. When that connection drops, the
//! run loop reconnects with exponential backoff (500 ms to 30 s) and
//! re-issues LISTEN on every registered channel. Subscribers see no
//! interruption other than a brief gap in notifications (the same gap the
//! old per-subsystem listeners had, except now there is exactly one
//! reconnect path to maintain).
//!
//! # Payload semantics
//!
//! The bus forwards the raw `notification.payload()` string. Channels that
//! use structured JSON payloads (typed via [`NotifyChannel`]) decode on the
//! subscriber side, just as before. Channels with custom string formats
//! (e.g. `forge_changes` with its `v1:table:OP:...` wire format) pass
//! through unmodified.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};

/// Per-channel broadcast buffer size. Subscribers that fall behind by more
/// than this many messages will see `RecvError::Lagged` and can decide
/// whether to catch up or resync.
const CHANNEL_BUFFER_SIZE: usize = 256;

/// Initial reconnection delay after a `PgListener` disconnect.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Maximum reconnection delay.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Process-wide PostgreSQL LISTEN multiplexer.
///
/// Create one at runtime startup, register all channels up front, then call
/// [`run`](Self::run) in a background task. Subsystems obtain a
/// `broadcast::Receiver<String>` via [`subscribe`](Self::subscribe).
pub struct PgNotifyBus {
    pool: sqlx::PgPool,
    /// Channel name -> broadcast sender. Populated at construction time;
    /// immutable afterwards.
    senders: Arc<HashMap<String, broadcast::Sender<String>>>,
    /// Ticks once per successful (re)connect. The initial connect publishes
    /// generation `1`; subsequent reconnects publish `2`, `3`, ... so a
    /// subscriber that snapshots the value at startup can detect a reconnect
    /// strictly later than its own start without being woken by the boot
    /// connect. Subscribers that need replay-on-reconnect call
    /// [`subscribe_reconnects`](Self::subscribe_reconnects) and react to
    /// changes whose value is greater than the snapshot they observed when
    /// they subscribed.
    reconnect_tx: watch::Sender<u64>,
}

impl PgNotifyBus {
    /// Create a new notify bus for the given channels.
    ///
    /// Each channel gets a `broadcast::channel(256)` so subscribers can lag
    /// slightly without losing messages. The bus does not start listening
    /// until [`run`](Self::run) is called.
    pub fn new(pool: sqlx::PgPool, channels: &[&str]) -> Self {
        let mut senders = HashMap::with_capacity(channels.len());
        for &ch in channels {
            let (tx, _) = broadcast::channel(CHANNEL_BUFFER_SIZE);
            senders.insert(ch.to_string(), tx);
        }
        // Generation starts at 0 so the first successful connect (which
        // publishes 1) is distinguishable from "never connected" and the
        // value a subscriber sees the moment they call
        // `subscribe_reconnects()` doesn't already look like a reconnect.
        let (reconnect_tx, _) = watch::channel(0u64);
        Self {
            pool,
            senders: Arc::new(senders),
            reconnect_tx,
        }
    }

    /// Subscribe to notifications on `channel`.
    ///
    /// Returns `None` if `channel` was not registered at construction time.
    /// The returned receiver yields the raw NOTIFY payload string.
    pub fn subscribe(&self, channel: &str) -> Option<broadcast::Receiver<String>> {
        self.senders.get(channel).map(|tx| tx.subscribe())
    }

    /// Returns the set of channel names this bus is configured for.
    pub fn channels(&self) -> Vec<&str> {
        self.senders.keys().map(|s| s.as_str()).collect()
    }

    /// Subscribe to reconnect events.
    ///
    /// The returned receiver's value is bumped once per successful (re)connect
    /// of the underlying `PgListener`. The initial connect publishes `1`;
    /// subsequent reconnects publish `2`, `3`, etc. Subscribers that want to
    /// trigger gap recovery on reconnect should:
    ///
    /// 1. Call `subscribe_reconnects()` and snapshot the current generation
    ///    via `*rx.borrow()`.
    /// 2. In their main loop, `select!` on `rx.changed()` alongside their
    ///    payload `recv()`.
    /// 3. On a change, compare `*rx.borrow()` to the snapshot and only treat
    ///    it as a reconnect if it is strictly greater than the snapshot —
    ///    this filters the first-boot connect for subscribers that attach
    ///    before `run()` succeeds.
    pub fn subscribe_reconnects(&self) -> watch::Receiver<u64> {
        self.reconnect_tx.subscribe()
    }

    /// Run the listener loop until `shutdown` fires.
    ///
    /// This must be spawned as a background task. It owns the single
    /// `PgListener` connection, reconnects on failure, and fans out every
    /// received notification to the matching broadcast channel.
    pub async fn run(&self, shutdown: tokio::sync::watch::Receiver<bool>) {
        let channel_names: Vec<String> = self.senders.keys().cloned().collect();
        let mut backoff = INITIAL_BACKOFF;
        let mut shutdown = shutdown;

        loop {
            // --- connect + LISTEN on all channels ---
            let listener = match self.connect_and_listen(&channel_names).await {
                Ok(l) => {
                    backoff = INITIAL_BACKOFF;
                    // Bump the reconnect generation. Subscribers compare the
                    // observed value against the snapshot they took at
                    // subscribe-time, so the first connect (generation 1)
                    // only fires gap recovery for late subscribers — which
                    // is the safe behaviour anyway since a late subscriber
                    // could have missed events before attaching.
                    self.reconnect_tx.send_modify(|g| *g = g.saturating_add(1));
                    l
                }
                Err(e) => {
                    tracing::warn!(error = %e, "PgNotifyBus: connect/listen failed, retrying");
                    tokio::select! {
                        biased;
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() {
                                tracing::debug!("PgNotifyBus: shutdown during reconnect");
                                return;
                            }
                        }
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };

            tracing::info!(
                channels = ?channel_names,
                "PgNotifyBus: listening on {} channel(s)",
                channel_names.len(),
            );

            // --- recv loop ---
            if self.recv_loop(listener, &mut shutdown).await {
                // shutdown requested
                tracing::debug!("PgNotifyBus: shutting down");
                return;
            }

            // recv_loop returned false => connection error, reconnect
            tracing::warn!("PgNotifyBus: connection lost, reconnecting");
        }
    }

    /// Connect a `PgListener` and LISTEN on every channel. Returns the
    /// ready listener or the first error encountered.
    async fn connect_and_listen(
        &self,
        channels: &[String],
    ) -> Result<sqlx::postgres::PgListener, sqlx::Error> {
        let mut listener = sqlx::postgres::PgListener::connect_with(&self.pool).await?;
        for ch in channels {
            listener.listen(ch).await?;
        }
        Ok(listener)
    }

    /// Receive notifications and fan out. Returns `true` if shutdown was
    /// requested, `false` if the connection broke.
    async fn recv_loop(
        &self,
        mut listener: sqlx::postgres::PgListener,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> bool {
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return true;
                    }
                }
                notification = listener.recv() => {
                    match notification {
                        Ok(n) => {
                            let channel = n.channel();
                            let payload = n.payload().to_string();
                            if let Some(tx) = self.senders.get(channel) {
                                // Ignore send errors — they mean no active receivers.
                                let _ = tx.send(payload);
                            } else {
                                tracing::debug!(
                                    channel = channel,
                                    "PgNotifyBus: notification on unregistered channel, ignoring",
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "PgNotifyBus: recv error");
                            return false;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn make_bus(channels: &[&str]) -> PgNotifyBus {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        PgNotifyBus::new(pool, channels)
    }

    #[tokio::test]
    async fn subscribe_returns_receiver_for_registered_channel() {
        let bus = make_bus(&["forge_changes", "forge_jobs_available"]);
        assert!(bus.subscribe("forge_changes").is_some());
        assert!(bus.subscribe("forge_jobs_available").is_some());
    }

    #[tokio::test]
    async fn subscribe_returns_none_for_unknown_channel() {
        let bus = make_bus(&["forge_changes"]);
        assert!(bus.subscribe("forge_nonexistent").is_none());
    }

    #[tokio::test]
    async fn channels_returns_all_registered_names() {
        let bus = make_bus(&[
            "forge_changes",
            "forge_jobs_available",
            "forge_workflow_wakeup",
        ]);
        let mut names = bus.channels();
        names.sort();
        assert_eq!(
            names,
            vec![
                "forge_changes",
                "forge_jobs_available",
                "forge_workflow_wakeup"
            ],
        );
    }

    #[tokio::test]
    async fn fan_out_delivers_to_all_subscribers() {
        let bus = make_bus(&["test_channel"]);
        let mut rx1 = bus.subscribe("test_channel").unwrap();
        let mut rx2 = bus.subscribe("test_channel").unwrap();

        // Simulate what the recv loop does: send directly on the broadcast.
        let tx = bus.senders.get("test_channel").unwrap();
        tx.send("hello".to_string()).unwrap();

        assert_eq!(rx1.recv().await.unwrap(), "hello");
        assert_eq!(rx2.recv().await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn send_without_subscribers_does_not_error() {
        let bus = make_bus(&["test_channel"]);
        let tx = bus.senders.get("test_channel").unwrap();
        // No subscribers — send returns Err but the bus must not care.
        let _ = tx.send("orphan".to_string());
    }

    #[tokio::test]
    async fn empty_channels_list_produces_empty_bus() {
        let bus = make_bus(&[]);
        assert!(bus.channels().is_empty());
        assert!(bus.subscribe("anything").is_none());
    }

    #[tokio::test]
    async fn reconnect_subscriber_starts_at_zero_and_observes_ticks() {
        // The reconnect generation starts at 0 so a freshly-attached
        // subscriber can distinguish the boot-time first connect from a
        // genuine reconnect by snapshotting the value at subscribe time.
        // Every successful (re)connect bumps the generation by one — we
        // simulate that by directly calling `send_modify` the way `run()`
        // does, since the real connect path requires a live PG backend.
        let bus = make_bus(&["test_channel"]);
        let mut rx = bus.subscribe_reconnects();
        assert_eq!(*rx.borrow(), 0, "fresh bus starts at generation 0");

        // First connect.
        bus.reconnect_tx.send_modify(|g| *g = g.saturating_add(1));
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), 1, "first connect publishes generation 1");

        // Reconnect.
        bus.reconnect_tx.send_modify(|g| *g = g.saturating_add(1));
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), 2, "reconnect publishes generation 2");
    }
}

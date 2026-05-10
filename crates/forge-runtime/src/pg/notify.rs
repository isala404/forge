//! Typed wrapper around `pg_notify` and `LISTEN`.
//!
//! Doctrine: every consumer that ships a typed JSON payload over PostgreSQL
//! NOTIFY goes through [`NotifyChannel`]. Custom string-format channels (the
//! `forge_changes` `v1:table:OP:row_id[#seq]` payload, for example) stay as
//! they are because their wire shape is part of the public schema and can
//! evolve independently of any Rust type.
//!
//! # Why a typed channel
//!
//! Without it, every site reinvents:
//!
//! - JSON serialization (`serde_json::to_string`) and deserialization on the
//!   listening side.
//! - The 8 KiB PostgreSQL `NOTIFY` payload limit. Hit that limit at runtime
//!   and the publish fails silently from the application's perspective; the
//!   trigger raises `ERROR:  payload string too long` and the wrapping
//!   transaction rolls back.
//! - `PgListener::connect_with` plus `listen` plus a `recv()` loop with
//!   parse-and-skip handling.
//!
//! [`NotifyChannel`] centralises these. It enforces a 7 KiB ceiling on the
//! serialized payload to leave headroom for PG framing, returns a typed
//! `ForgeError::InvalidArgument` when the caller exceeds it, and exposes a
//! `Stream<Item = T>` for subscribers so the listener loop is no longer
//! every consumer's problem.
//!
//! # When the payload is too big
//!
//! For records that don't fit, write the full row to `forge_change_log` and
//! publish only the row id over the channel. Subscribers fetch the body from
//! the log when they receive the notification. Helpers for the change-log
//! side live in [`crate::pg::change_log`] (forthcoming).

use std::marker::PhantomData;

use futures_util::stream::{Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::PgExecutor;
use sqlx::postgres::PgListener;

use forge_core::error::{ForgeError, Result};

/// Maximum serialized JSON payload bytes. PostgreSQL caps `NOTIFY` payloads
/// at 8000 bytes; we reserve ~1 KiB for PG framing, channel name, and the
/// `pg_notify` SQL wrapper.
pub const MAX_PAYLOAD_BYTES: usize = 7 * 1024;

/// Typed handle to a single PostgreSQL `NOTIFY` channel.
///
/// `T` is the JSON payload shape. Construct one per channel as a `const`-ish
/// value (`name` is `&'static str`) and reuse it everywhere that channel is
/// touched, so publish and subscribe sites can never disagree on the shape.
pub struct NotifyChannel<T> {
    name: &'static str,
    _marker: PhantomData<fn(T) -> T>,
}

impl<T> NotifyChannel<T> {
    /// Create a typed channel handle. `name` is the PostgreSQL channel
    /// identifier passed to `pg_notify` and `LISTEN`; it must be a valid
    /// SQL identifier (the framework uses snake_case `forge_*` names).
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }

    /// PostgreSQL channel name.
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl<T> NotifyChannel<T>
where
    T: Serialize,
{
    /// Publish `payload` on this channel.
    ///
    /// Errors:
    /// - `ForgeError::Serialization` if `serde_json::to_string(payload)` fails.
    /// - `ForgeError::InvalidArgument` if the serialized payload exceeds
    ///   [`MAX_PAYLOAD_BYTES`]. Use the change-log fallback for larger bodies.
    /// - `ForgeError::Database` if the underlying `SELECT pg_notify(...)`
    ///   fails (transaction rolled back, connection dropped, etc.).
    pub async fn publish<'e, E>(&self, executor: E, payload: &T) -> Result<()>
    where
        E: PgExecutor<'e>,
    {
        let body = serde_json::to_string(payload)
            .map_err(|e| ForgeError::Serialization(e.to_string()))?;
        if body.len() > MAX_PAYLOAD_BYTES {
            return Err(ForgeError::InvalidArgument(format!(
                "NotifyChannel `{}` payload is {} bytes, exceeds {} byte limit; \
                 write the body to forge_change_log and emit only the row id",
                self.name,
                body.len(),
                MAX_PAYLOAD_BYTES,
            )));
        }
        sqlx::query!("SELECT pg_notify($1, $2)", self.name, &body)
            .execute(executor)
            .await
            .map_err(ForgeError::Database)?;
        Ok(())
    }
}

impl<T> NotifyChannel<T>
where
    T: DeserializeOwned + Send + 'static,
{
    /// Subscribe to this channel and return a stream of decoded payloads.
    ///
    /// `listener` is consumed; the caller surrenders the connection to the
    /// stream for the duration of the subscription. Notifications whose
    /// payload fails JSON decoding are logged and skipped, so a malformed
    /// publish from one peer cannot tear down a long-running subscriber.
    /// Errors from the underlying `recv` (connection dropped, etc.) end the
    /// stream; the caller decides whether to reconnect.
    pub async fn subscribe(&self, mut listener: PgListener) -> Result<impl Stream<Item = T>> {
        listener
            .listen(self.name)
            .await
            .map_err(ForgeError::Database)?;
        let channel_name = self.name;
        let raw = listener.into_stream();
        let stream = raw
            .take_while(|res| {
                let cont = res.is_ok();
                async move { cont }
            })
            .filter_map(move |res| async move {
                let notification = match res {
                    Ok(n) => n,
                    Err(_) => return None,
                };
                match serde_json::from_str::<T>(notification.payload()) {
                    Ok(value) => Some(value),
                    Err(e) => {
                        tracing::debug!(
                            channel = channel_name,
                            error = %e,
                            payload = notification.payload(),
                            "NotifyChannel: dropping malformed payload",
                        );
                        None
                    }
                }
            });
        Ok(stream)
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
    use serde::Deserialize;
    use sqlx::postgres::PgListener;
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Wakeup {
        id: i64,
        kind: String,
    }

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        base.isolated(test_name)
            .await
            .expect("Failed to create isolated db")
    }

    #[tokio::test]
    async fn publish_then_subscribe_round_trip() {
        let db = setup_db("notify_round_trip").await;
        let channel: NotifyChannel<Wakeup> = NotifyChannel::new("forge_test_notify_round_trip");

        let listener = PgListener::connect_with(db.pool()).await.unwrap();
        let mut stream = Box::pin(channel.subscribe(listener).await.unwrap());

        // Publish must happen on a separate connection so the listener
        // (a different backend) actually sees the NOTIFY.
        let payload = Wakeup {
            id: 42,
            kind: "test".into(),
        };
        channel.publish(db.pool(), &payload).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream did not yield within 5s")
            .expect("stream ended before yielding");
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn publish_rejects_oversize_payload() {
        let db = setup_db("notify_oversize").await;
        let channel: NotifyChannel<String> = NotifyChannel::new("forge_test_notify_oversize");

        let big = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let err = channel.publish(db.pool(), &big).await.unwrap_err();
        assert!(matches!(err, ForgeError::InvalidArgument(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("forge_change_log"),
            "error should hint at the change-log fallback, got: {msg}",
        );
    }

    #[tokio::test]
    async fn subscribe_skips_malformed_payloads() {
        let db = setup_db("notify_malformed").await;
        // Subscriber expects {id, kind}; we will publish a non-JSON string and
        // then a real payload, and assert only the real one comes through.
        let channel: NotifyChannel<Wakeup> = NotifyChannel::new("forge_test_notify_malformed");
        let listener = PgListener::connect_with(db.pool()).await.unwrap();
        let mut stream = Box::pin(channel.subscribe(listener).await.unwrap());

        // Publish raw string via SQL (bypasses NotifyChannel's typed publish)
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind("forge_test_notify_malformed")
            .bind("not-json")
            .execute(db.pool())
            .await
            .unwrap();

        // Then a real payload through the typed publisher.
        let payload = Wakeup {
            id: 7,
            kind: "ok".into(),
        };
        channel.publish(db.pool(), &payload).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream did not yield within 5s")
            .expect("stream ended before yielding");
        assert_eq!(received, payload);
    }
}

//! `pubsub` — lightweight publish/subscribe for live fan-out to connected clients.
//! Lineage: Postgres `LISTEN`/`NOTIFY`, with Redis pub/sub delivery semantics
//! (fire-and-forget, no persistence, delivered only to currently-connected
//! subscribers). See `docs/contracts/pubsub.md`.
//!
//! This is the transport behind realtime features (GraphQL subscriptions, live
//! presence): a request handler `publish`es an event, and every open `subscribe`
//! stream for that topic receives it. It is deliberately *not* a queue — there is
//! no durability, ordering guarantee across connections, or redelivery. Use
//! [`crate::Queue`] when a message must not be lost.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;

use crate::error::Result;

#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub(crate) use pg::PgPubsub;

/// The Postgres `LISTEN`/`NOTIFY` channel a `topic` maps to. Exposed so a process
/// in another language (via the bindings) can `LISTEN` on the exact channel that a
/// `publish` notifies — useful when a host app already holds a native Postgres
/// connection and prefers to subscribe through it rather than over the FFI boundary.
pub fn channel_for(topic: &str) -> String {
    let h = crate::util::sha256_hex(topic.as_bytes());
    format!("forge_{}", h.get(..32).unwrap_or(h.as_str()))
}

/// Largest allowed topic in UTF-8 bytes. Over => [`crate::ForgeError::Invalid`].
pub const MAX_TOPIC_BYTES: usize = 256;

/// Largest allowed payload in bytes. Postgres caps a `NOTIFY` payload at 8000
/// bytes; we reserve headroom. Over => [`crate::ForgeError::Limit`]. For larger
/// data, publish a reference (e.g. a row id) and have the subscriber read it.
pub const MAX_PAYLOAD_BYTES: usize = 7000;

/// A live stream of payloads for one subscribed topic. Each item is one published
/// message; the stream ends (`None`) if the underlying connection drops.
pub type Subscription = BoxStream<'static, Result<Bytes>>;

/// Publish/subscribe over a single Postgres connection. Object-safe; the facade
/// hands out `&dyn Pubsub` via [`crate::Forge::pubsub`].
///
/// Exact delivery semantics (at-most-once, connected-only, no persistence) are in
/// `docs/contracts/pubsub.md`.
#[async_trait]
pub trait Pubsub: crate::sealed::Sealed + Send + Sync {
    /// Publish `payload` to every subscriber currently listening on `topic`.
    ///
    /// Fire-and-forget: returns `Ok` even with zero subscribers, and a message
    /// published while a subscriber is disconnected is **not** retained for it.
    /// `payload` must be valid UTF-8 (NOTIFY payloads are text) and at most
    /// [`MAX_PAYLOAD_BYTES`]; `topic` at most [`MAX_TOPIC_BYTES`].
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<()>;

    /// Subscribe to `topic`, returning a stream of payloads published *after* the
    /// returned future resolves. Subscriptions share one per-process listener
    /// connection (not a connection each); drop the stream to unsubscribe, and the
    /// channel is released once it has no remaining subscribers.
    async fn subscribe(&self, topic: &str) -> Result<Subscription>;
}

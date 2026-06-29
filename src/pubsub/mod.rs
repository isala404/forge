//! `pubsub`: lightweight publish/subscribe for live fan-out to connected clients.
//! Lineage: Postgres `LISTEN`/`NOTIFY`, with Redis pub/sub delivery semantics
//! (fire-and-forget, no persistence, delivered only to currently-connected
//! subscribers). See `docs/contracts/pubsub.md`.
//!
//! A request handler `publish`es an event, and every open `subscribe` stream for that
//! topic receives it. Not a queue: no durability, ordering across connections, or
//! redelivery. Use [`crate::queue::Queue`] when a message must not be lost.
//!
//! The [`Pubsub`] trait and [`Subscription`] live here, alongside the Postgres backend.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;

use crate::error::Result;

pub(crate) fn hashed_channel_for(topic: &str) -> String {
    let h = crate::util::sha256_hex(topic.as_bytes());
    format!("forge_{}", h.get(..32).unwrap_or(h.as_str()))
}

/// The legacy unnamespaced Postgres `LISTEN`/`NOTIFY` hash for `topic`.
///
/// Prefer [`Pubsub::channel_for`] (via `forge.pubsub().channel_for(topic)`); it includes
/// the configured Forge namespace and any backend-specific mapping.
#[deprecated(
    note = "use forge.pubsub().channel_for(topic) so namespaces and backend mapping are honored"
)]
pub fn channel_for(topic: &str) -> String {
    hashed_channel_for(topic)
}

/// Largest allowed topic in UTF-8 bytes. Over => [`crate::error::ForgeError::Invalid`].
pub const MAX_TOPIC_BYTES: usize = 256;

/// Largest allowed payload in bytes. Postgres caps a `NOTIFY` payload at 8000
/// bytes; we reserve headroom. Over => [`crate::error::ForgeError::Limit`]. For larger
/// data, publish a reference (e.g. a row id) and have the subscriber read it.
pub const MAX_PAYLOAD_BYTES: usize = 7000;

/// A live stream of payloads for one subscribed topic. Each item is one published
/// message; transient connection drops are re-established, so the stream ends (`None`)
/// only when the shared listener is shut down.
pub type Subscription = BoxStream<'static, Result<Bytes>>;

/// Publish/subscribe over a single Postgres connection. Reached via `forgelib::Forge::pubsub`.
///
/// Delivery semantics (at-most-once, connected-only, no persistence) are in
/// `docs/contracts/pubsub.md`.
#[async_trait]
pub trait Pubsub: Send + Sync {
    /// The backend channel a topic maps to. For Postgres, the `LISTEN`/`NOTIFY`
    /// channel, including any Forge namespace prefixing.
    fn channel_for(&self, topic: &str) -> Result<String>;

    /// Publish `payload` to every subscriber currently listening on `topic`.
    ///
    /// Fire-and-forget: returns `Ok` even with zero subscribers, and a message
    /// published while a subscriber is disconnected is not retained for it.
    /// `payload` must be valid UTF-8 (NOTIFY payloads are text) and at most
    /// [`MAX_PAYLOAD_BYTES`]; `topic` at most [`MAX_TOPIC_BYTES`].
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<()>;

    /// Subscribe to `topic`, returning a stream of payloads published after the
    /// returned future resolves. Subscriptions share one per-process listener
    /// connection (not a connection each); drop the stream to unsubscribe, and the
    /// channel is released once it has no remaining subscribers.
    async fn subscribe(&self, topic: &str) -> Result<Subscription>;
}

mod memory;
mod postgres;
pub(crate) use memory::MemPubsub;
pub(crate) use postgres::PgPubsub;

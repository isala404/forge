//! Postgres `pubsub` backend over `LISTEN`/`NOTIFY`. Contract: docs/contracts/pubsub.md.
//!
//! `publish` is one `pg_notify($channel, $payload)` on a pooled connection.
//! `subscribe` opens a *dedicated* connection (a long-lived `LISTEN` must not hold
//! a pooled connection hostage for its whole lifetime) and streams notifications.
//! Arbitrary topics are mapped to a valid, fixed-length channel identifier by hash.

use super::{MAX_PAYLOAD_BYTES, MAX_TOPIC_BYTES, Pubsub, Subscription};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::util::key_hash;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tracing::field::Empty;

/// Postgres-backed [`Pubsub`].
pub(crate) struct PgPubsub {
    pool: PgPool,
    /// Connection string for dedicated `LISTEN` connections. A subscription cannot
    /// borrow a pooled connection for its entire (open-ended) lifetime without
    /// starving the pool, so it opens its own.
    url: String,
}

impl PgPubsub {
    pub(crate) fn new(pool: PgPool, url: String) -> Self {
        Self { pool, url }
    }

    /// Map an arbitrary topic onto a valid Postgres channel name. Single source of
    /// truth shared with [`super::channel_for`].
    fn channel(topic: &str) -> String {
        super::channel_for(topic)
    }

    fn check_topic(topic: &str) -> Result<()> {
        if topic.is_empty() {
            return Err(ForgeError::invalid("pubsub topic must not be empty"));
        }
        if topic.len() > MAX_TOPIC_BYTES {
            return Err(ForgeError::invalid(format!(
                "pubsub topic exceeds {MAX_TOPIC_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl Pubsub for PgPubsub {
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<()> {
        let span = tracing::info_span!(
            "forge.pubsub.publish",
            pubsub.topic_hash = %key_hash(topic),
            pubsub.payload_bytes = payload.len(),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("pubsub", "publish", span, async move {
            Self::check_topic(topic)?;
            if payload.len() > MAX_PAYLOAD_BYTES {
                return Err(ForgeError::limit(format!(
                    "pubsub payload is {} bytes; max is {MAX_PAYLOAD_BYTES}",
                    payload.len()
                )));
            }
            // NOTIFY payloads are text; reject non-UTF-8 rather than corrupting it.
            let text = std::str::from_utf8(&payload).map_err(|_| {
                ForgeError::invalid("pubsub payload must be valid UTF-8 (NOTIFY payloads are text)")
            })?;
            let channel = Self::channel(topic);
            sqlx::query!("SELECT pg_notify($1, $2)", channel, text)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription> {
        Self::check_topic(topic)?;
        let channel = Self::channel(topic);

        let mut listener = PgListener::connect(&self.url)
            .await
            .map_err(ForgeError::from_sqlx)?;
        listener
            .listen(&channel)
            .await
            .map_err(ForgeError::from_sqlx)?;

        // subscribe has no completing Result to instrument like the other ops; emit a
        // counter so live subscription counts are still observable.
        metrics::counter!(
            "forge_ops_total",
            "primitive" => "pubsub",
            "op" => "subscribe",
            "outcome" => "ok",
        )
        .increment(1);

        let stream = listener.into_stream().map(|res| {
            res.map(|note| Bytes::copy_from_slice(note.payload().as_bytes()))
                .map_err(ForgeError::from_sqlx)
        });
        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_is_valid_fixed_length_identifier() {
        let c = PgPubsub::channel("chat:550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(c.len(), 38); // "forge_" + 32 hex
        assert!(c.starts_with("forge_"));
        assert!(c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        assert!(c.len() <= 63, "must fit a Postgres channel identifier");
    }

    #[test]
    fn distinct_topics_get_distinct_channels() {
        assert_ne!(PgPubsub::channel("chat:1"), PgPubsub::channel("chat:2"));
        assert_eq!(PgPubsub::channel("presence"), PgPubsub::channel("presence"));
    }

    #[test]
    fn empty_topic_is_invalid() {
        assert!(matches!(
            PgPubsub::check_topic(""),
            Err(ForgeError::Invalid(_))
        ));
    }
}

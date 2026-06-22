//! Postgres `pubsub` backend over `LISTEN`/`NOTIFY`. Contract: docs/contracts/pubsub.md.
//!
//! `publish` is one `pg_notify($channel, $payload)` on a pooled connection.
//! `subscribe` registers a topic's channel on a single shared `LISTEN` connection
//! (a per-process broker task) and hands back an in-process fan-out stream. Sharing
//! one connection across every subscription — instead of opening a dedicated
//! connection per `subscribe` — keeps `N` subscribers on `M` topics at one Postgres
//! connection rather than `N×M`. The broker awaits `LISTEN` registration before a
//! `subscribe` resolves, so a publish can never race ahead of its own subscription.
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
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{OnceCell, broadcast, mpsc, oneshot};
use tracing::field::Empty;

/// Per-channel broadcast buffer. A subscriber that falls this far behind starts
/// dropping (logged as a lag); ample for realtime fan-out, where clients also
/// reconcile.
const BROADCAST_CAPACITY: usize = 1024;

/// A request to the broker task: start delivering `channel` to a fresh receiver.
struct Register {
    channel: String,
    ack: oneshot::Sender<Result<broadcast::Receiver<Bytes>>>,
}

/// Handle to the shared-listener broker task.
struct Broker {
    cmd_tx: mpsc::UnboundedSender<Register>,
}

/// Postgres-backed [`Pubsub`].
pub(crate) struct PgPubsub {
    pool: PgPool,
    /// Connection string for the shared `LISTEN` connection. A `LISTEN` cannot borrow
    /// a pooled connection for its open-ended lifetime without starving the pool, so
    /// the broker opens its own dedicated connection.
    url: String,
    /// Namespace mixed into the channel name, so apps sharing a database don't
    /// receive each other's messages on the same topic. Empty = no prefix.
    namespace: String,
    /// The broker is started lazily on the first `subscribe`, so a publish-only app
    /// never opens the extra connection.
    broker: OnceCell<Broker>,
}

impl PgPubsub {
    pub(crate) fn new(pool: PgPool, url: String, namespace: String) -> Self {
        Self {
            pool,
            url,
            namespace,
            broker: OnceCell::new(),
        }
    }

    /// Map an arbitrary topic onto a valid Postgres channel name, mixing in the
    /// namespace so two apps' identical topics resolve to different channels.
    fn channel(&self, topic: &str) -> String {
        if self.namespace.is_empty() {
            super::channel_for(topic)
        } else {
            super::channel_for(&format!("{}:{}", self.namespace, topic))
        }
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

    /// Start (once) and return the shared broker.
    async fn broker(&self) -> Result<&Broker> {
        self.broker
            .get_or_try_init(|| async {
                let listener = PgListener::connect(&self.url)
                    .await
                    .map_err(ForgeError::from_sqlx)?;
                let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                tokio::spawn(broker_task(listener, cmd_rx, self.url.clone()));
                Ok::<Broker, ForgeError>(Broker { cmd_tx })
            })
            .await
    }
}

/// The shared-listener loop: own one `PgListener`, register channels on demand, and
/// fan each `NOTIFY` out to that channel's broadcast. Reconnects (re-`LISTEN`ing every
/// active channel) if the connection drops; exits when the last `PgPubsub` is dropped.
async fn broker_task(
    mut listener: PgListener,
    mut cmd_rx: mpsc::UnboundedReceiver<Register>,
    url: String,
) {
    let mut channels: HashMap<String, broadcast::Sender<Bytes>> = HashMap::new();
    loop {
        tokio::select! {
            // Bias toward registrations so a subscribe never starves behind traffic,
            // and so the LISTEN it needs is issued promptly.
            biased;
            cmd = cmd_rx.recv() => {
                let Some(Register { channel, ack }) = cmd else {
                    break; // every PgPubsub handle dropped; nothing can subscribe again
                };
                let tx = match channels.get(&channel) {
                    Some(tx) => tx.clone(),
                    None => {
                        if let Err(e) = listener.listen(&channel).await {
                            let _ = ack.send(Err(ForgeError::from_sqlx(e)));
                            continue;
                        }
                        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
                        channels.insert(channel, tx.clone());
                        tx
                    }
                };
                // The ack carries a receiver and is sent only after LISTEN is active,
                // so the caller's subscribe() resolves with registration guaranteed.
                let _ = ack.send(Ok(tx.subscribe()));
            }
            res = listener.recv() => {
                match res {
                    Ok(note) => {
                        if let Some(tx) = channels.get(note.channel()) {
                            // Fire-and-forget: a send error just means no live receivers.
                            let _ = tx.send(Bytes::copy_from_slice(note.payload().as_bytes()));
                            if tx.receiver_count() == 0 {
                                let ch = note.channel().to_string();
                                let _ = listener.unlisten(&ch).await;
                                channels.remove(&ch);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pubsub listener dropped; reconnecting");
                        listener = reconnect(&url, &channels).await;
                    }
                }
            }
        }
    }
}

/// Reconnect the shared listener and re-`LISTEN` every active channel, retrying with a
/// fixed backoff until it succeeds. Notifications during the gap are lost — pubsub is
/// connected-only / fire-and-forget by contract.
async fn reconnect(url: &str, channels: &HashMap<String, broadcast::Sender<Bytes>>) -> PgListener {
    loop {
        match PgListener::connect(url).await {
            Ok(mut listener) => {
                let mut all_ok = true;
                for ch in channels.keys() {
                    if let Err(e) = listener.listen(ch).await {
                        tracing::warn!(error = %e, channel = %ch, "pubsub re-listen failed");
                        all_ok = false;
                        break;
                    }
                }
                if all_ok {
                    return listener;
                }
            }
            Err(e) => tracing::warn!(error = %e, "pubsub reconnect failed; retrying"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

impl crate::sealed::Sealed for PgPubsub {}

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
            let channel = self.channel(topic);
            sqlx::query!("SELECT pg_notify($1, $2)", channel, text)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription> {
        Self::check_topic(topic)?;
        let channel = self.channel(topic);
        let broker = self.broker().await?;

        let (ack_tx, ack_rx) = oneshot::channel();
        broker
            .cmd_tx
            .send(Register {
                channel,
                ack: ack_tx,
            })
            .map_err(|_| ForgeError::unavailable("pubsub broker has stopped"))?;
        let rx = ack_rx
            .await
            .map_err(|_| ForgeError::unavailable("pubsub broker dropped before acknowledging"))??;

        // subscribe has no completing Result to instrument like the other ops; emit a
        // counter so live subscription counts are still observable.
        metrics::counter!(
            "forge_ops_total",
            "primitive" => "pubsub",
            "op" => "subscribe",
            "outcome" => "ok",
        )
        .increment(1);

        // A lagging subscriber skips dropped messages rather than erroring the stream;
        // the stream ends when the broadcast sender is gone (broker shutdown).
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(payload) => return Some((Ok(payload), rx)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "pubsub subscriber lagged; skipped messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_is_valid_fixed_length_identifier() {
        let c = super::super::channel_for("chat:550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(c.len(), 38); // "forge_" + 32 hex
        assert!(c.starts_with("forge_"));
        assert!(c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        assert!(c.len() <= 63, "must fit a Postgres channel identifier");
    }

    #[test]
    fn distinct_topics_get_distinct_channels() {
        assert_ne!(super::super::channel_for("chat:1"), super::super::channel_for("chat:2"));
        assert_eq!(super::super::channel_for("presence"), super::super::channel_for("presence"));
    }

    #[test]
    fn empty_topic_is_invalid() {
        assert!(matches!(
            PgPubsub::check_topic(""),
            Err(ForgeError::Invalid(_))
        ));
    }
}

//! In-process `pubsub` backend. Contract: docs/contracts/pubsub.md.
//!
//! A `Mutex<HashMap>` of `channel -> tokio::sync::broadcast::Sender`, keyed by the
//! same hashed `<namespace>:<topic>` channel the Postgres backend derives, so
//! namespacing and topic-to-channel mapping are identical. `publish` fans a message
//! out to every receiver currently live on that channel; `subscribe` hands back a
//! stream of payloads published *after* it returns. Delivery is fire-and-forget and
//! connected-only, exactly like [`super::PgPubsub`] — the one difference being that
//! the broadcast never leaves this process. That is the declared caveat: there is no
//! cross-process / cross-replica delivery here, where `LISTEN`/`NOTIFY` would have it.
//!
//! Subscriberless channels are reclaimed lazily on the hot path (a `publish` whose
//! send finds no receivers drops the channel) and in bulk by `maintain`, mirroring the
//! kv backend's lazy-purge-plus-sweep model.

use super::{MAX_PAYLOAD_BYTES, MAX_TOPIC_BYTES, Pubsub, Subscription};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use tokio::sync::broadcast;

/// Per-channel broadcast buffer. A subscriber that falls this far behind starts
/// dropping (surfaced as a logged lag, not a stream error); ample for realtime
/// fan-out, where clients also reconcile. Matches the Postgres backend's capacity.
const BROADCAST_CAPACITY: usize = 1024;

pub(crate) struct MemPubsub {
    channels: Mutex<HashMap<String, broadcast::Sender<Bytes>>>,
    /// Namespace mixed into every channel name, so apps sharing a process namespace
    /// don't receive each other's messages on the same topic. Empty = no prefix.
    namespace: String,
}

impl MemPubsub {
    pub(crate) fn new(namespace: String) -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            namespace,
        }
    }

    /// Take the map lock, recovering the guard if a previous holder panicked. The
    /// critical sections are short and synchronous (a `broadcast::send` does not await),
    /// so a poisoned lock never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, broadcast::Sender<Bytes>>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Map an arbitrary topic onto a channel name, mixing in the namespace so two
    /// apps' identical topics resolve to different channels — the exact scheme the
    /// Postgres backend uses, so a topic resolves the same way in either backend.
    fn channel(&self, topic: &str) -> String {
        super::channel_for(&crate::util::namespaced(&self.namespace, topic))
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

    /// Drop every channel that currently has no subscribers. `publish` already reclaims
    /// a channel the moment it finds it dead; this reclaims those that went idle without
    /// a subsequent publish.
    pub(crate) fn purge_idle(&self) {
        self.lock().retain(|_, tx| tx.receiver_count() > 0);
    }
}

#[async_trait]
impl Pubsub for MemPubsub {
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<()> {
        Self::check_topic(topic)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ForgeError::limit(format!(
                "pubsub payload is {} bytes; max is {MAX_PAYLOAD_BYTES}",
                payload.len()
            )));
        }
        // Payloads are text in the Postgres backend (NOTIFY carries text); reject
        // non-UTF-8 here too so the two backends accept the same inputs.
        if std::str::from_utf8(&payload).is_err() {
            return Err(ForgeError::invalid(
                "pubsub payload must be valid UTF-8 (NOTIFY payloads are text)",
            ));
        }

        let channel = self.channel(topic);
        let mut channels = self.lock();
        // A send error means every receiver is gone; fire-and-forget with zero
        // subscribers is success, so reclaim the dead channel and return Ok.
        let dead = match channels.get(&channel) {
            Some(tx) => tx.send(payload).is_err(),
            None => false,
        };
        if dead {
            channels.remove(&channel);
        }
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription> {
        Self::check_topic(topic)?;
        let channel = self.channel(topic);

        // Subscribe to the channel's sender (creating it on first subscribe). The
        // receiver captures the sender's current position, so only messages published
        // after this point are delivered — matching the connected-only contract.
        let rx = self
            .lock()
            .entry(channel)
            .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
            .subscribe();

        // A lagging subscriber skips dropped messages rather than erroring the stream.
        // The stream ends (`Closed`) only once the sender is gone, which can't happen
        // while this receiver is live (a channel is dropped only with zero receivers) —
        // so in practice it ends when the consumer drops the stream to unsubscribe.
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(payload) => return Some((Ok(payload), rx)),
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "pubsub subscriber lagged; skipped messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        Ok(stream.boxed())
    }
}

#[async_trait]
impl BackendLifecycle for MemPubsub {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Pubsub
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process, no cross-process delivery"
    }
    async fn maintain(&self) -> Result<()> {
        self.purge_idle();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn b(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn publish_delivers_to_active_subscriber() {
        let ps = MemPubsub::new(String::new());
        let mut sub = ps.subscribe("chat").await.unwrap();
        ps.publish("chat", b("hello")).await.unwrap();
        assert_eq!(sub.next().await.unwrap().unwrap(), b("hello"));
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_ok() {
        let ps = MemPubsub::new(String::new());
        // Fire-and-forget: a publish to a topic nobody is listening on still succeeds,
        // and the message is not retained for a future subscriber.
        ps.publish("nobody", b("dropped")).await.unwrap();
        let mut sub = ps.subscribe("nobody").await.unwrap();
        ps.publish("nobody", b("seen")).await.unwrap();
        assert_eq!(sub.next().await.unwrap().unwrap(), b("seen"));
    }

    #[tokio::test]
    async fn only_messages_after_subscribe_are_delivered() {
        let ps = MemPubsub::new(String::new());
        let mut early = ps.subscribe("t").await.unwrap();
        ps.publish("t", b("first")).await.unwrap();
        // `late` subscribes after "first" was published, so it must never see it.
        let mut late = ps.subscribe("t").await.unwrap();
        ps.publish("t", b("second")).await.unwrap();

        assert_eq!(early.next().await.unwrap().unwrap(), b("first"));
        assert_eq!(early.next().await.unwrap().unwrap(), b("second"));
        assert_eq!(
            late.next().await.unwrap().unwrap(),
            b("second"),
            "a late subscriber misses messages published before it subscribed"
        );
    }

    #[tokio::test]
    async fn all_active_subscribers_receive_each_message() {
        let ps = MemPubsub::new(String::new());
        let mut a = ps.subscribe("fanout").await.unwrap();
        let mut bb = ps.subscribe("fanout").await.unwrap();
        ps.publish("fanout", b("broadcast")).await.unwrap();
        assert_eq!(a.next().await.unwrap().unwrap(), b("broadcast"));
        assert_eq!(bb.next().await.unwrap().unwrap(), b("broadcast"));
    }

    #[tokio::test]
    async fn invalid_topic_and_payload_are_rejected() {
        let ps = MemPubsub::new(String::new());
        assert!(matches!(
            ps.publish("", b("x")).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            ps.subscribe("").await.err(),
            Some(ForgeError::Invalid(_))
        ));
        let too_big = Bytes::from(vec![b'x'; MAX_PAYLOAD_BYTES + 1]);
        assert!(matches!(
            ps.publish("t", too_big).await,
            Err(ForgeError::Limit(_))
        ));
        let not_utf8 = Bytes::from(vec![0xff, 0xfe, 0xfd]);
        assert!(matches!(
            ps.publish("t", not_utf8).await,
            Err(ForgeError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn namespaces_isolate_delivery() {
        // The namespace is mixed into the channel, so identical topics in two
        // namespaces resolve to different channels.
        let a = MemPubsub::new("app_a".to_string());
        let bb = MemPubsub::new("app_b".to_string());
        assert_ne!(a.channel("shared"), bb.channel("shared"));
        let none = MemPubsub::new(String::new());
        assert_eq!(none.channel("shared"), super::super::channel_for("shared"));

        // Observably, one app's publish never reaches another app's subscriber.
        let mut sub_b = bb.subscribe("shared").await.unwrap();
        a.publish("shared", b("from-a")).await.unwrap();
        bb.publish("shared", b("from-b")).await.unwrap();
        assert_eq!(
            sub_b.next().await.unwrap().unwrap(),
            b("from-b"),
            "the only message b's subscriber sees is b's own"
        );
    }

    #[tokio::test]
    async fn publish_reclaims_a_subscriberless_channel() {
        let ps = MemPubsub::new(String::new());
        {
            let _sub = ps.subscribe("t").await.unwrap();
            assert_eq!(ps.lock().len(), 1, "subscribe registers the channel");
        }
        // The subscriber is gone but its sender lingers in the map; a publish that finds
        // no receivers reclaims it on the spot rather than leaking it.
        ps.publish("t", b("x")).await.unwrap();
        assert!(
            ps.lock().is_empty(),
            "a publish to a dead channel drops it"
        );
    }

    #[tokio::test]
    async fn purge_idle_drops_only_subscriberless_channels() {
        let ps = MemPubsub::new(String::new());
        let _live = ps.subscribe("live").await.unwrap();
        {
            let _dead = ps.subscribe("dead").await.unwrap();
        }
        assert_eq!(ps.lock().len(), 2, "both channels are registered");
        ps.purge_idle();
        let channels = ps.lock();
        assert!(channels.contains_key(&ps.channel("live")));
        assert!(!channels.contains_key(&ps.channel("dead")));
    }
}

//! Background worker loops. Each loop is idempotent and recovers from transient
//! errors without crashing the process.

use std::time::Duration;

use forge::{Bytes, DequeueOpts, Forge, NackOpts, SetOpts};

use crate::types::{LinkRecord, OwnedLink};
use crate::util::{click_topic, clicks_key, link_slug_key, owner_key, qr_blob_key};

/// Remove all KV and blob state for a link. Idempotent: if the link is already
/// gone, this is a no-op.
pub async fn delete_link(forge: &Forge, slug: &str) {
    let rec: LinkRecord = match forge.kv().get(&link_slug_key(slug)).await {
        Ok(Some(b)) => match serde_json::from_slice(&b) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, slug, "corrupt link record during delete");
                return;
            }
        },
        Ok(None) => return, // already gone
        Err(e) => {
            tracing::warn!(error = %e, slug, "kv get failed during delete");
            return;
        }
    };

    let key = owner_key(&rec.owner_id);
    if let Ok(Some(bytes)) = forge.kv().get(&key).await {
        let mut list: Vec<OwnedLink> = serde_json::from_slice(&bytes).unwrap_or_default();
        list.retain(|l| l.slug != slug);
        if let Ok(data) = serde_json::to_vec(&list) {
            let _ = forge
                .kv()
                .set(&key, Bytes::from(data), SetOpts::new())
                .await;
        }
    }

    let _ = forge.kv().delete(&link_slug_key(slug)).await;
    let _ = forge.kv().delete(&clicks_key(slug)).await;
    let _ = forge.blob().delete(&qr_blob_key(slug)).await;
}

/// Drain the clicks queue: read the current total and publish it to the live
/// dashboard topic. Published payload is JSON `{ slug, clicks }`.
pub async fn run_clicks_worker(forge: Forge) {
    loop {
        let job = match forge
            .queue()
            .dequeue(
                crate::routes::CLICKS_QUEUE,
                DequeueOpts::new()
                    .with_wait(Duration::from_secs(1))
                    .with_visibility_timeout(Duration::from_secs(30)),
            )
            .await
        {
            Ok(Some(j)) => j,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(error = %err, "clicks dequeue error");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        match handle_click(&forge, &job).await {
            Ok(()) => {
                if let Err(err) = forge.queue().ack(&job).await {
                    tracing::warn!(error = %err, "clicks ack failed");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "clicks handler failed");
                let _ = forge.queue().nack(&job, NackOpts::default()).await;
            }
        }
    }
}

async fn handle_click(forge: &Forge, job: &forge::Job) -> anyhow::Result<()> {
    let payload: serde_json::Value = job
        .payload_json()
        .map_err(|e| anyhow::anyhow!("bad clicks payload: {e}"))?;
    let slug = payload["slug"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing slug in payload"))?;

    let total: i64 = match forge.kv().get(&clicks_key(slug)).await? {
        Some(b) => std::str::from_utf8(&b)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        None => 0,
    };

    let msg = serde_json::json!({"slug": slug, "clicks": total});
    forge
        .pubsub()
        .publish(&click_topic(slug), Bytes::from(msg.to_string()))
        .await?;

    Ok(())
}

/// Drain the link-expire queue: delete the named link. Idempotent via
/// `delete_link` so redelivery is harmless.
pub async fn run_expire_worker(forge: Forge) {
    loop {
        let job = match forge
            .queue()
            .dequeue(
                crate::routes::EXPIRE_QUEUE,
                DequeueOpts::new()
                    .with_wait(Duration::from_secs(5))
                    .with_visibility_timeout(Duration::from_secs(30)),
            )
            .await
        {
            Ok(Some(j)) => j,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(error = %err, "expire dequeue error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        match handle_expire(&forge, &job).await {
            Ok(()) => {
                if let Err(err) = forge.queue().ack(&job).await {
                    tracing::warn!(error = %err, "expire ack failed");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "expire handler failed");
                let _ = forge.queue().nack(&job, NackOpts::default()).await;
            }
        }
    }
}

async fn handle_expire(forge: &Forge, job: &forge::Job) -> anyhow::Result<()> {
    let payload: serde_json::Value = job
        .payload_json()
        .map_err(|e| anyhow::anyhow!("bad expire payload: {e}"))?;
    let slug = payload["slug"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing slug in expire payload"))?;
    delete_link(forge, slug).await;
    Ok(())
}

/// Run the scheduler and maintenance sweep every 30 seconds.
pub async fn run_scheduler_loop(forge: Forge) {
    loop {
        if let Err(err) = forge.run_scheduler_once().await {
            tracing::warn!(error = %err, "scheduler tick failed");
        }
        if let Err(err) = forge.maintain().await {
            tracing::warn!(error = %err, "maintenance sweep failed");
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

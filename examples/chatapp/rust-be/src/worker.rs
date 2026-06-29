//! In-process queue workers. All run on Forge's at-least-once queue, so every handler
//! is idempotent on the message id. Safe to run on every replica.

use std::future::Future;
use std::time::Duration;

use forgelib::{Bytes, EnqueueOpts, Job};

use crate::context::{Ctx, FAIL_QUEUE, FANOUT_QUEUE, MessageJob, REAP_QUEUE};
use crate::db;

/// Marks each recipient's receipt delivered. Idempotent: `mark_delivered` flips
/// `delivered_at` at most once, so redelivery is a no-op. Unread is derived from
/// receipts at read time, so there is nothing else to do here.
pub async fn run_fanout(ctx: Ctx, shutdown: impl Future<Output = ()> + Send) {
    let c = ctx.clone();
    ctx.forge
        .worker(FANOUT_QUEUE)
        .concurrency(4)
        .poll_wait(Duration::from_millis(200))
        .run_until(shutdown, move |job: Job| {
            let c = c.clone();
            async move { fanout(c, job).await }
        })
        .await;
}

/// Always errors, so a single-attempt job dead-letters into `fail.dlq`.
pub async fn run_fail(ctx: Ctx, shutdown: impl Future<Output = ()> + Send) {
    ctx.forge
        .worker(FAIL_QUEUE)
        .poll_wait(Duration::from_millis(200))
        .run_until(shutdown, move |_job: Job| async move {
            Err(anyhow::anyhow!("intentional failure (DLQ demo)"))
        })
        .await;
}

/// Reaps a disappearing message when `forge schedule` fires its expiry: delete the
/// row and its blob. Idempotent on an already-gone message.
pub async fn run_reap(ctx: Ctx, shutdown: impl Future<Output = ()> + Send) {
    let c = ctx.clone();
    ctx.forge
        .worker(REAP_QUEUE)
        .poll_wait(Duration::from_millis(200))
        .run_until(shutdown, move |job: Job| {
            let c = c.clone();
            async move { reap(c, job).await }
        })
        .await;
}

async fn fanout(c: Ctx, job: Job) -> anyhow::Result<()> {
    let fan: MessageJob = job
        .payload_json()
        .map_err(|e| anyhow::anyhow!("bad fanout payload: {e}"))?;

    let Some(msg) = db::message(&c.pool, fan.message_id).await? else {
        // Message vanished (e.g. disappeared) before delivery; not an error.
        return Ok(());
    };

    for uid in db::other_member_ids(&c.pool, msg.chat_id, msg.sender_id).await? {
        db::mark_delivered(&c.pool, fan.message_id, uid).await?;
    }
    Ok(())
}

async fn reap(c: Ctx, job: Job) -> anyhow::Result<()> {
    let r: MessageJob = job
        .payload_json()
        .map_err(|e| anyhow::anyhow!("bad reap payload: {e}"))?;
    let Some((media_key, expires_at)) = db::message_reap_info(&c.pool, r.message_id).await? else {
        return Ok(()); // already gone
    };
    // Not due (toggled off / recalled, or not yet expired): leave it alone.
    if expires_at.is_none_or(|e| e > chrono::Utc::now()) {
        return Ok(());
    }
    // Delete the blob before the row, and propagate failure so the at-least-once queue
    // redelivers rather than orphaning the object.
    if let Some(key) = media_key {
        c.forge.blob().delete(&key).await?;
    }
    db::delete_expired_message(&c.pool, r.message_id).await?;
    Ok(())
}

/// Heal work whose post-commit enqueue/schedule was dropped. The app and Forge hold
/// separate pools, so a message commit and its queue/schedule call can't share a tx;
/// a crash between them strands the follow-up. Run once per scheduler tick, bounded.
pub async fn reconcile(c: &Ctx) -> anyhow::Result<()> {
    // Dropped reaps: a due disappearing message whose reap never fired. Delete blob
    // (best-effort here; the bounded sweep re-runs next tick) then the row.
    for (id, media_key) in db::due_messages(&c.pool, 100).await? {
        if let Some(key) = media_key {
            let _ = c.forge.blob().delete(&key).await;
        }
        db::delete_expired_message(&c.pool, id).await?;
    }
    // Dropped fanout: a message past the grace window with never-delivered receipts.
    // Re-enqueue with a dedup id so a still-pending original can't double up; fanout is
    // idempotent on `mark_delivered` regardless.
    for id in db::undelivered_message_ids(&c.pool, 100).await? {
        let payload = serde_json::to_vec(&MessageJob { message_id: id })?;
        c.forge
            .queue()
            .enqueue(
                FANOUT_QUEUE,
                Bytes::from(payload),
                EnqueueOpts::new().with_dedup_id(id.to_string()),
            )
            .await?;
    }
    Ok(())
}

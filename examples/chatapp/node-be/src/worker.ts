import * as db from "./db.ts";
import { type AppCtx, FANOUT_QUEUE, FAIL_QUEUE, REAP_QUEUE, schedulerMs } from "./context.ts";

interface MessageJob {
  message_id: string;
}

type Stopped = () => boolean;

const VISIBILITY = 30;
const WAIT = 1;

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// fanout: marks each recipient's receipt delivered, idempotent on message id.
// Unread now derives from receipts.read_at, so there is no counter to bump.
async function handleFanout(app: AppCtx, payload: string): Promise<void> {
  const { message_id } = JSON.parse(payload) as MessageJob;
  const msg = await db.messageById(app.pool, message_id);
  if (!msg) return; // disappeared before delivery
  const recipients = await db.otherMemberIds(app.pool, msg.chat_id, msg.sender_id);
  for (const uid of recipients) {
    await db.markDelivered(app.pool, message_id, uid);
  }
}

export function runFanoutWorker(app: AppCtx, stopped: Stopped): void {
  void (async () => {
    while (!stopped()) {
      let job;
      try {
        job = await app.forge.queueDequeue(FANOUT_QUEUE, VISIBILITY, WAIT);
      } catch (e) {
        console.warn("fanout dequeue failed:", (e as Error).message);
        await sleep(200);
        continue;
      }
      if (!job) continue;
      try {
        await handleFanout(app, job.payload);
        await app.forge.queueAck(job.id);
      } catch (e) {
        console.warn("fanout handler failed:", (e as Error).message);
        try {
          await app.forge.queueNack(job.id);
        } catch {
          /* redelivery is the queue's job */
        }
      }
    }
  })();
}

// reap: hard-deletes a disappearing message's row + blob when its scheduled job
// fires. Blob goes BEFORE the row, and a blob-delete failure propagates so the
// at-least-once queue redelivers instead of orphaning the object. Idempotent:
// already-gone or recalled (toggled off / not yet due) messages succeed cleanly.
async function reapMessage(app: AppCtx, payload: string): Promise<void> {
  const { message_id } = JSON.parse(payload) as MessageJob;
  const target = await db.reapTarget(app.pool, message_id);
  if (!target) return; // already gone
  // expires_at cleared (toggled off) or in the future (recalled / not yet due).
  if (target.expires_at === null || target.expires_at.getTime() > Date.now()) return;
  if (target.media_key) {
    // Propagate on failure -> nack -> redelivery, rather than orphan the blob.
    await app.forge.blobDelete(target.media_key);
  }
  await db.deleteIfDue(app.pool, message_id);
}

export function runReapWorker(app: AppCtx, stopped: Stopped): void {
  void (async () => {
    while (!stopped()) {
      let job;
      try {
        job = await app.forge.queueDequeue(REAP_QUEUE, VISIBILITY, WAIT);
      } catch (e) {
        console.warn("reap dequeue failed:", (e as Error).message);
        await sleep(200);
        continue;
      }
      if (!job) continue;
      try {
        await reapMessage(app, job.payload);
        await app.forge.queueAck(job.id);
      } catch (e) {
        console.warn("reap handler failed:", (e as Error).message);
        try {
          await app.forge.queueNack(job.id);
        } catch {
          /* ignore */
        }
      }
    }
  })();
}

// fail: always nacks, so a triggered job exhausts its single attempt and
// dead-letters into `fail.dlq` (the opsStats DLQ demo).
export function runFailWorker(app: AppCtx, stopped: Stopped): void {
  void (async () => {
    while (!stopped()) {
      let job;
      try {
        job = await app.forge.queueDequeue(FAIL_QUEUE, VISIBILITY, WAIT);
      } catch (e) {
        console.warn("fail dequeue failed:", (e as Error).message);
        await sleep(200);
        continue;
      }
      if (!job) continue;
      try {
        await app.forge.queueNack(job.id);
      } catch {
        /* ignore; DLQ transition is the contract's job */
      }
    }
  })();
}

// Reconciliation heals work dropped after commit. The app and Forge hold separate
// pools, so the post-commit enqueue/schedule can't share the message's tx; a crash
// in that window leaves a message with no reap or no fanout. Each tick sweeps a
// bounded batch to self-heal both, idempotently.
const RECONCILE_LIMIT = 100;

async function reconcileOnce(app: AppCtx): Promise<void> {
  // Dropped reaps: delete due disappearing messages and their blobs. Blob delete is
  // best-effort here (the scheduled reap is the durable path); the row delete is
  // idempotent on expires_at <= now().
  for (const m of await db.dueMessages(app.pool, RECONCILE_LIMIT)) {
    if (m.media_key) {
      try {
        await app.forge.blobDelete(m.media_key);
      } catch {
        /* best-effort; deleteIfDue still clears the row */
      }
    }
    await db.deleteIfDue(app.pool, m.id);
  }
  // Dropped fanout: re-enqueue fanout for older messages with undelivered receipts.
  // Fanout is idempotent on mark_delivered, so a re-enqueue is always safe.
  for (const id of await db.undeliveredMessageIds(app.pool, RECONCILE_LIMIT)) {
    await app.forge.queueEnqueue(FANOUT_QUEUE, JSON.stringify({ message_id: id }), undefined, id);
  }
}

// scheduler + reconciliation tick: fire due `at` jobs into their queue, then heal
// any work the post-commit enqueue/schedule dropped.
export function runScheduler(app: AppCtx, stopped: Stopped): void {
  const tick = schedulerMs();
  void (async () => {
    while (!stopped()) {
      try {
        await app.forge.runSchedulerOnce();
        await reconcileOnce(app);
        // Sweep expired kv/queue/ratelimit/auth rows (forge-node now exposes maintain).
        await app.forge.maintain();
      } catch (e) {
        console.warn("scheduler tick failed:", (e as Error).message);
      }
      await sleep(tick);
    }
  })();
}

import type { ForgeClient } from "forgelib";

import type { LinkRecord, OwnedLink } from "./types.ts";
import {
  CLICKS_QUEUE,
  EXPIRE_QUEUE,
  clickTopic,
  clicksKey,
  ownerKey,
  qrKey,
  slugKey,
} from "./utils.ts";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// Idempotent: if the slug key is already gone, does nothing.
export async function deleteLink(forge: ForgeClient, slug: string): Promise<void> {
  const raw = await forge.kvGet(slugKey(slug));
  if (!raw) return;

  const rec = JSON.parse(raw) as LinkRecord;
  const rawList = await forge.kvGet(ownerKey(rec.ownerId));
  const list: OwnedLink[] = rawList ? (JSON.parse(rawList) as OwnedLink[]) : [];
  await forge.kvSet(ownerKey(rec.ownerId), JSON.stringify(list.filter((l) => l.slug !== slug)));

  await forge.kvDelete(slugKey(slug));
  await forge.kvDelete(clicksKey(slug));
  await forge.blobDelete(qrKey(slug));
}

// Drains the clicks queue, reads the current count, and publishes it to the
// click topic so SSE subscribers see the updated total.
export function runClicksWorker(forge: ForgeClient): void {
  void (async () => {
    for (;;) {
      let job;
      try {
        job = await forge.queueDequeue(CLICKS_QUEUE, 30, 1);
      } catch (e) {
        console.warn("clicks dequeue failed:", (e as Error).message);
        await sleep(200);
        continue;
      }
      if (!job) continue;
      try {
        const { slug } = JSON.parse(job.payload) as { slug: string };
        const raw = await forge.kvGet(clicksKey(slug));
        const total = raw ? parseInt(raw, 10) : 0;
        await forge.pubsubPublish(clickTopic(slug), JSON.stringify({ slug, clicks: total }));
        await forge.queueAck(job.receipt);
      } catch (e) {
        console.warn("clicks handler failed:", (e as Error).message);
        try {
          await forge.queueNack(job.receipt);
        } catch {
          /* redelivery is the queue's job */
        }
      }
    }
  })();
}

// Drains the expire queue and hard-deletes each link whose scheduled TTL has
// fired. deleteLink is idempotent, so redeliveries are safe.
export function runExpireWorker(forge: ForgeClient): void {
  void (async () => {
    for (;;) {
      let job;
      try {
        job = await forge.queueDequeue(EXPIRE_QUEUE, 30, 5);
      } catch (e) {
        console.warn("expire dequeue failed:", (e as Error).message);
        await sleep(200);
        continue;
      }
      if (!job) continue;
      try {
        const { slug } = JSON.parse(job.payload) as { slug: string };
        await deleteLink(forge, slug);
        await forge.queueAck(job.receipt);
      } catch (e) {
        console.warn("expire handler failed:", (e as Error).message);
        try {
          await forge.queueNack(job.receipt);
        } catch {
          /* ignore */
        }
      }
    }
  })();
}

// Fires due scheduleAt jobs into their queues and runs housekeeping every 30 s.
export function runSchedulerLoop(forge: ForgeClient): void {
  void (async () => {
    for (;;) {
      try {
        await forge.runSchedulerOnce();
        await forge.maintain();
      } catch (e) {
        console.warn("scheduler tick failed:", (e as Error).message);
      }
      await sleep(30_000);
    }
  })();
}

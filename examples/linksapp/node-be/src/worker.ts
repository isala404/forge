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
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// Idempotent: if the slug key is already gone, does nothing.
export async function deleteLink(forge: ForgeClient, slug: string): Promise<void> {
  const rec = await forge.kv<LinkRecord>(slugKey(slug)).get();
  if (!rec) return;

  const ownerList = forge.kv<OwnedLink[]>(ownerKey(rec.ownerId));
  const list = (await ownerList.get()) ?? [];
  await ownerList.set(list.filter((l) => l.slug !== slug));

  await forge.kvDelete(slugKey(slug));
  await forge.kvDelete(clicksKey(slug));
  await forge.blobDelete(qrKey(slug));
}

// Drains the clicks queue, reads the current count, and publishes it to the
// click topic so SSE subscribers see the updated total.
export function runClicksWorker(forge: ForgeClient): void {
  void forge.worker<{ slug: string }>(
    CLICKS_QUEUE,
    async (job) => {
      const { slug } = job.payload;
      const raw = await forge.kvGet(clicksKey(slug));
      const total = raw ? parseInt(raw, 10) : 0;
      await forge.topic<{ slug: string; clicks: number }>(clickTopic(slug))
        .publish({ slug, clicks: total });
    },
    { waitSeconds: 1 },
  );
}

// Drains the expire queue and hard-deletes each link whose scheduled TTL has
// fired. deleteLink is idempotent, so redeliveries are safe.
export function runExpireWorker(forge: ForgeClient): void {
  void forge.worker<{ slug: string }>(
    EXPIRE_QUEUE,
    async (job) => {
      await deleteLink(forge, job.payload.slug);
    },
    { waitSeconds: 5 },
  );
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

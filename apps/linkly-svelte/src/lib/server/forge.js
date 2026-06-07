// Stored on globalThis so dev HMR can't spin up a second client or worker loop.
import { ForgeClient } from 'forge-node';

const PG = process.env.FORGE_POSTGRES_URL || 'postgres://postgres:forge@localhost:5432/forge_dev';

/** @returns {Promise<import('forge-node').ForgeClient>} */
export function getForge() {
  if (!globalThis.__forgeClient) {
    globalThis.__forgeClient = (async () => {
      const forge = await ForgeClient.connect(PG);
      startClickWorker(forge);
      return forge;
    })();
  }
  return globalThis.__forgeClient;
}

function startClickWorker(forge) {
  (async () => {
    for (;;) {
      try {
        // 1s long-poll; null when idle.
        const job = await forge.queueDequeue('clicks', 30, 1);
        if (!job) continue;
        const { code } = JSON.parse(job.payload);
        await forge.kvIncr(`clicks:${code}`, 1);
        await forge.kvIncr('stats:total_clicks', 1);
        await forge.queueAck(job.id);
      } catch (err) {
        console.error('[click-worker]', err);
        await new Promise((r) => setTimeout(r, 500));
      }
    }
  })();
}

/** Base62-encode a counter into a short URL code. */
export function base62(n) {
  const A = '0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ';
  if (n <= 0) return '0';
  let s = '';
  while (n > 0) {
    s = A[n % 62] + s;
    n = Math.floor(n / 62);
  }
  return s;
}

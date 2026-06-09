// A guided tour of every Forge primitive from Node.js, via the forge-node binding.
//
// Mirrors examples/full_tour.rs: signup, session + API key, a feature flag, a rate
// limit, a stored+presigned file, and a one-shot scheduled job — all asserted.
//
// Run it:
//   1. Build the binding:  cd ../../bindings/forge-node && npm install && npm run build
//   2. Install + run:      cd ../../examples/javascript && npm install && node full_tour.js
//   (set FORGE_POSTGRES_URL if your Postgres isn't the docker-compose default)

import assert from 'node:assert/strict';
import { ForgeClient } from 'forge-node';

const PG =
  process.env.FORGE_POSTGRES_URL || 'postgres://postgres:forge@localhost:5432/forge_dev';

const run = `${Date.now().toString(36)}`;
const userId = `user:${run}`;

const forge = await ForgeClient.connect(PG, 'tour-secret-change-me');

// ---- auth: password, session, API key --------------------------------------
const hash = await forge.hashPassword('hunter2-correct-horse');
assert.equal(await forge.verifyPassword('hunter2-correct-horse', hash), true);
assert.equal(await forge.verifyPassword('wrong', hash), false);

const token = await forge.createSession(userId);
assert.equal(await forge.validateSession(token), userId);

const apiKey = await forge.createApiKey(userId, 'cli');
assert.ok(apiKey.secret.startsWith('fk_'));
assert.equal(await forge.verifyApiKey(apiKey.secret), userId);
console.log(`auth: password verified, session + API key (${apiKey.id}) minted`);

// ---- config + flags --------------------------------------------------------
await forge.configSet(`plan:${run}`, 'pro');
assert.equal(await forge.configGet(`plan:${run}`), 'pro');

const flagKey = `new_ui:${run}`;
await forge.setFlagPercent(flagKey, 100);
const on = await forge.flag(flagKey, false, userId);
assert.equal(on, true);
console.log(`config: plan=pro stored; flag ${flagKey} resolved to ${on}`);

// ---- ratelimit: 3 per minute, the 4th throttled ----------------------------
let allowed = 0;
for (let i = 0; i < 4; i++) {
  const d = await forge.rateLimitCheck('login', userId, 3, 60);
  if (d.allowed) allowed++;
}
assert.equal(allowed, 3);
console.log(`ratelimit: ${allowed}/4 login attempts admitted (limit 3/min)`);

// ---- blob: store, read back, presign ---------------------------------------
const key = `exports/${run}/report.csv`;
await forge.blobPut(key, 'hello,world\n1,2\n', 'text/csv');
assert.equal(await forge.blobGet(key), 'hello,world\n1,2\n');
const url = await forge.blobPresignDownload(key, 300);
console.log(`blob: stored + presigned ${url}`);

// ---- schedule: a one-shot due now, fired into the queue --------------------
const queue = `reports_${run}`;
const jobId = await forge.scheduleAt(Date.now(), queue, 'generate-report');
const fired = await forge.runSchedulerOnce();
assert.ok(fired >= 1);
const job = await forge.queueDequeue(queue, 30, 0);
assert.equal(job.id, jobId);
await forge.queueAck(job.id);
console.log(`schedule: one-shot ${jobId} fired and consumed from ${queue}`);

// ---- kv: a counter ---------------------------------------------------------
assert.equal(await forge.kvIncr(`hits:${run}`, 1), 1);

console.log('\nOK — every primitive worked end to end (via the Node binding).');

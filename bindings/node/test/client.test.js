const test = require('node:test');
const assert = require('node:assert/strict');

const { ForgeClient, bytesCodec, configSnapshotFlagDetails, configSnapshotGet, decodeCloudEvent, decodeConfigSnapshot, decodeInvalidationEvent, decodeQueueEnvelope, encodeCloudEvent, encodeConfigSnapshot, encodeInvalidationEvent, encodeQueueEnvelope, exportEnvConfig, importEnvConfig, jsonCodec, forgeErrorCode, forgeErrorRetryable, parseScopedName, runWorker, scopeBlobKey, scopeKvKey, scopeTopic, QueueJob } = require('../client.js');

test('invalidation hints are bounded and discard additive fields', () => {
  const encoded = Buffer.from('{"schema_version":1,"tags":["links"],"query_keys":[["link",{"owner":"u1"}]],"revision":"42","future":true}');
  const decoded = decodeInvalidationEvent(encoded);
  assert.deepEqual(decoded.tags, ['links']);
  assert.equal(encodeInvalidationEvent(decoded).includes(Buffer.from('future')), false);
  assert.throws(() => encodeInvalidationEvent({ schema_version: 1, tags: ['x', 'x'], query_keys: [] }), /unique/);
  assert.throws(() => decodeInvalidationEvent('x'.repeat(4097)), /4096/);
});

test('CloudEvents and environment adapters match canonical vectors', () => {
  const vectors = require('../../../contract/interop-vectors.json');
  const decoded = decodeCloudEvent(Buffer.from(JSON.stringify(vectors.cloud_event.input)));
  assert.equal(decoded.id, vectors.cloud_event.input.id);
  assert.equal(decoded.data.toString('hex'), vectors.cloud_event.data_hex);
  assert.deepEqual(decoded.extensions, vectors.cloud_event.extensions);
  assert.deepEqual(decodeCloudEvent(encodeCloudEvent(decoded)), decoded);
  assert.deepEqual(importEnvConfig(vectors.environment.source, vectors.environment.mappings), vectors.environment.imported);
  assert.deepEqual(exportEnvConfig(vectors.environment.imported, vectors.environment.mappings), vectors.environment.exported);
  assert.throws(() => decodeCloudEvent('{"specversion":"1.0","id":"1","source":"/","type":"x","data":{},"data_base64":"eA=="}'), /mutually exclusive/);
  assert.throws(() => importEnvConfig({ DATABASE_URL: 'one', POSTGRES_URL: 'two' }, vectors.environment.mappings), /conflict/);
});

test('scoped names are primitive-specific and reversible', () => {
  const vectors = require('../../../contract/scope-vectors.json').valid;
  const args = [vectors.application, vectors.tenant, vectors.user, vectors.resource];
  assert.equal(scopeKvKey(...args), vectors.kv);
  assert.equal(scopeBlobKey(...args), vectors.blob);
  assert.deepEqual(parseScopedName(vectors.kv), { kind: 'kv', application: vectors.application, tenant: vectors.tenant, user: vectors.user, resource: vectors.resource });
  assert.throws(() => scopeTopic('app', '', 'u', 'r'), (error) => error.code === 'INVALID' && error.retryable === false && /1 to 255/.test(error.message));
  assert.throws(() => scopeKvKey('a'.repeat(100), 't'.repeat(100), 'u'.repeat(100), 'r'.repeat(100)), (error) => error.code === 'LIMIT');
  assert.throws(() => parseScopedName('v1|kv|+7:billing3:a:b3:u/19:invoice:7'), (error) => error.code === 'INVALID' && /length/.test(error.message));
});

test('memory lifecycle uses the canonical string config', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  assert.equal(await forge.kvSet('key', 'value'), true);
  assert.equal(await forge.kvGet('key'), 'value');
  assert.throws(() => forge.postgresUrl(), (error) => {
    assert.equal(error.code, 'NOT_CONFIGURED');
    assert.equal(error.retryable, false);
    assert.equal(error.operation, 'unknown');
    assert.equal(error.safeMessage, error.message);
    return true;
  });
  await forge.close(1);
  await forge.close(1);
  await assert.rejects(forge.kvGet('after-close'), (error) => {
    assert.equal(error.code, 'PRECONDITION');
    assert.equal(error.retryable, false);
    assert.equal(error.operation, 'lifecycle.reject_new_work');
    return true;
  });
});

test('static migration errors keep the structured Forge contract', async () => {
  const config = '[forge]\nmode = "memory"\nenvironment = "test"\n';
  for (const operation of [
    () => ForgeClient.migrateFromString(config),
    () => ForgeClient.migrationStatusFromString(config),
    () => ForgeClient.validateSchemaFromString(config),
  ]) {
    await assert.rejects(operation(), (error) => {
      assert.equal(error.name, 'ForgeError');
      assert.equal(error.code, 'NOT_CONFIGURED');
      assert.equal(error.retryable, false);
      assert.equal(error.safeMessage, error.message);
      return true;
    });
  }
});

test('memory test factory advances time without sleeping', async () => {
  const toml = '[forge]\nmode = "memory"\nenvironment = "test"\n';
  const first = await ForgeClient.initMemoryForTesting(toml, 1_700_000_000_000, 42);
  const second = await ForgeClient.initMemoryForTesting(toml, 1_700_000_000_000, 42);
  assert.equal(await first.kvSet('ttl', 'value', 10), true);
  const a = await first.createToken('user', 'test', 60);
  const b = await second.createToken('user', 'test', 60);
  assert.equal(a, b);
  first.advanceTestClock(10);
  assert.equal(await first.kvGet('ttl'), null);
  await first.close(1);
  await second.close(1);
});

test('bulk config and read-only snapshots preserve order and expiry', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  await forge.configSet('color', 'blue');
  await forge.setFlagValue('theme', '"dark"', 'theme-v1');
  const values = await forge.configGetMany(['missing', 'color', 'color']);
  assert.deepEqual(values.map((entry) => entry.value), [undefined, 'blue', 'blue']);
  const requests = [{ id: 'theme-user', key: 'theme', defaultJson: '"light"', targetingKey: 'user-1', contextJson: '{"tenant":"acme"}' }];
  const details = await forge.flagDetailsMany(requests);
  assert.equal(details[0].evaluation.valueJson, '"dark"');
  assert.equal(details[0].evaluation.variant, 'theme-v1');
  const snapshot = await forge.configSnapshot(['color'], requests, 60, 'no_secrets');
  assert.equal(Object.isFrozen(snapshot), true);
  const decoded = decodeConfigSnapshot(encodeConfigSnapshot(snapshot));
  assert.equal(configSnapshotGet(decoded, 'color', decoded.createdAtMs), 'blue');
  assert.equal(configSnapshotFlagDetails(decoded, 'theme-user', decoded.createdAtMs).valueJson, '"dark"');
  assert.throws(() => configSnapshotGet(decoded, 'missing', decoded.createdAtMs), /not included/);
  assert.throws(() => configSnapshotGet(decoded, 'color', decoded.expiresAtMs + 1), /stale/);
  await forge.close(1);
});

test('scheduler controls expose bounded catch-up and diagnostics', async () => {
  const toml = '[forge]\nmode = "memory"\nenvironment = "test"\n';
  const forge = await ForgeClient.initMemoryForTesting(toml, 1_700_000_000_000, 15);
  await forge.scheduleCron('minute', '* * * * *', 'jobs', 'x', null, 'catch_up', 3);
  assert.equal(await forge.schedulePause('minute'), true);
  forge.advanceTestClock(20 * 60);
  assert.equal((await forge.schedulerDiagnostics()).dueCount, 0);
  const paused = await forge.scheduleInspect('minute');
  assert.equal(paused.paused, true);
  assert.equal(paused.misfirePolicy, 'catch_up');
  assert.equal(paused.maxCatchUp, 3);
  assert.equal(await forge.scheduleResume('minute'), true);
  assert.equal(await forge.runSchedulerOnce(), 3);
  const diagnostics = await forge.schedulerDiagnostics();
  assert.equal(diagnostics.dueCount, 0);
  assert.ok(diagnostics.lastSuccessfulTickMs > 0);
  await forge.close(1);
});

test('auth metadata, opaque payloads, rehash, and scoped names stay structured', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  const hash = await forge.hashPassword('correct horse battery staple');
  assert.equal(forge.needsRehash(hash), false);
  assert.equal(forge.needsRehash('malformed'), true);

  const key = await forge.createApiKeyWith('owner-1', 'ci', 60, ['deploy'], { env: 'test' });
  const info = await forge.verifyApiKey(key.secret);
  assert.deepEqual(info.scopes, ['deploy']);
  assert.deepEqual(info.metadata, { env: 'test' });
  assert.ok(info.expiresAtMs > Date.now());

  const token = await forge.createToken('user-1', 'reset', 60, Buffer.from('return=/settings'));
  const consumed = await forge.consumeToken(token, 'reset');
  assert.equal(consumed.userId, 'user-1');
  assert.equal(Buffer.from(consumed.payload).toString(), 'return=/settings');
  assert.equal(await forge.consumeToken(token, 'reset'), null);

  assert.equal(scopeKvKey('billing', 'a:b', 'u/1', 'invoice:7'), 'v1|kv|7:billing3:a:b3:u/19:invoice:7');
  await forge.close(1);
});

test('blob conditions, copy headers, and SHA-256 verification compose', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  const checksum = '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';
  await forge.blobPutObject('source', Buffer.from('hello'), {
    contentType: 'text/plain',
    metadata: { purpose: 'test' },
    cacheControl: 'public, max-age=60',
    contentDisposition: 'attachment; filename="hello.txt"',
    checksumSha256: checksum,
  });
  const info = await forge.blobHead('source');
  assert.equal(info.checksumSha256, checksum);
  assert.equal((await forge.blobGetIf('source', info.etag)).state, 'found');
  const notModified = await forge.blobGetIf('source', undefined, info.etag);
  assert.equal(notModified.state, 'not_modified');
  assert.equal(notModified.body, undefined);
  await assert.rejects(forge.blobGetIf('source', 'wrong'), (error) => error.code === 'PRECONDITION');
  const copied = await forge.blobCopy('source', 'copy');
  assert.equal(copied.cacheControl, 'public, max-age=60');
  assert.equal(copied.contentDisposition, 'attachment; filename="hello.txt"');
  assert.equal(await forge.blobVerifyChecksumSha256('copy', checksum), true);
  await assert.rejects(forge.blobCreateMultipart('large'), (error) => error.code === 'NOT_CONFIGURED');
  await forge.close(1);
});

test('close cancels an in-flight JavaScript worker and releases its lease', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  await forge.queue('jobs').enqueue({ id: 1 });
  let markStarted;
  const started = new Promise((resolve) => {
    markStarted = resolve;
  });
  let cancelled = false;
  const worker = forge.worker('jobs', async (job) => {
    markStarted();
    await new Promise((resolve) => {
      job.signal.addEventListener('abort', resolve, { once: true });
    });
    cancelled = true;
  }, { waitSeconds: 0 });

  await started;
  await forge.close(1);
  await worker;
  assert.equal(cancelled, true);
});

test('close cancels an in-flight outbox relay', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  const relay = forge.runOutboxRelay({ retryBackoffSeconds: 0.001 });
  await new Promise((resolve) => setTimeout(resolve, 5));
  await forge.close(1);
  await relay;
});

test('managed worker runs up to its configured concurrency', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  for (let id = 0; id < 3; id += 1) await forge.queue('parallel').enqueue({ id });
  const controller = new AbortController();
  let active = 0;
  let peak = 0;
  let started = 0;
  let releaseHandlers;
  let markStarted;
  const released = new Promise((resolve) => { releaseHandlers = resolve; });
  const allStarted = new Promise((resolve) => { markStarted = resolve; });
  const worker = forge.worker('parallel', async () => {
    active += 1;
    peak = Math.max(peak, active);
    started += 1;
    if (started === 3) markStarted();
    await released;
    active -= 1;
  }, {
    concurrency: 3,
    visibilitySeconds: 1,
    heartbeatSeconds: 0.1,
    waitSeconds: 0,
    signal: controller.signal,
  });

  await allStarted;
  assert.equal(peak, 3);
  releaseHandlers();
  controller.abort();
  await worker;
  assert.equal((await forge.queue('parallel').depth()).visible, 0);
  await forge.close(1);
});

test('deterministic ids, dedup release, and dead-letter operators compose', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  const queue = forge.queue('operator');
  const jobId = '11111111-1111-4111-8111-111111111111';
  assert.equal(await queue.enqueue({ value: 1 }, { jobId, dedupId: 'content', maxAttempts: 1 }), jobId);
  assert.equal(await queue.enqueue({ ignored: true }, { jobId }), jobId);
  const job = await queue.dequeue({ visibilitySeconds: 1, waitSeconds: 0 });
  await job.nack({ retrySeconds: 0, failureSummary: 'safe failure' });
  const replacement = await queue.enqueue({ value: 2 }, { dedupId: 'content' });
  assert.notEqual(replacement, jobId);
  const page = await queue.deadLetters({ limit: 10 });
  assert.equal(page.items[0].jobId, jobId);
  assert.equal(page.items[0].failureSummary, 'safe failure');
  assert.equal(await queue.redrive(jobId, { destination: 'recovered', dedupPolicy: 'clear' }), true);
  assert.equal((await forge.queue('recovered').dequeue({ waitSeconds: 0 })).id, jobId);
  await forge.close(1);
});

test('long-running queue controls, raw envelopes, and weighted reservations compose', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  const queue = forge.queue('long', bytesCodec);
  const firstId = await queue.enqueue(Buffer.from([0, 255]), { priority: 'high', concurrencyKey: 'tenant-a' });
  await queue.enqueue(Buffer.from('blocked'), { priority: 'high', concurrencyKey: 'tenant-a' });
  const otherId = await queue.enqueue(Buffer.from('other'), { concurrencyKey: 'tenant-b' });
  const first = await queue.dequeue({ waitSeconds: 0, concurrencyLimitPerKey: 1 });
  const other = await queue.dequeue({ waitSeconds: 0, concurrencyLimitPerKey: 1 });
  assert.equal(first.id, firstId);
  assert.equal(other.id, otherId);
  assert.equal((await queue.cancel(firstId)).state, 'cancel_requested');
  assert.equal(await forge.queueCancellationRequested(first.receipt), true);
  await forge.queueFinishCancellation(first.receipt);
  await other.ack();
  assert.equal((await queue.status(firstId)).state, 'cancelled');

  const encoded = encodeQueueEnvelope({ schema: 'example.task.v1', contentType: 'application/octet-stream', body: Buffer.from([0, 255]), artifacts: [{ uri: 'blob://generated/result' }] });
  assert.deepEqual(decodeQueueEnvelope(encoded).body, Buffer.from([0, 255]));
  assert.throws(() => encodeQueueEnvelope({ schema: 'v1', contentType: 'application/octet-stream', body: Buffer.alloc(0), artifacts: [{ uri: '' }] }), TypeError);
  assert.throws(() => decodeQueueEnvelope(Buffer.from('{"version":1,"schema":"v1","content_type":"application/octet-stream","body":[300]}')), TypeError);

  const reservation = await forge.reserveRateLimit('tokens', 'tenant', { max: 10, perSeconds: 3600, cost: 5, ttlSeconds: 60 });
  assert.ok(reservation);
  const committed = await forge.commitRateLimit(reservation.id, 2);
  assert.equal(committed.committedUnits, 2);
  assert.deepEqual(await forge.commitRateLimit(reservation.id, 2), committed);
  const decision = await forge.rateLimitCheck('tokens', 'tenant', 10, 3600, false, undefined, 8);
  assert.equal(decision.allowed, true);
  assert.equal(decision.remaining, 0);
  await forge.close(1);
});

test('batch queue operators and diagnostics stay typed', async () => {
  const forge = await ForgeClient.initFromString('[forge]\nmode = "memory"\nenvironment = "test"\n');
  const queue = forge.queue('operator-batch', bytesCodec);
  const results = await queue.enqueueBatch([
    { payload: Buffer.from('one'), jobId: '11111111-1111-4111-8111-111111111111' },
    { payload: Buffer.from('two') },
  ]);
  assert.equal(results.length, 2);
  assert.equal(results[0].jobId, '11111111-1111-4111-8111-111111111111');
  assert.equal(results[0].errorCode, undefined);
  await queue.pause();
  assert.equal(await queue.isPaused(), true);
  assert.equal(await queue.dequeue({ waitSeconds: 0 }), null);
  await queue.resume();
  const jobs = await queue.dequeueBatch(10, { waitSeconds: 0 });
  assert.equal(jobs.length, 2);
  await Promise.all(jobs.map((job) => job.ack()));
  const stats = await queue.stats();
  assert.equal(stats.enqueuedTotal, 2);
  assert.equal(stats.settledTotal, 2);
  const diagnostics = await forge.diagnostics(1);
  assert.equal(diagnostics.ready, true);
  assert.ok(diagnostics.checks.some((check) => check.name === 'backend_reachability'));
  await forge.close();
});

test('backend package operators cover lifecycle, auth, kv, pubsub, and batch redrive', async () => {
  const config = '[forge]\nmode = "memory"\nenvironment = "test"\nnamespace = "package-test"\n';
  const forge = await ForgeClient.initMemoryForTesting(config, 1_700_000_000_000, 27);

  assert.equal(forge.isLive(), true);
  assert.equal(forge.backendCapabilities().length, 8);
  assert.equal(forge.pubsubChannel('updates'), forge.pubsubChannel('updates'));

  await forge.kvSet('first', 'one');
  await forge.kvSet('second', 'two');
  assert.deepEqual(await forge.kvMget(['second', 'missing', 'first']), ['two', null, 'one']);
  assert.equal(await forge.kvExpire('first', 1), true);
  forge.advanceTestClock(1);
  await forge.maintain();
  assert.equal(await forge.kvGet('first'), null);

  const firstSession = await forge.createSession('owner');
  const secondSession = await forge.createSession('owner');
  assert.equal(await forge.revokeAllSessions('owner'), 2);
  assert.equal(await forge.validateSession(firstSession), null);
  assert.equal(await forge.validateSession(secondSession), null);
  const apiKey = await forge.createApiKey('owner', 'test');
  assert.equal(await forge.revokeApiKey(apiKey.id), true);
  assert.equal(await forge.verifyApiKey(apiKey.secret), null);

  const queue = forge.queue('batch-redrive', bytesCodec);
  for (const payload of ['one', 'two']) {
    await queue.enqueue(Buffer.from(payload), { maxAttempts: 1 });
    const job = await queue.dequeue({ waitSeconds: 0 });
    await job.nack({ retrySeconds: 0, failureSummary: 'safe' });
  }
  const statuses = await forge.queue('batch-redrive.dlq').statuses({ limit: 10 });
  assert.equal(statuses.items.length, 2);
  assert.ok(statuses.items.every((item) => item.state === 'queued'));
  const redriven = await queue.redriveBatch({ destination: 'recovered', dedupPolicy: 'clear', limit: 10 });
  assert.equal(redriven.redriven, 2);
  assert.equal((await forge.queue('recovered').depth()).visible, 2);

  await assert.rejects(forge.runOutboxOnce(), (error) => error.code === 'NOT_CONFIGURED');
  const probe = await forge.probe(1);
  assert.equal(probe.ready, true);
  assert.ok(forge.metricsSnapshot().some((metric) => metric.name === 'forge_operations_total'));
  assert.match(forge.renderPrometheus(), /forge_operations_total/);

  await forge.close();
  assert.equal(forge.isLive(), false);
});

test('jsonCodec round-trips objects', () => {
  const value = { a: 1, b: ['x', null], c: 'text' };
  assert.deepEqual(jsonCodec.decode(jsonCodec.encode(value)), value);
});

test('jsonCodec is strict JSON, matching the Python binding', () => {
  // A bare string is stored quoted, so json.loads on the Python side accepts it.
  assert.equal(jsonCodec.encode('hello'), '"hello"');
  assert.equal(jsonCodec.decode('"hello"'), 'hello');
  assert.deepEqual(jsonCodec.decode(Buffer.from('{"n":2}')), { n: 2 });
  assert.throws(() => jsonCodec.decode('not json'));
});

test('forgeErrorCode parses the message prefix, not error.code', () => {
  const e = new Error('NOT_FOUND: no such key');
  e.code = 'GenericFailure'; // what napi actually sets
  assert.equal(forgeErrorCode(e), 'NOT_FOUND');
  assert.equal(forgeErrorCode(new Error('UNAVAILABLE: pool timed out')), 'UNAVAILABLE');
  assert.equal(forgeErrorCode(new Error('plain failure')), undefined);
  assert.equal(forgeErrorCode(new Error('SOMETHING_ELSE: nope')), undefined);
  assert.equal(forgeErrorCode('CONFIG: bad toml'), 'CONFIG');
  assert.equal(forgeErrorCode(null), undefined);
});

test('forgeErrorCode strips the retryable marker from backend errors', () => {
  assert.equal(forgeErrorCode(new Error('BACKEND(retryable): deadlock detected')), 'BACKEND');
  assert.equal(forgeErrorCode(new Error('BACKEND: broke')), 'BACKEND');
  // The marker only counts on a known code.
  assert.equal(forgeErrorCode(new Error('WEIRD(retryable): nope')), undefined);
});

test('forgeErrorRetryable honors UNAVAILABLE and the backend retryable marker', () => {
  assert.equal(forgeErrorRetryable(new Error('UNAVAILABLE: down')), true);
  assert.equal(forgeErrorRetryable(new Error('BACKEND(retryable): deadlock detected')), true);
  assert.equal(forgeErrorRetryable(new Error('BACKEND: broke')), false);
  assert.equal(forgeErrorRetryable(new Error('WEIRD(retryable): nope')), false);
  assert.equal(forgeErrorRetryable(new Error('nope')), false);
});

function rawJob(payload, receipt = 'r1') {
  return {
    id: 'j1',
    receipt,
    payload: JSON.stringify(payload),
    attempt: 1,
    maxAttempts: 5,
    leasedUntilMs: Date.now() + 30000,
    queue: 'q',
  };
}

// A queue stub: serves the given raw jobs one per dequeue, then aborts the loop.
function stubClient(jobs, controller) {
  const calls = { acked: [], nacked: [], heartbeats: 0, dequeues: 0 };
  return {
    calls,
    async queueDequeue() {
      calls.dequeues += 1;
      if (jobs.length === 0) {
        controller.abort();
        return null;
      }
      return jobs.shift();
    },
    async queueAck(receipt) {
      calls.acked.push(receipt);
    },
    async queueNack(receipt, retrySeconds) {
      calls.nacked.push({ receipt, retrySeconds });
    },
    async queueHeartbeat() {
      calls.heartbeats += 1;
    },
  };
}

test('runWorker acks a handled job', async () => {
  const controller = new AbortController();
  const client = stubClient([rawJob({ n: 1 })], controller);
  const seen = [];
  await runWorker(client, 'q', async (job) => {
    assert.ok(job instanceof QueueJob);
    seen.push(job.payload);
  }, { signal: controller.signal, waitSeconds: 0 });
  assert.deepEqual(seen, [{ n: 1 }]);
  assert.deepEqual(client.calls.acked, ['r1']);
  assert.deepEqual(client.calls.nacked, []);
});

test('runWorker releases a job returned after shutdown during long-poll', async () => {
  const controller = new AbortController();
  const raw = rawJob({ n: 1 });
  const calls = { nacked: [], handled: 0 };
  const client = {
    async queueDequeue() {
      controller.abort();
      return raw;
    },
    async queueNack(receipt, retrySeconds) {
      calls.nacked.push({ receipt, retrySeconds });
    },
  };

  await runWorker(client, 'q', async () => {
    calls.handled += 1;
  }, { signal: controller.signal });

  assert.equal(calls.handled, 0);
  assert.deepEqual(calls.nacked, [{ receipt: 'r1', retrySeconds: 0 }]);
});

test('runWorker nacks and reports a failing handler', async () => {
  const controller = new AbortController();
  const client = stubClient([rawJob({ n: 1 })], controller);
  const reported = [];
  await runWorker(client, 'q', async () => {
    throw new Error('boom');
  }, {
    signal: controller.signal,
    retrySeconds: 7,
    onError: (error, job) => {
      reported.push({ message: error.message, receipt: job?.receipt });
    },
  });
  assert.deepEqual(client.calls.acked, []);
  assert.deepEqual(client.calls.nacked, [{ receipt: 'r1', retrySeconds: 7 }]);
  assert.deepEqual(reported, [{ message: 'boom', receipt: 'r1' }]);
});

test('runWorker nacks an undecodable payload', async () => {
  const controller = new AbortController();
  const raw = rawJob({});
  raw.payload = 'not json';
  const client = stubClient([raw], controller);
  const reported = [];
  await runWorker(client, 'q', async () => {
    assert.fail('handler must not run');
  }, {
    signal: controller.signal,
    onError: (error, job) => reported.push({ job }),
  });
  assert.equal(client.calls.nacked.length, 1);
  assert.deepEqual(reported, [{ job: undefined }]);
});

test('runWorker survives dequeue errors with backoff', async () => {
  const controller = new AbortController();
  let failures = 0;
  const reported = [];
  const client = {
    async queueDequeue() {
      if (failures < 2) {
        failures += 1;
        throw new Error('UNAVAILABLE: db blip');
      }
      controller.abort();
      return null;
    },
  };
  await runWorker(client, 'q', async () => {}, {
    signal: controller.signal,
    onError: (error) => reported.push(forgeErrorCode(error)),
  });
  assert.equal(failures, 2);
  assert.deepEqual(reported, ['UNAVAILABLE', 'UNAVAILABLE']);
});

test('runWorker stops on non-retryable dequeue errors', async () => {
  let dequeues = 0;
  const client = {
    async queueDequeue() {
      dequeues += 1;
      const error = new Error('bad queue');
      error.code = 'INVALID';
      error.retryable = false;
      throw error;
    },
  };
  await assert.rejects(runWorker(client, 'q', async () => {}), (error) => {
    assert.equal(error.code, 'INVALID');
    return true;
  });
  assert.equal(dequeues, 1);
});

test('runWorker heartbeats long-running handlers and honors a lost lease', async () => {
  const controller = new AbortController();
  const raw = rawJob({ n: 1 });
  let heartbeats = 0;
  const client = {
    async queueDequeue() {
      if (heartbeats === -1) throw new Error('done');
      if (raw.taken) {
        controller.abort();
        return null;
      }
      raw.taken = true;
      return raw;
    },
    async queueAck() {
      assert.fail('must not ack after the lease is lost');
    },
    async queueNack() {
      assert.fail('must not nack after the lease is lost');
    },
    async queueHeartbeat() {
      heartbeats += 1;
      throw new Error('PRECONDITION: unknown receipt: the lease was lost');
    },
  };
  let sawLeaseLost = false;
  await runWorker(client, 'q', async (job) => {
    await new Promise((resolve) => job.signal.addEventListener('abort', resolve, { once: true }));
    sawLeaseLost = job.leaseLost;
  }, { signal: controller.signal, visibilitySeconds: 0.03 });
  assert.ok(heartbeats >= 1);
  assert.equal(sawLeaseLost, true);
});

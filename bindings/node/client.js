const native = require('./index.js');

// Strict JSON on both sides, matching the Python binding's json.dumps/json.loads
// defaults: a value written by one language must decode identically in the other.
const jsonCodec = {
  encode(value) {
    return JSON.stringify(value);
  },
  decode(value) {
    const text = Buffer.isBuffer(value) ? value.toString('utf8') : value;
    return JSON.parse(text);
  },
};

const bytesCodec = {
  encode(value) { return Buffer.from(value); },
  decode(value) { return Buffer.from(value); },
};

const INVALIDATION_MAX_BYTES = 4096;
const CONFIG_SNAPSHOT_MAX_BYTES = 1024 * 1024;
const CLOUD_EVENT_MAX_BYTES = 1024 * 1024;
const CLOUD_EVENT_RESERVED = new Set(['specversion', 'id', 'source', 'type', 'datacontenttype', 'dataschema', 'subject', 'time', 'data', 'data_base64', 'dataref', 'dataref_base64']);

function validateConfigSnapshot(snapshot) {
  if (!snapshot || typeof snapshot !== 'object' || snapshot.schemaVersion !== 1) throw new TypeError('unsupported config snapshot schema version');
  if (!Number.isFinite(snapshot.createdAtMs) || !Number.isFinite(snapshot.expiresAtMs) || snapshot.expiresAtMs < snapshot.createdAtMs || snapshot.expiresAtMs - snapshot.createdAtMs > 86_400_000) throw new TypeError('config snapshot staleness is invalid');
  if (snapshot.secretHandling !== 'no_secrets' && snapshot.secretHandling !== 'application_protected') throw new TypeError('config snapshot secret handling is invalid');
  if (!Array.isArray(snapshot.config) || !Array.isArray(snapshot.flags) || snapshot.config.length > 256 || snapshot.flags.length > 256) throw new TypeError('config snapshot entries are invalid');
  if (new Set(snapshot.config.map(entry => entry.key)).size !== snapshot.config.length || new Set(snapshot.flags.map(entry => entry.id)).size !== snapshot.flags.length) throw new TypeError('config snapshot identifiers must be unique');
  return snapshot;
}

function freezeConfigSnapshot(snapshot) {
  validateConfigSnapshot(snapshot);
  for (const entry of snapshot.config) Object.freeze(entry);
  for (const entry of snapshot.flags) {
    Object.freeze(entry.evaluation);
    Object.freeze(entry);
  }
  Object.freeze(snapshot.config);
  Object.freeze(snapshot.flags);
  return Object.freeze(snapshot);
}

function encodeConfigSnapshot(snapshot) {
  const encoded = Buffer.from(JSON.stringify(validateConfigSnapshot(snapshot)));
  if (encoded.length > CONFIG_SNAPSHOT_MAX_BYTES) throw new TypeError('config snapshot exceeds 1 MiB');
  return encoded;
}

function decodeConfigSnapshot(input) {
  const encoded = Buffer.isBuffer(input) ? input : Buffer.from(input);
  if (encoded.length > CONFIG_SNAPSHOT_MAX_BYTES) throw new TypeError('config snapshot exceeds 1 MiB');
  let snapshot;
  try { snapshot = JSON.parse(encoded.toString('utf8')); } catch { throw new TypeError('config snapshot must be valid JSON'); }
  return freezeConfigSnapshot(snapshot);
}

function configSnapshotGet(snapshot, key, nowMs = Date.now()) {
  validateConfigSnapshot(snapshot);
  if (nowMs > snapshot.expiresAtMs) throw new TypeError('config snapshot is stale');
  const entry = snapshot.config.find(item => item.key === key);
  if (!entry) throw new TypeError('config key was not included in the snapshot');
  return entry.value ?? null;
}

function configSnapshotFlagDetails(snapshot, id, nowMs = Date.now()) {
  validateConfigSnapshot(snapshot);
  if (nowMs > snapshot.expiresAtMs) throw new TypeError('config snapshot is stale');
  const entry = snapshot.flags.find(item => item.id === id);
  if (!entry) throw new TypeError('flag request id was not included in the snapshot');
  return entry.evaluation;
}

function validateInvalidationValue(value, depth, count) {
  count.nodes += 1;
  if (count.nodes > 32) throw new TypeError('query-key fragment has too many nodes');
  if (value == null || typeof value === 'boolean' || (typeof value === 'number' && Number.isFinite(value))) return;
  if (typeof value === 'string') {
    if (Buffer.byteLength(value) > 128) throw new TypeError('query-key string exceeds 128 bytes');
    return;
  }
  if (typeof value !== 'object') throw new TypeError('query-key parts must be JSON values');
  if (depth >= 3) throw new TypeError('query-key nesting exceeds 3 levels');
  const entries = Array.isArray(value) ? value.map((item, index) => [String(index), item]) : Object.entries(value);
  if (entries.length > 16) throw new TypeError('query-key container has too many items');
  for (const [key, item] of entries) {
    if (!Array.isArray(value) && Buffer.byteLength(key) > 64) throw new TypeError('query-key object key exceeds 64 bytes');
    validateInvalidationValue(item, depth + 1, count);
  }
}

function decodeInvalidationEvent(input) {
  const encoded = Buffer.isBuffer(input) ? input.toString('utf8') : input;
  if (Buffer.byteLength(encoded) > INVALIDATION_MAX_BYTES) throw new TypeError('invalidation event exceeds 4096 bytes');
  let value;
  try { value = JSON.parse(encoded); } catch { throw new TypeError('invalidation event must be valid JSON'); }
  return validateInvalidationEvent(value);
}

function validateInvalidationEvent(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || value.schema_version !== 1) throw new TypeError('unsupported invalidation schema version');
  const tags = value.tags ?? [];
  const queryKeys = value.query_keys ?? [];
  if (!Array.isArray(tags) || !Array.isArray(queryKeys) || (tags.length === 0 && queryKeys.length === 0)) throw new TypeError('invalidation event requires a target');
  if (tags.length > 32 || queryKeys.length > 32 || tags.length + queryKeys.length > 64) throw new TypeError('invalidation event has too many targets');
  const unique = new Set();
  for (const tag of tags) {
    if (typeof tag !== 'string' || Buffer.byteLength(tag) === 0 || Buffer.byteLength(tag) > 128) throw new TypeError('invalid invalidation tag');
    if (unique.has(tag)) throw new TypeError('invalidation tags must be unique');
    unique.add(tag);
  }
  for (const key of queryKeys) {
    if (!Array.isArray(key) || key.length === 0 || key.length > 8) throw new TypeError('query-key fragments must contain 1..=8 parts');
    const count = { nodes: 0 };
    for (const part of key) validateInvalidationValue(part, 1, count);
  }
  if (value.revision !== undefined && (typeof value.revision !== 'string' || Buffer.byteLength(value.revision) === 0 || Buffer.byteLength(value.revision) > 256)) throw new TypeError('invalid invalidation revision');
  const normalized = { schema_version: 1, tags: [...tags], query_keys: queryKeys.map((key) => structuredClone(key)), ...(value.revision === undefined ? {} : { revision: value.revision }) };
  if (Buffer.byteLength(JSON.stringify(normalized)) > INVALIDATION_MAX_BYTES) throw new TypeError('invalidation event exceeds 4096 bytes');
  return normalized;
}

function encodeInvalidationEvent(event) {
  return Buffer.from(JSON.stringify(validateInvalidationEvent(event)));
}

function validCloudEventString(value) {
  return typeof value === 'string' && value.length > 0 && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function validateCloudEventExtension(name, value) {
  if (!/^[a-z0-9]+$/.test(name) || CLOUD_EVENT_RESERVED.has(name)) throw new TypeError('CloudEvents extension names must be lowercase alphanumeric and non-reserved');
  if (value == null || typeof value === 'boolean' || typeof value === 'string') return;
  if (typeof value === 'number' && Number.isInteger(value) && value >= -2147483648 && value <= 2147483647) return;
  throw new TypeError('CloudEvents extension values must be null, boolean, 32-bit integer, or string');
}

function isJsonContentType(contentType) {
  if (contentType == null) return true;
  const mediaType = contentType.split(';', 1)[0].trim().toLowerCase();
  const slash = mediaType.indexOf('/');
  if (slash < 0) return false;
  const subtype = mediaType.slice(slash + 1);
  return subtype === 'json' || subtype.endsWith('+json');
}

function validateCloudEvent(event) {
  if (!event || typeof event !== 'object' || Array.isArray(event)) throw new TypeError('CloudEvent must be an object');
  for (const name of ['id', 'source', 'type']) if (!validCloudEventString(event[name])) throw new TypeError(`CloudEvent ${name} is empty or contains control characters`);
  for (const name of ['subject', 'datacontenttype', 'dataschema']) if (event[name] !== undefined && !validCloudEventString(event[name])) throw new TypeError(`CloudEvent ${name} is empty or contains control characters`);
  if (event.time !== undefined && (!validCloudEventString(event.time) || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(event.time) || !Number.isFinite(Date.parse(event.time)))) throw new TypeError('CloudEvents time must be RFC 3339');
  if (event.data !== undefined && !Buffer.isBuffer(event.data) && !(event.data instanceof Uint8Array)) throw new TypeError('CloudEvent data must be bytes');
  const extensions = event.extensions ?? {};
  if (!extensions || typeof extensions !== 'object' || Array.isArray(extensions) || Object.keys(extensions).length > 64) throw new TypeError('CloudEvent extensions are invalid');
  for (const [name, value] of Object.entries(extensions)) validateCloudEventExtension(name, value);
  return { id: event.id, source: event.source, type: event.type, ...(event.subject === undefined ? {} : { subject: event.subject }), ...(event.time === undefined ? {} : { time: event.time }), ...(event.datacontenttype === undefined ? {} : { datacontenttype: event.datacontenttype }), ...(event.dataschema === undefined ? {} : { dataschema: event.dataschema }), ...(event.data === undefined ? {} : { data: Buffer.from(event.data) }), extensions: { ...extensions } };
}

function encodeCloudEvent(event) {
  const normalized = validateCloudEvent(event);
  const envelope = { specversion: '1.0', id: normalized.id, source: normalized.source, type: normalized.type };
  for (const name of ['subject', 'time', 'datacontenttype', 'dataschema']) if (normalized[name] !== undefined) envelope[name] = normalized[name];
  Object.assign(envelope, normalized.extensions);
  if (normalized.data !== undefined) envelope.data_base64 = normalized.data.toString('base64');
  const encoded = Buffer.from(JSON.stringify(envelope));
  if (encoded.length > CLOUD_EVENT_MAX_BYTES) throw new TypeError('CloudEvent exceeds 1 MiB');
  return encoded;
}

function decodeCloudEvent(input) {
  const encoded = Buffer.isBuffer(input) ? input : Buffer.from(input);
  if (encoded.length > CLOUD_EVENT_MAX_BYTES) throw new TypeError('CloudEvent exceeds 1 MiB');
  let envelope;
  try { envelope = JSON.parse(encoded.toString('utf8')); } catch { throw new TypeError('CloudEvent must be valid JSON'); }
  if (!envelope || typeof envelope !== 'object' || Array.isArray(envelope)) throw new TypeError('CloudEvent must be a JSON object');
  if (envelope.specversion !== '1.0') throw new TypeError('unsupported CloudEvents specversion');
  if (Object.hasOwn(envelope, 'data') && Object.hasOwn(envelope, 'data_base64')) throw new TypeError('CloudEvent data and data_base64 are mutually exclusive');
  const event = { id: envelope.id, source: envelope.source, type: envelope.type };
  for (const name of ['subject', 'time', 'datacontenttype', 'dataschema']) if (envelope[name] != null) event[name] = envelope[name];
  if (Object.hasOwn(envelope, 'data_base64')) {
    const value = envelope.data_base64;
    if (typeof value !== 'string' || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) throw new TypeError('CloudEvent data_base64 is invalid');
    event.data = Buffer.from(value, 'base64');
  } else if (Object.hasOwn(envelope, 'data')) {
    if (isJsonContentType(event.datacontenttype)) {
      if (event.datacontenttype === undefined) event.datacontenttype = 'application/json';
      event.data = Buffer.from(JSON.stringify(envelope.data));
    } else if (typeof envelope.data === 'string') {
      event.data = Buffer.from(envelope.data);
    } else {
      throw new TypeError('non-JSON CloudEvent data must be a string');
    }
  }
  const known = new Set(['specversion', 'id', 'source', 'type', 'subject', 'time', 'datacontenttype', 'dataschema', 'data', 'data_base64']);
  event.extensions = Object.fromEntries(Object.entries(envelope).filter(([name]) => !known.has(name)));
  return validateCloudEvent(event);
}

function validateEnvMappings(mappings) {
  if (!Array.isArray(mappings) || mappings.length > 256) throw new TypeError('environment mapping must contain at most 256 keys');
  const keys = new Set();
  const names = new Set();
  for (const mapping of mappings) {
    if (!mapping || typeof mapping.key !== 'string' || Buffer.byteLength(mapping.key) === 0 || Buffer.byteLength(mapping.key) > 256 || keys.has(mapping.key)) throw new TypeError('environment mapping keys must be unique 1..=256-byte strings');
    keys.add(mapping.key);
    if (!Array.isArray(mapping.names) || mapping.names.length === 0 || mapping.names.length > 16) throw new TypeError('environment mapping requires 1..=16 aliases per key');
    for (const name of mapping.names) {
      if (typeof name !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) || names.has(name)) throw new TypeError('environment aliases must be valid and unique');
      names.add(name);
    }
  }
}

function importEnvConfig(environment, mappings) {
  validateEnvMappings(mappings);
  const imported = {};
  for (const mapping of mappings) {
    const values = mapping.names.filter((name) => Object.hasOwn(environment, name)).map((name) => environment[name]);
    if (values.length === 0) continue;
    if (values.some((value) => typeof value !== 'string' || value !== values[0])) throw new TypeError(`environment aliases for ${mapping.key} conflict`);
    if (Buffer.byteLength(values[0]) > 65536) throw new TypeError('environment config value exceeds 64 KiB');
    imported[mapping.key] = values[0];
  }
  return imported;
}

function exportEnvConfig(config, mappings) {
  validateEnvMappings(mappings);
  const exported = {};
  for (const mapping of mappings) {
    if (!Object.hasOwn(config, mapping.key)) continue;
    const value = config[mapping.key];
    if (typeof value !== 'string') throw new TypeError('environment config values must be strings');
    if (Buffer.byteLength(value) > 65536) throw new TypeError('environment config value exceeds 64 KiB');
    exported[mapping.names[0]] = value;
  }
  return exported;
}

function codecOrDefault(codec) {
  return codec || jsonCodec;
}

class QueueJob {
  constructor(client, raw, codec) {
    this._client = client;
    this._codec = codec;
    this._settled = false;
    this._leaseLost = false;
    this.signal = undefined;
    this.id = raw.id;
    this.receipt = raw.receipt;
    this.payload = codec.decode(raw.payload);
    this.attempt = raw.attempt;
    this.maxAttempts = raw.maxAttempts;
    this.leasedUntilMs = raw.leasedUntilMs;
    this.queue = raw.queue;
    this.traceContext = raw.traceparent
      ? {
          traceparent: raw.traceparent,
          tracestate: raw.tracestate ?? undefined,
          baggage: raw.baggage ?? undefined,
        }
      : undefined;
  }

  get settled() {
    return this._settled;
  }

  get leaseLost() {
    return this._leaseLost;
  }

  async ack() {
    await this._client.queueAck(this.receipt);
    this._settled = true;
  }

  async nack(opts = {}) {
    await this._client.queueNack(this.receipt, opts.retrySeconds, opts.failureSummary);
    this._settled = true;
  }

  async heartbeat() {
    await this._client.queueHeartbeat(this.receipt);
  }
}

class Queue {
  constructor(client, name, codec) {
    this.client = client;
    this.name = name;
    this.codec = codecOrDefault(codec);
  }

  async enqueue(payload, opts = {}) {
    const encoded = this.codec.encode(payload);
    return this.client.queueEnqueue(
      this.name,
      Buffer.isBuffer(encoded) ? encoded : Buffer.from(encoded),
      opts.maxAttempts,
      opts.dedupId,
      opts.delaySeconds,
      opts.jobId,
      opts.traceContext?.traceparent,
      opts.traceContext?.tracestate,
      opts.traceContext?.baggage,
      opts.baggageAllowlist,
      opts.priority,
      opts.concurrencyKey,
    );
  }

  async enqueueBatch(items) {
    return this.client.queueEnqueueBatch(
      this.name,
      items.map(({ payload, ...opts }) => ({
        payload: Buffer.from(this.codec.encode(payload)),
        maxAttempts: opts.maxAttempts,
        dedupId: opts.dedupId,
        delaySeconds: opts.delaySeconds,
        jobId: opts.jobId,
        priority: opts.priority,
        concurrencyKey: opts.concurrencyKey,
      })),
    );
  }

  async dequeue(opts = {}) {
    const raw = await this.client.queueDequeue(
      this.name,
      opts.visibilitySeconds ?? 30,
      opts.waitSeconds ?? 20,
      opts.concurrencyLimitPerKey,
    );
    return raw ? new QueueJob(this.client, raw, this.codec) : null;
  }

  async dequeueBatch(maxItems, opts = {}) {
    const jobs = await this.client.queueDequeueBatch(
      this.name,
      maxItems,
      opts.visibilitySeconds ?? 30,
      opts.waitSeconds ?? 20,
      opts.concurrencyLimitPerKey,
    );
    return jobs.map((job) => new QueueJob(this.client, job, this.codec));
  }

  async depth() {
    return this.client.queueDepth(this.name);
  }

  async pause() {
    return this.client.queuePause(this.name);
  }

  async resume() {
    return this.client.queueResume(this.name);
  }

  async isPaused() {
    return this.client.queueIsPaused(this.name);
  }

  async stats() {
    return this.client.queueStats(this.name);
  }

  async cancel(jobId) {
    const value = await this.client.queueCancel(jobId);
    return value == null ? null : JSON.parse(value);
  }

  async status(jobId) {
    const value = await this.client.queueStatus(jobId);
    return value == null ? null : JSON.parse(value);
  }

  async statuses(opts = {}) {
    return JSON.parse(await this.client.queueListStatus(this.name, opts.states, opts.cursor, opts.limit));
  }

  async deadLetters(opts = {}) {
    return this.client.queueDeadLetters(this.name, opts.cursor, opts.limit ?? 50);
  }

  async redrive(jobId, opts) {
    return this.client.queueRedrive(jobId, opts.destination, opts.dedupPolicy);
  }

  async redriveBatch(opts) {
    return this.client.queueRedriveBatch(
      this.name,
      opts.cursor,
      opts.limit ?? 50,
      opts.destination,
      opts.dedupPolicy,
    );
  }

  async purgeDryRun() {
    return this.client.queuePurgeDeadLettersDryRun(this.name);
  }

  async purge(confirmation) {
    return this.client.queuePurgeDeadLetters(this.name, confirmation);
  }

  async worker(handler, opts = {}) {
    return runWorker(this.client, this.name, handler, { ...opts, codec: this.codec });
  }
}

class KvKey {
  constructor(client, key, codec) {
    this.client = client;
    this.key = key;
    this.codec = codecOrDefault(codec);
  }

  async get() {
    const raw = await this.client.kvGet(this.key);
    return raw == null ? null : this.codec.decode(raw);
  }

  async getOrDefault(defaultValue) {
    const value = await this.get();
    return value == null ? defaultValue : value;
  }

  async set(value, opts = {}) {
    return this.client.kvSet(
      this.key,
      this.codec.encode(value),
      opts.ttlSeconds,
      opts.ifNotExists,
      opts.ifExists,
    );
  }

  async delete() {
    return this.client.kvDelete(this.key);
  }

  async exists() {
    return this.client.kvExists(this.key);
  }

  async expire(ttlSeconds) {
    return this.client.kvExpire(this.key, ttlSeconds);
  }

  async compareAndSwap(oldValue, newValue) {
    return this.client.kvCompareAndSwap(
      this.key,
      oldValue == null ? oldValue : this.codec.encode(oldValue),
      this.codec.encode(newValue),
    );
  }
}

class ConfigKey {
  constructor(client, key, defaultValue, codec) {
    this.client = client;
    this.key = key;
    this.defaultValue = defaultValue;
    this.codec = codecOrDefault(codec);
  }

  async get() {
    const raw = await this.client.configGet(this.key);
    if (raw == null) return this.defaultValue ?? null;
    return this.codec.decode(raw);
  }

  async getOrDefault(defaultValue = this.defaultValue) {
    const value = await this.get();
    return value == null ? defaultValue : value;
  }

  async set(value) {
    await this.client.configSet(this.key, this.codec.encode(value));
  }

  async delete() {
    return this.client.configDelete(this.key);
  }

  async flag(targetingKey) {
    return this.client.flag(this.key, Boolean(this.defaultValue), targetingKey);
  }
}

class TopicSubscription {
  constructor(raw, codec) {
    this._raw = raw;
    this._codec = codec;
    this._closed = false;
  }

  async next() {
    if (this._closed) return { value: undefined, done: true };
    const raw = await this._raw.next();
    if (raw == null) {
      this._closed = true;
      return { value: undefined, done: true };
    }
    return { value: this._codec.decode(raw), done: false };
  }

  async return() {
    await this.close();
    return { value: undefined, done: true };
  }

  async close() {
    if (this._closed) return;
    this._closed = true;
    if (typeof this._raw.close === 'function') {
      await this._raw.close();
    }
  }

  [Symbol.asyncIterator]() {
    return this;
  }
}

class Topic {
  constructor(client, name, codec) {
    this.client = client;
    this.name = name;
    this.codec = codecOrDefault(codec);
  }

  async publish(payload) {
    await this.client.pubsubPublish(this.name, this.codec.encode(payload));
  }

  async subscribe() {
    const raw = await this.client.pubsubSubscribe(this.name);
    return new TopicSubscription(raw, this.codec);
  }

  channel() {
    return this.client.pubsubChannel(this.name);
  }
}

function waitDelay(ms, signal) {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, Math.max(0, ms));
    if (!signal) return;
    const aborted = () => {
      clearTimeout(timer);
      resolve();
    };
    if (signal.aborted) aborted();
    else signal.addEventListener('abort', aborted, { once: true });
  });
}

function withDeadline(promise, ms, timeoutValue) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => resolve(timeoutValue), Math.max(0, ms));
    promise.then(
      (value) => { clearTimeout(timer); resolve(value); },
      (error) => { clearTimeout(timer); reject(error); },
    );
  });
}

function abortPromise(signal) {
  return new Promise((resolve) => {
    if (!signal) return;
    if (signal.aborted) return resolve(true);
    signal.addEventListener('abort', () => resolve(true), { once: true });
  });
}

function retryDelayMs(baseSeconds, attempt) {
  const capped = Math.min(30, baseSeconds * (2 ** Math.min(attempt, 5)));
  return capped * (0.8 + Math.random() * 0.4) * 1000;
}

async function processWorkerJob(client, raw, handler, opts, codec, controller) {
  const report = async (error, job, state) => {
    if (opts.onError) await opts.onError(error, job, { identity: opts.identity, state });
  };
  let job;
  try {
    job = new QueueJob(client, raw, codec);
  } catch (error) {
    await client.queueNack(raw.receipt, opts.retrySeconds, 'payload could not be decoded').catch(() => {});
    await report(error, undefined, 'decode');
    return;
  }

  job.signal = controller.signal;
  const heartbeatMs = (opts.heartbeatSeconds ?? Math.max(0.001, opts.visibilitySeconds / 3)) * 1000;
  const beat = setInterval(() => {
    Promise.resolve(typeof client.queueCancellationRequested === 'function' ? client.queueCancellationRequested(job.receipt) : false).then(async (requested) => {
      if (requested) {
        job._cancelRequested = true;
        clearInterval(beat);
        controller.abort();
        return;
      }
      await client.queueHeartbeat(job.receipt);
    }).catch(async (error) => {
      job._leaseLost = true;
      clearInterval(beat);
      controller.abort();
      await report(error, job, 'lease_lost');
    });
  }, heartbeatMs);
  const handled = Promise.resolve()
    .then(() => handler(job))
    .then(() => ({ done: true }), (error) => ({ error }));
  const outcome = await Promise.race([
    handled,
    abortPromise(controller.signal).then(() => ({ aborted: true })),
  ]);

  try {
    if (outcome.aborted) {
      if (job._cancelRequested) {
        await client.queueFinishCancellation(job.receipt).catch((error) => report(error, job, 'settle'));
        void handled.then((late) => late.error && report(late.error, job, 'handler')).catch(() => {});
        return;
      }
      if (!job.settled && !job.leaseLost) {
        await job.nack({ retrySeconds: 0 }).catch((error) => report(error, job, 'settle'));
      }
      void handled.then((late) => late.error && report(late.error, job, 'handler')).catch(() => {});
      return;
    }
    if (outcome.error) throw outcome.error;
    if (!job.settled && !job.leaseLost) await job.ack();
  } catch (error) {
    if (!job.settled && !job.leaseLost) {
      await job.nack({ retrySeconds: opts.retrySeconds, failureSummary: 'handler returned an error' }).catch((settleError) =>
        report(settleError, job, 'settle'));
    }
    await report(error, job, 'handler');
  } finally {
    clearInterval(beat);
  }
}

async function runWorkerLoop(client, name, handler, opts = {}) {
  const codec = codecOrDefault(opts.codec);
  const visibilitySeconds = opts.visibilitySeconds ?? 30;
  const waitSeconds = opts.waitSeconds ?? 20;
  const heartbeatSeconds = opts.heartbeatSeconds ?? Math.max(0.001, visibilitySeconds / 3);
  const concurrency = Math.max(1, Math.floor(opts.concurrency ?? 1));
  const retryBackoffSeconds = Math.min(30, Math.max(0, opts.retryBackoffSeconds ?? 0.25));
  const drainDeadlineSeconds = Math.max(0, opts.drainDeadlineSeconds ?? 30);
  if (!(heartbeatSeconds > 0 && heartbeatSeconds < visibilitySeconds)) {
    throw new TypeError('heartbeatSeconds must be positive and shorter than visibilitySeconds');
  }
  opts = { ...opts, visibilitySeconds, heartbeatSeconds, identity: opts.identity ?? 'worker' };
  const report = async (error, job) => {
    if (opts.onError) await opts.onError(error, job, { identity: opts.identity, state: 'dequeue' });
  };
  const active = new Set();
  const controllers = opts._handlerControllers ?? new Set();
  let retryAttempt = 0;

  while (!opts.signal?.aborted) {
    if (active.size >= concurrency) {
      await Promise.race([...active, abortPromise(opts.signal)]);
      continue;
    }
    let raw;
    try {
      raw = await client.queueDequeue(name, visibilitySeconds, waitSeconds);
    } catch (error) {
      await report(error, undefined);
      if (!forgeErrorRetryable(error)) throw error;
      retryAttempt += 1;
      await waitDelay(retryDelayMs(retryBackoffSeconds, retryAttempt), opts.signal);
      continue;
    }
    retryAttempt = 0;
    if (!raw) continue;
    // The signal may have fired while the long-poll was in flight. Release the
    // newly leased job immediately instead of beginning fresh work after shutdown.
    if (opts.signal?.aborted) {
      try {
        await client.queueNack(raw.receipt, 0, 'worker stopped before handler start');
      } catch (error) {
        await report(error, undefined);
      }
      break;
    }
    const controller = new AbortController();
    controllers.add(controller);
    const task = processWorkerJob(client, raw, handler, opts, codec, controller);
    active.add(task);
    void task.finally(() => {
      controllers.delete(controller);
      active.delete(task);
    });
  }

  if (active.size > 0) {
    const drained = await withDeadline(
      Promise.allSettled([...active]).then(() => true),
      drainDeadlineSeconds * 1000,
      false,
    );
    if (!drained) {
      for (const controller of controllers) controller.abort();
      await Promise.allSettled([...active]);
    }
  }
}

const workerStates = new WeakMap();

function workerState(client) {
  let state = workerStates.get(client);
  if (!state) {
    state = { controllers: new Set(), handlerControllers: new Set(), runs: new Set() };
    workerStates.set(client, state);
  }
  return state;
}

function runWorker(client, name, handler, opts = {}) {
  const state = workerState(client);
  const controller = new AbortController();
  const abort = () => controller.abort();
  opts.signal?.addEventListener('abort', abort, { once: true });
  if (opts.signal?.aborted) controller.abort();
  state.controllers.add(controller);
  const run = runWorkerLoop(client, name, handler, {
    ...opts,
    signal: controller.signal,
    _handlerControllers: state.handlerControllers,
  });
  state.runs.add(run);
  const cleanup = () => {
    opts.signal?.removeEventListener('abort', abort);
    state.controllers.delete(controller);
    state.runs.delete(run);
  };
  void run.then(cleanup, cleanup);
  return run;
}

function encodeQueueEnvelope(envelope) {
  const rawBody = envelope.body ?? Buffer.alloc(0);
  if (!(Buffer.isBuffer(rawBody) || rawBody instanceof Uint8Array || (Array.isArray(rawBody) && rawBody.every((value) => Number.isInteger(value) && value >= 0 && value <= 255)))) throw new TypeError('queue envelope body must contain bytes');
  const value = {
    version: envelope.version ?? 1,
    schema: envelope.schema,
    content_type: envelope.contentType,
    ...(envelope.correlationId ? { correlation_id: envelope.correlationId } : {}),
    ...(envelope.traceContext ? { trace_context: envelope.traceContext } : {}),
    ...(envelope.artifacts?.length ? { artifacts: envelope.artifacts.map((item) => ({ uri: item.uri, ...(item.contentType ? { content_type: item.contentType } : {}), ...(item.version ? { version: item.version } : {}) })) } : {}),
    body: [...Buffer.from(rawBody)],
  };
  if (value.version !== 1 || !value.schema || !value.content_type) throw new TypeError('version 1, schema, and contentType are required');
  if (Buffer.byteLength(value.schema) > 256 || Buffer.byteLength(value.content_type) > 128 || Buffer.byteLength(value.correlation_id ?? '') > 256 || (value.artifacts?.length ?? 0) > 32) throw new RangeError('queue envelope metadata exceeds its limit');
  for (const artifact of value.artifacts ?? []) {
    if (!artifact.uri) throw new TypeError('artifact uri must not be empty');
    if (Buffer.byteLength(artifact.uri) > 2048 || Buffer.byteLength(artifact.content_type ?? '') > 128 || Buffer.byteLength(artifact.version ?? '') > 256) throw new RangeError('artifact metadata exceeds its limit');
  }
  if (value.trace_context) {
    const { traceparent, tracestate = '', baggage = '' } = value.trace_context;
    const validTraceparent = typeof traceparent === 'string' && /^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$/.test(traceparent) && !traceparent.startsWith('ff-') && !/^.{3}0{32}-/.test(traceparent) && !/^.{36}0{16}-/.test(traceparent);
    if (!validTraceparent || Buffer.byteLength(traceparent) > 512 || Buffer.byteLength(tracestate) > 512 || Buffer.byteLength(baggage) > 1024 || /[\r\n]/.test(tracestate + baggage) || baggage.split(',').filter(Boolean).length > 16) throw new TypeError('queue envelope trace context is invalid');
  }
  const encoded = Buffer.from(JSON.stringify(value));
  if (encoded.length > 256 * 1024) throw new RangeError('encoded envelope exceeds 256 KiB; use blob references for large bodies');
  return encoded;
}

function decodeQueueEnvelope(encoded) {
  const value = JSON.parse(Buffer.from(encoded).toString('utf8'));
  if (!Array.isArray(value.body) || !value.body.every((item) => Number.isInteger(item) && item >= 0 && item <= 255)) throw new TypeError('queue envelope body must contain bytes');
  const checked = encodeQueueEnvelope({ version: value.version, schema: value.schema, contentType: value.content_type, correlationId: value.correlation_id, traceContext: value.trace_context, artifacts: value.artifacts?.map((item) => ({ uri: item.uri, contentType: item.content_type, version: item.version })), body: value.body });
  void checked;
  return { version: value.version, schema: value.schema, contentType: value.content_type, correlationId: value.correlation_id, traceContext: value.trace_context, artifacts: value.artifacts?.map((item) => ({ uri: item.uri, contentType: item.content_type, version: item.version })) ?? [], body: Buffer.from(value.body) };
}

async function runOutboxRelayLoop(client, opts = {}) {
  const idleMs = Math.max(0, (opts.idleSeconds ?? 0.5) * 1000);
  let attempt = 0;
  while (!opts.signal?.aborted) {
    try {
      const report = await client.runOutboxOnce(
        opts.batchSize,
        opts.claimSeconds,
        opts.failureBackoffSeconds,
        opts.baggageAllowlist,
      );
      attempt = 0;
      if (report.claimed > 0) continue;
      await waitDelay(idleMs, opts.signal);
    } catch (error) {
      if (opts.signal?.aborted) break;
      if (opts.onError) await opts.onError(error, { identity: opts.identity ?? 'outbox-relay' });
      attempt += 1;
      await waitDelay(retryDelayMs(opts.retryBackoffSeconds ?? 0.25, attempt), opts.signal);
    }
  }
}

function runOutboxRelay(client, opts = {}) {
  const state = workerState(client);
  const controller = new AbortController();
  const abort = () => controller.abort();
  opts.signal?.addEventListener('abort', abort, { once: true });
  if (opts.signal?.aborted) controller.abort();
  state.controllers.add(controller);
  const run = runOutboxRelayLoop(client, { ...opts, signal: controller.signal });
  state.runs.add(run);
  const cleanup = () => {
    opts.signal?.removeEventListener('abort', abort);
    state.controllers.delete(controller);
    state.runs.delete(run);
  };
  void run.then(cleanup, cleanup);
  return run;
}

function queue(client, name, codec) {
  return new Queue(client, name, codec);
}

function kv(client, key, codec) {
  return new KvKey(client, key, codec);
}

function config(client, key, defaultValue, codec) {
  return new ConfigKey(client, key, defaultValue, codec);
}

function topic(client, name, codec) {
  return new Topic(client, name, codec);
}

const FORGE_CODES = new Set([
  'NOT_FOUND',
  'INVALID',
  'LIMIT',
  'PRECONDITION',
  'UNAVAILABLE',
  'CONFIG',
  'NOT_CONFIGURED',
  'BACKEND',
]);

const RETRYABLE_MARKER = '(retryable)';

function forgeErrorMetadata(error) {
  if (!(error instanceof Error)) return undefined;
  if (typeof error.code === 'string' && FORGE_CODES.has(error.code)) return error;
  try {
    const parsed = JSON.parse(error.message);
    return parsed?.forge_error === true && FORGE_CODES.has(parsed.code) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

function decorateForgeError(error) {
  const metadata = forgeErrorMetadata(error);
  if (metadata === undefined || !(error instanceof Error) || metadata === error) return error;
  error.name = 'ForgeError';
  error.code = metadata.code;
  error.retryable = Boolean(metadata.retryable);
  error.operation = metadata.operation;
  error.backend = metadata.backend ?? undefined;
  error.safeMessage = metadata.message;
  error.message = metadata.message;
  return error;
}

function forgeHelperError(code, message, operation) {
  const error = new Error(message);
  error.name = 'ForgeError';
  error.code = code;
  error.retryable = false;
  error.operation = operation;
  error.backend = undefined;
  error.safeMessage = message;
  return error;
}

// The napi layer cannot set custom properties on thrown errors, so the Forge
// error class travels as a "CODE: message" prefix ("CODE(retryable): message"
// for backend errors that are safe to retry); recover it from there.
function forgeErrorHead(error) {
  const message =
    error instanceof Error ? error.message : typeof error === 'string' ? error : '';
  const sep = message.indexOf(': ');
  return sep > 0 ? message.slice(0, sep) : undefined;
}

function forgeErrorCode(error) {
  const metadata = forgeErrorMetadata(error);
  if (metadata !== undefined) return metadata.code;
  let head = forgeErrorHead(error);
  if (head === undefined) return undefined;
  if (head.endsWith(RETRYABLE_MARKER)) head = head.slice(0, -RETRYABLE_MARKER.length);
  return FORGE_CODES.has(head) ? head : undefined;
}

function forgeErrorRetryable(error) {
  const metadata = forgeErrorMetadata(error);
  if (metadata !== undefined) return Boolean(metadata.retryable);
  const code = forgeErrorCode(error);
  if (code === 'UNAVAILABLE') return true;
  const head = forgeErrorHead(error);
  return code !== undefined && head !== undefined && head.endsWith(RETRYABLE_MARKER);
}

const { ForgeClient: NativeForgeClient } = native;
function structuredForgeCall(method, receiver, args) {
  try {
    const result = method.apply(receiver, args);
    return result instanceof Promise ? result.catch((error) => Promise.reject(decorateForgeError(error))) : result;
  } catch (error) {
    throw decorateForgeError(error);
  }
}

function wrapForgeErrors(target, name) {
  const method = target[name];
  if (typeof method !== 'function') return;
  target[name] = function structuredForgeErrors(...args) {
    return structuredForgeCall(method, this, args);
  };
}
for (const name of Object.getOwnPropertyNames(NativeForgeClient.prototype)) {
  if (name !== 'constructor') wrapForgeErrors(NativeForgeClient.prototype, name);
}

function callNativeStatic(name, args, returnsClient = false) {
  const result = structuredForgeCall(NativeForgeClient[name], NativeForgeClient, args);
  if (!returnsClient) return result;
  return result.then((client) => {
    Object.setPrototypeOf(client, ForgeClient.prototype);
    return client;
  });
}

class ForgeClient extends NativeForgeClient {
  static init() { return callNativeStatic('init', [], true); }
  static initFrom(path) { return callNativeStatic('initFrom', [path], true); }
  static initFromString(toml) { return callNativeStatic('initFromString', [toml], true); }
  static initMemoryForTesting(toml, epochMs, seed) { return callNativeStatic('initMemoryForTesting', [toml, epochMs, seed], true); }
  static migrate() { return callNativeStatic('migrate', []); }
  static migrateFrom(path) { return callNativeStatic('migrateFrom', [path]); }
  static migrateFromString(toml) { return callNativeStatic('migrateFromString', [toml]); }
  static migrationStatus() { return callNativeStatic('migrationStatus', []); }
  static migrationStatusFrom(path) { return callNativeStatic('migrationStatusFrom', [path]); }
  static migrationStatusFromString(toml) { return callNativeStatic('migrationStatusFromString', [toml]); }
  static validateSchema() { return callNativeStatic('validateSchema', []); }
  static validateSchemaFrom(path) { return callNativeStatic('validateSchemaFrom', [path]); }
  static validateSchemaFromString(toml) { return callNativeStatic('validateSchemaFromString', [toml]); }
}
const nativeClose = ForgeClient.prototype.close;
const nativeConfigSnapshot = ForgeClient.prototype.configSnapshot;

ForgeClient.prototype.configSnapshot = async function forgeConfigSnapshot(...args) {
  return freezeConfigSnapshot(await nativeConfigSnapshot.apply(this, args));
};

ForgeClient.prototype.close = async function close(timeoutSeconds = 30) {
  const started = Date.now();
  const state = workerState(this);
  for (const controller of state.controllers) controller.abort();
  let drained = state.runs.size === 0;
  if (!drained) {
    const outcome = await withDeadline(
      Promise.allSettled([...state.runs]).then(() => 'drained'),
      Math.max(0, timeoutSeconds) * 1000,
      'deadline',
    );
    drained = outcome === 'drained';
  }
  if (!drained) {
    for (const controller of state.handlerControllers) controller.abort();
    await Promise.allSettled([...state.runs]);
    drained = true;
  }
  const elapsed = (Date.now() - started) / 1000;
  await nativeClose.call(this, Math.max(0, timeoutSeconds - elapsed));
};

ForgeClient.prototype.queue = function forgeQueue(name, codec) {
  return queue(this, name, codec);
};

ForgeClient.prototype.kv = function forgeKv(key, codec) {
  return kv(this, key, codec);
};

ForgeClient.prototype.config = function forgeConfig(key, defaultValue, codec) {
  return config(this, key, defaultValue, codec);
};

ForgeClient.prototype.topic = function forgeTopic(name, codec) {
  return topic(this, name, codec);
};

ForgeClient.prototype.worker = function forgeWorker(name, handler, opts = {}) {
  return runWorker(this, name, handler, opts);
};

ForgeClient.prototype.runOutboxRelay = function forgeOutboxRelay(opts = {}) {
  return runOutboxRelay(this, opts);
};

ForgeClient.prototype.reserveRateLimit = async function reserveRateLimit(bucket, key, opts) {
  const value = await this.rateLimitReserve(bucket, key, opts.max, opts.perSeconds, opts.cost, opts.ttlSeconds, opts.algo);
  return value == null ? null : JSON.parse(value);
};

ForgeClient.prototype.commitRateLimit = async function commitRateLimit(reservationId, actualUnits) {
  return JSON.parse(await this.rateLimitCommit(reservationId, actualUnits));
};

ForgeClient.prototype.releaseRateLimit = async function releaseRateLimit(reservationId) {
  return JSON.parse(await this.rateLimitRelease(reservationId));
};

function validateScopeComponent(label, value) {
  if (typeof value !== 'string' || Buffer.byteLength(value) < 1 || Buffer.byteLength(value) > 255) throw forgeHelperError('INVALID', `scope ${label} must contain 1 to 255 bytes`, 'scope');
  if (/\p{Cc}/u.test(value)) throw forgeHelperError('INVALID', `scope ${label} must not contain control characters`, 'scope');
  return value;
}

function renderScopedName(kind, parts) {
  const budget = kind === 'blob' ? 895 : 383;
  const labels = ['application', 'tenant', 'user', 'resource'];
  parts = parts.map((part, index) => validateScopeComponent(labels[index], part));
  const encoded = parts.map(part => `${Buffer.byteLength(part)}:${part}`).join('');
  const value = `v1|${kind}|${encoded}`;
  if (Buffer.byteLength(value) > budget) throw forgeHelperError('LIMIT', `scoped ${kind} name exceeds its backend-safe length`, 'scope');
  return value;
}

function scopeKvKey(application, tenant, user, resource) { return renderScopedName('kv', [application, tenant, user, resource]); }
function scopeBlobKey(application, tenant, user, resource) { return renderScopedName('blob', [application, tenant, user, resource]); }
function scopeRateLimitSubject(application, tenant, user, resource) { return renderScopedName('rate', [application, tenant, user, resource]); }
function scopeTopic(application, tenant, user, resource) { return renderScopedName('topic', [application, tenant, user, resource]); }

function parseScopedName(value) {
  if (typeof value !== 'string' || !value.startsWith('v1|')) throw forgeHelperError('INVALID', 'scoped name must use v1', 'scope.parse');
  const second = value.indexOf('|', 3);
  if (second < 0) throw forgeHelperError('INVALID', 'scoped name is malformed', 'scope.parse');
  const kind = value.slice(3, second);
  const budget = kind === 'blob' ? 895 : ['kv', 'rate', 'topic'].includes(kind) ? 383 : 0;
  if (!budget) throw forgeHelperError('INVALID', 'scoped name kind is unknown', 'scope.parse');
  const bytes = Buffer.from(value.slice(second + 1));
  const parts = [];
  let offset = 0;
  for (const label of ['application', 'tenant', 'user', 'resource']) {
    const colon = bytes.indexOf(58, offset);
    if (colon < 0) throw forgeHelperError('INVALID', 'scoped name is malformed', 'scope.parse');
    const lengthText = bytes.subarray(offset, colon).toString('ascii');
    if (!/^\d+$/.test(lengthText)) throw forgeHelperError('INVALID', 'scoped name length is malformed', 'scope.parse');
    const length = Number(lengthText);
    const end = colon + 1 + length;
    if (end > bytes.length) throw forgeHelperError('INVALID', 'scoped name component length is invalid', 'scope.parse');
    let part;
    try { part = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(colon + 1, end)); }
    catch { throw forgeHelperError('INVALID', 'scoped name component length is invalid', 'scope.parse'); }
    if (Buffer.byteLength(part) !== length) throw forgeHelperError('INVALID', 'scoped name component length is invalid', 'scope.parse');
    parts.push(validateScopeComponent(label, part));
    offset = end;
  }
  if (offset !== bytes.length) throw forgeHelperError('INVALID', 'scoped name has trailing data', 'scope.parse');
  if (Buffer.byteLength(value) > budget) throw forgeHelperError('LIMIT', `scoped ${kind} name exceeds its backend-safe length`, 'scope.parse');
  const [application, tenant, user, resource] = parts;
  return Object.freeze({ kind, application, tenant, user, resource });
}

module.exports = {
  ...native,
  ForgeClient,
  Queue,
  QueueJob,
  KvKey,
  ConfigKey,
  Topic,
  TopicSubscription,
  runWorker,
  bytesCodec,
  encodeQueueEnvelope,
  decodeQueueEnvelope,
  encodeInvalidationEvent,
  decodeInvalidationEvent,
  encodeCloudEvent,
  decodeCloudEvent,
  importEnvConfig,
  exportEnvConfig,
  encodeConfigSnapshot,
  decodeConfigSnapshot,
  configSnapshotGet,
  configSnapshotFlagDetails,
  runOutboxRelay,
  queue,
  kv,
  config,
  topic,
  jsonCodec,
  forgeErrorCode,
  forgeErrorRetryable,
  scopeKvKey,
  scopeBlobKey,
  scopeRateLimitSubject,
  scopeTopic,
  parseScopedName,
};

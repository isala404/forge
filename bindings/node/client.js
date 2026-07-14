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

function codecOrDefault(codec) {
  return codec || jsonCodec;
}

class QueueJob {
  constructor(client, raw, codec) {
    this._client = client;
    this._codec = codec;
    this._settled = false;
    this._leaseLost = false;
    this.id = raw.id;
    this.receipt = raw.receipt;
    this.payload = codec.decode(raw.payload);
    this.attempt = raw.attempt;
    this.maxAttempts = raw.maxAttempts;
    this.leasedUntilMs = raw.leasedUntilMs;
    this.queue = raw.queue;
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
    await this._client.queueNack(this.receipt, opts.retrySeconds);
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
    return this.client.queueEnqueue(
      this.name,
      this.codec.encode(payload),
      opts.maxAttempts,
      opts.dedupId,
      opts.delaySeconds,
    );
  }

  async dequeue(opts = {}) {
    const raw = await this.client.queueDequeue(
      this.name,
      opts.visibilitySeconds ?? 30,
      opts.waitSeconds ?? 20,
    );
    return raw ? new QueueJob(this.client, raw, this.codec) : null;
  }

  async depth() {
    return this.client.queueDepth(this.name);
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

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function runWorker(client, name, handler, opts = {}) {
  const codec = codecOrDefault(opts.codec);
  const visibilitySeconds = opts.visibilitySeconds ?? 30;
  const waitSeconds = opts.waitSeconds ?? 20;
  const heartbeatMs = Math.max(1, visibilitySeconds / 3) * 1000;
  const report = async (error, job) => {
    if (opts.onError) await opts.onError(error, job);
  };

  while (!opts.signal?.aborted) {
    let raw;
    try {
      raw = await client.queueDequeue(name, visibilitySeconds, waitSeconds);
    } catch (error) {
      // Transient backend blip; report, back off, retry (same as the Python worker).
      await report(error, undefined);
      await sleep(250);
      continue;
    }
    if (!raw) continue;
    // The signal may have fired while the long-poll was in flight. Release the
    // newly leased job immediately instead of beginning fresh work after shutdown.
    if (opts.signal?.aborted) {
      try {
        await client.queueNack(raw.receipt, 0);
      } catch (error) {
        await report(error, undefined);
      }
      break;
    }

    let job;
    try {
      job = new QueueJob(client, raw, codec);
    } catch (error) {
      await client.queueNack(raw.receipt, opts.retrySeconds).catch(() => {});
      await report(error, undefined);
      continue;
    }

    // Keep the lease alive while the handler runs; a failed heartbeat means the
    // lease is gone and the job will be redelivered, so stop settling it here.
    const beat = setInterval(() => {
      client.queueHeartbeat(job.receipt).catch(() => {
        job._leaseLost = true;
        clearInterval(beat);
      });
    }, heartbeatMs);

    try {
      await handler(job);
      try {
        if (!job.settled && !job.leaseLost) await job.ack();
      } catch (error) {
        await report(error, job);
      }
    } catch (error) {
      if (!job.settled && !job.leaseLost) {
        await job.nack({ retrySeconds: opts.retrySeconds }).catch(() => {});
      }
      await report(error, job);
    } finally {
      clearInterval(beat);
    }
  }
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
  'BACKEND',
]);

const RETRYABLE_MARKER = '(retryable)';

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
  let head = forgeErrorHead(error);
  if (head === undefined) return undefined;
  if (head.endsWith(RETRYABLE_MARKER)) head = head.slice(0, -RETRYABLE_MARKER.length);
  return FORGE_CODES.has(head) ? head : undefined;
}

function forgeErrorRetryable(error) {
  const code = forgeErrorCode(error);
  if (code === 'UNAVAILABLE') return true;
  const head = forgeErrorHead(error);
  return code !== undefined && head !== undefined && head.endsWith(RETRYABLE_MARKER);
}

const { ForgeClient } = native;

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
  queue,
  kv,
  config,
  topic,
  jsonCodec,
  forgeErrorCode,
  forgeErrorRetryable,
};

'use strict'

/** Parse the stable error code the binding prefixes onto a thrown error's message
 *  (e.g. "INVALID: ..." -> "INVALID"). Returns "UNKNOWN" for non-Forge errors. */
function forgeErrorCode(err) {
  const msg = err && err.message ? String(err.message) : ''
  const m = /^([A-Z_]+):\s/.exec(msg)
  return m ? m[1] : 'UNKNOWN'
}

/** Whether a thrown Forge error is retryable. Only
 *  UNAVAILABLE is retryable from the message surface; a retryable BACKEND error is
 *  indistinguishable here (the flag is not in the message), so it reads as false. */
function forgeErrorRetryable(err) {
  return forgeErrorCode(err) === 'UNAVAILABLE'
}

/** A typed queue handle: name + codec bound to a payload type. */
class TypedQueue {
  constructor(client, name) {
    this.client = client
    this.name = name
  }
  enqueue(payload, opts = {}) {
    return this.client.queueEnqueue(
      this.name,
      JSON.stringify(payload),
      opts.maxAttempts ?? null,
      opts.dedupId ?? null,
    )
  }
  async dequeue(opts = {}) {
    const job = await this.client.queueDequeue(
      this.name,
      opts.visibilitySeconds ?? 30,
      opts.waitSeconds ?? 20,
    )
    if (!job) return null
    return {
      id: job.id,
      receipt: job.receipt,
      attempt: job.attempt,
      maxAttempts: job.maxAttempts,
      leasedUntilMs: job.leasedUntilMs,
      payload: JSON.parse(job.payload),
    }
  }
  ack(receipt) {
    return this.client.queueAck(receipt)
  }
  nack(receipt, retrySeconds) {
    return this.client.queueNack(receipt, retrySeconds ?? null)
  }
  heartbeat(receipt) {
    return this.client.queueHeartbeat(receipt)
  }
  depth() {
    return this.client.queueDepth(this.name)
  }
}

/** A typed kv key: key + codec bound to a value type. */
class TypedKvKey {
  constructor(client, key) {
    this.client = client
    this.key = key
  }
  async get() {
    const raw = await this.client.kvGet(this.key)
    return raw == null ? null : JSON.parse(raw)
  }
  set(value, opts = {}) {
    return this.client.kvSet(
      this.key,
      JSON.stringify(value),
      opts.ttlSeconds ?? null,
      opts.ifNotExists ?? null,
    )
  }
  delete() {
    return this.client.kvDelete(this.key)
  }
}

/** A typed config key: key + codec + default bound to a value type. */
class TypedConfigKey {
  constructor(client, key, defaultValue) {
    this.client = client
    this.key = key
    this.defaultValue = defaultValue
  }
  async get() {
    const raw = await this.client.configGet(this.key)
    return raw == null ? null : JSON.parse(raw)
  }
  async getOrDefault() {
    const v = await this.get()
    return v == null ? this.defaultValue : v
  }
  set(value) {
    return this.client.configSet(this.key, JSON.stringify(value))
  }
}

/** A typed pubsub topic: topic + codec bound to an event type. */
class TypedTopic {
  constructor(client, topic) {
    this.client = client
    this.topic = topic
  }
  publish(event) {
    return this.client.pubsubPublish(this.topic, JSON.stringify(event))
  }
  /** Subscribe; yields decoded events. `for await (const e of topic.subscribe())`. */
  async subscribe() {
    const sub = await this.client.pubsubSubscribe(this.topic)
    return {
      [Symbol.asyncIterator]() {
        return {
          async next() {
            const buf = await sub.next()
            if (buf == null) return { value: undefined, done: true }
            return { value: JSON.parse(buf.toString('utf8')), done: false }
          },
        }
      },
    }
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/**
 * Managed worker loop over a queue: dequeue, auto-heartbeat at a third of the
 * visibility window, ack on success / nack on throw, abandon on lease loss, drain
 * on shutdown.
 *
 *   const stop = new AbortController()
 *   runWorker(client, "emails", async (job) => { ... }, { signal: stop.signal })
 *
 * `handler(job)` receives `{ id, attempt, maxAttempts, payload, signal }`; `payload`
 * is the raw string. `signal` aborts if the lease is lost mid-flight; a cooperative
 * handler should check it and stop. Returns a promise that resolves when `signal`
 * (opts.signal) aborts and the in-flight job has drained.
 */
async function runWorker(client, queueName, handler, opts = {}) {
  const visibility = opts.visibilitySeconds ?? 30
  const wait = opts.waitSeconds ?? 20
  const stop = opts.signal
  const hbEvery = Math.max(1000, Math.floor((visibility * 1000) / 3))

  while (!(stop && stop.aborted)) {
    let job
    try {
      job = await client.queueDequeue(queueName, visibility, wait)
    } catch {
      await sleep(250) // transient backend blip; back off and retry
      continue
    }
    if (!job) continue

    const lease = new AbortController()
    const beat = setInterval(() => {
      client.queueHeartbeat(job.receipt).catch(() => lease.abort())
    }, hbEvery)
    try {
      await handler({
        id: job.id,
        attempt: job.attempt,
        maxAttempts: job.maxAttempts,
        payload: job.payload,
        signal: lease.signal,
      })
      clearInterval(beat)
      if (!lease.signal.aborted) await client.queueAck(job.receipt).catch(() => {})
    } catch {
      clearInterval(beat)
      // Lease already lost => the receipt is gone; nacking would just throw.
      if (!lease.signal.aborted) await client.queueNack(job.receipt).catch(() => {})
    }
  }
}

const typedQueue = (client, name) => new TypedQueue(client, name)
const typedKv = (client, key) => new TypedKvKey(client, key)
const typedConfig = (client, key, defaultValue) => new TypedConfigKey(client, key, defaultValue)
const typedTopic = (client, topic) => new TypedTopic(client, topic)

module.exports = {
  forgeErrorCode,
  forgeErrorRetryable,
  runWorker,
  TypedQueue,
  TypedKvKey,
  TypedConfigKey,
  TypedTopic,
  typedQueue,
  typedKv,
  typedConfig,
  typedTopic,
}

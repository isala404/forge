// Typed projection over the napi ForgeClient: bind a name + JSON codec to a type, so
// app code enqueues `SendEmail` instead of a raw queue string + JSON.stringify. This
// is the Node view of the same typed layer the Rust crate exposes (src/typed.rs) and
// the Python binding exposes (forge_py/typed.py). Plain JS (no build step); the types
// live in typed.d.ts.

'use strict'

/** Parse the stable error code the binding prefixes onto a thrown error's message
 *  (e.g. "INVALID: ..." -> "INVALID"). Returns "UNKNOWN" for non-Forge errors. */
function forgeErrorCode(err) {
  const msg = err && err.message ? String(err.message) : ''
  const m = /^([A-Z_]+):\s/.exec(msg)
  return m ? m[1] : 'UNKNOWN'
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
      opts.waitSeconds ?? 1,
    )
    if (!job) return null
    return { id: job.id, attempt: job.attempt, payload: JSON.parse(job.payload) }
  }
  ack(id) {
    return this.client.queueAck(id)
  }
  nack(id, retrySeconds) {
    return this.client.queueNack(id, retrySeconds ?? null)
  }
  heartbeat(id) {
    return this.client.queueHeartbeat(id)
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
  /** Subscribe, yielding decoded events. `for await (const e of topic.subscribe())`. */
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

const typedQueue = (client, name) => new TypedQueue(client, name)
const typedKv = (client, key) => new TypedKvKey(client, key)
const typedConfig = (client, key, defaultValue) => new TypedConfigKey(client, key, defaultValue)
const typedTopic = (client, topic) => new TypedTopic(client, topic)

module.exports = {
  forgeErrorCode,
  TypedQueue,
  TypedKvKey,
  TypedConfigKey,
  TypedTopic,
  typedQueue,
  typedKv,
  typedConfig,
  typedTopic,
}

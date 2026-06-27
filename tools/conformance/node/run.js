'use strict'
// Cross-language conformance runner, Node side. Reads the shared scenario
// matrix in src/conformance/scenarios/*.json and runs each scenario against the
// forge-node binding on a throwaway database. Asserts the observed failure set
// equals exactly the `node` entries in src/conformance/known_gaps.json. See
// ../README.md.
//
//   TEST_DATABASE_URL=postgres://… node tools/conformance/node/run.js

const fs = require('fs')
const path = require('path')
const crypto = require('crypto')
const os = require('os')
const { Client } = require('pg')
const { ForgeClient } = require('../../../bindings/forge-node')

const SCENARIO_DIR = path.join(__dirname, '..', '..', '..', 'src', 'conformance', 'scenarios')
const GAPS_FILE = path.join(__dirname, '..', '..', '..', 'src', 'conformance', 'known_gaps.json')
const LANG = 'node'
// The binding runs against Postgres, so a scenario pinned to other backends is skipped.
const BACKEND = 'postgres'
// Enables the blob presign/verify scenarios; harmless for the rest.
const SIGNING_SECRET = 'conformance-signing-secret'
// Sentinel a pubsub.receive race resolves to when the 2s bound elapses with no message.
const TIMEOUT = Symbol('timeout')

const ADMIN_URL = process.env.TEST_DATABASE_URL
if (!ADMIN_URL) {
  console.error('TEST_DATABASE_URL is not set')
  process.exit(2)
}

// error code mapping: binding uses UPPER_SNAKE; canonical is Pascal
const CODE_MAP = {
  CONFIG: 'Config',
  UNAVAILABLE: 'Unavailable',
  NOT_FOUND: 'NotFound',
  PRECONDITION: 'Precondition',
  LIMIT: 'Limit',
  INVALID: 'Invalid',
  BACKEND: 'Backend',
}
function canonicalErrorCode(err) {
  const msg = err && err.message ? String(err.message) : ''
  const m = /^([A-Z_]+):/.exec(msg)
  return (m && CODE_MAP[m[1]]) || 'Backend'
}

function swapDb(url, name) {
  const u = new URL(url)
  u.pathname = '/' + name
  return u.toString()
}
async function withTestDb(fn) {
  const name = 'forge_conf_' + crypto.randomUUID().replace(/-/g, '').slice(0, 12)
  let admin = new Client({ connectionString: ADMIN_URL })
  await admin.connect()
  await admin.query(`CREATE DATABASE "${name}"`)
  await admin.end()
  try {
    const url = swapDb(ADMIN_URL, name)
    // connect/connectWith migrates the throwaway DB's schema before use.
    return await fn(url)
  } finally {
    admin = new Client({ connectionString: ADMIN_URL })
    await admin.connect()
    await admin.query(`DROP DATABASE IF EXISTS "${name}" WITH (FORCE)`).catch(() => {})
    await admin.end()
  }
}

function valueToString(v) {
  if (typeof v === 'string') return v
  if (v && Array.isArray(v.$bytes)) return Buffer.from(v.$bytes).toString('utf8') // lossy until a bytes API exists
  throw new Error('cannot coerce value to string: ' + JSON.stringify(v))
}
function asBytes(actual) {
  if (actual == null) return null
  if (Buffer.isBuffer(actual)) return Array.from(actual)
  if (typeof actual === 'string') return Array.from(Buffer.from(actual, 'utf8'))
  if (actual && Array.isArray(actual.$bytes)) return actual.$bytes
  return null
}
function asBuffer(v) {
  if (typeof v === 'string') return Buffer.from(v, 'utf8')
  if (v && Array.isArray(v.$bytes)) return Buffer.from(v.$bytes)
  throw new Error('cannot coerce value to bytes: ' + JSON.stringify(v))
}

function provider(report, primitive) {
  const row = report.find((r) => r.primitive === primitive)
  return row && row.provider
}

function restoreEnv(saved) {
  for (const [key, value] of Object.entries(saved)) {
    if (value == null) delete process.env[key]
    else process.env[key] = value
  }
}

async function runBackendSelectionSmoke() {
  await withTestDb(async (url) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'forge-node-blob-'))
    const keys = [
      'FORGE_POSTGRES_URL',
      'FORGE_BLOB_SIGNING_SECRET',
      'FORGE_QUEUE_BACKEND',
      'FORGE_BLOB_BACKEND',
      'FORGE_BLOB_FS_ROOT',
    ]
    const saved = Object.fromEntries(keys.map((key) => [key, process.env[key]]))
    try {
      process.env.FORGE_POSTGRES_URL = url
      process.env.FORGE_BLOB_SIGNING_SECRET = SIGNING_SECRET
      process.env.FORGE_QUEUE_BACKEND = 'memory'
      process.env.FORGE_BLOB_BACKEND = 'filesystem'
      process.env.FORGE_BLOB_FS_ROOT = root

      const client = await ForgeClient.connectFromEnv()
      const report = client.backendReport()
      if (provider(report, 'queue') !== 'memory') throw new Error('queue backend was not memory')
      if (provider(report, 'blob') !== 'filesystem') throw new Error('blob backend was not filesystem')

      await client.queueEnqueue('swapq', 'hello')
      const job = await client.queueDequeue('swapq', 30, 0)
      if (!job || job.payload !== 'hello') throw new Error('memory queue smoke failed')
      await client.queueAck(job.receipt)

      await client.blobPut('swap/blob.txt', 'blob', 'text/plain')
      if ((await client.blobGet('swap/blob.txt')) !== 'blob') {
        throw new Error('filesystem blob smoke failed')
      }
    } finally {
      restoreEnv(saved)
      fs.rmSync(root, { recursive: true, force: true })
    }
  })
  console.log('PASS  backend/env_backend_selection')
}
// Decompose a presigned URL into the signed params verify_presigned needs, so a scenario
// can $ref them straight into a verify step. Mirrors the Rust runner's presign_to_value.
function parsePresign(url, key, method) {
  const q = url.includes('?') ? url.slice(url.indexOf('?') + 1) : ''
  let expires_epoch = 0
  let max_bytes = 0
  let sig = ''
  for (const pair of q.split('&')) {
    const [k, val] = pair.split('=')
    if (k === 'expires') expires_epoch = Number(val)
    else if (k === 'max_bytes') max_bytes = Number(val)
    else if (k === 'sig') sig = val ?? ''
  }
  return { url, key, method, expires_epoch, max_bytes, sig }
}

async function dispatch(client, op, args, subscriptions, captureAs) {
  switch (op) {
    case 'kv.set':
      return client.kvSet(
        args.key,
        valueToString(args.value),
        args.ttl_seconds ?? null,
        args.if_not_exists ?? null,
        args.if_exists ?? null,
      )
    case 'kv.get':
      return await client.kvGet(args.key) // string | null
    case 'kv.set_bytes':
      return client.kvSetBytes(args.key, Buffer.from(args.value.$bytes), args.ttl_seconds ?? null, args.if_not_exists ?? null)
    case 'kv.get_bytes':
      return await client.kvGetBytes(args.key) // Buffer | null
    case 'kv.exists':
      return client.kvExists(args.key)
    case 'kv.delete':
      return client.kvDelete(args.key)
    case 'kv.incr':
      return client.kvIncr(args.key, args.by)
    case 'kv.compare_and_swap':
      return client.kvCompareAndSwap(args.key, args.old == null ? null : valueToString(args.old), valueToString(args.new))
    case 'kv.scan_page': {
      const page = await client.kvScanPage(args.prefix, args.cursor ?? null, args.limit ?? 100)
      return { keys: page.keys, cursor: page.cursor ?? null }
    }
    case 'ratelimit.check': {
      const d = await client.rateLimitCheck(args.bucket, args.key, args.max, args.per_seconds, args.fail_open ?? null, args.algo ?? null)
      return {
        allowed: d.allowed,
        limit: d.limit ?? null,
        remaining: d.remaining,
        reset_after_seconds: d.resetAfterSeconds ?? null,
        retry_after_seconds: d.retryAfterSeconds ?? null,
      }
    }
    case 'schedule.at':
      return await client.scheduleAt(args.when_epoch_ms, args.queue, valueToString(args.payload), args.max_attempts ?? null)
    case 'schedule.cron':
      return await client.scheduleCron(args.name, args.expr, args.queue, valueToString(args.payload), args.max_attempts ?? null)
    case 'schedule.cancel':
      return client.scheduleCancel(args.name)
    case 'schedule.cancel_at':
      return client.scheduleCancelAt(args.job_id)
    case 'schedule.list': {
      const page = await client.scheduleList(args.cursor ?? null, args.limit ?? null)
      return {
        items: page.items.map((s) => ({
          name: s.name ?? null,
          kind: s.kind,
          queue: s.queue ?? null,
          next_run_ms: s.nextRunMs,
          last_run_ms: s.lastRunMs ?? null,
          cron_expr: s.cronExpr ?? null,
        })),
        cursor: page.cursor ?? null,
      }
    }
    case 'schedule.tick':
      return await client.runSchedulerOnce()
    case 'queue.enqueue':
      return await client.queueEnqueue(args.queue, valueToString(args.payload), args.max_attempts ?? null, args.dedup_id ?? null, args.delay_seconds ?? null)
    case 'queue.dequeue': {
      const job = await client.queueDequeue(args.queue, args.visibility_seconds, args.wait_seconds)
      return job == null ? null : { id: job.id, receipt: job.receipt, payload: job.payload, attempt: job.attempt, max_attempts: job.maxAttempts }
    }
    case 'queue.ack':
      return client.queueAck(args.receipt)
    case 'queue.nack':
      return client.queueNack(args.receipt, args.retry_seconds ?? null)
    case 'queue.depth': {
      const d = await client.queueDepth(args.queue)
      return { visible: d.visible, in_flight: d.inFlight, delayed: d.delayed }
    }
    case 'config.set':
      return await client.configSet(args.key, args.value)
    case 'config.get':
      return await client.configGet(args.key)
    case 'config.flag':
      return await client.flag(args.key, args.default ?? false, args.targeting_key ?? null)
    case 'config.set_flag_on':
      return await client.setFlagOn(args.key)
    case 'config.set_flag_off':
      return await client.setFlagOff(args.key)
    case 'auth.create_session':
      return await client.createSession(args.user_id, args.idle_seconds ?? null, args.absolute_seconds ?? null)
    case 'auth.validate_session':
      return await client.validateSession(args.token)
    case 'auth.revoke_session':
      return client.revokeSession(args.token)
    case 'auth.create_api_key': {
      const k = await client.createApiKey(args.owner_id, args.label)
      return { id: k.id, secret: k.secret, label: k.label ?? null, created_at_ms: k.createdAtMs ?? null }
    }
    case 'auth.verify_api_key':
      return await client.verifyApiKey(args.key)
    case 'blob.put':
      return client.blobPutObject(args.key, asBuffer(args.value), args.content_type ?? null, args.metadata ?? null)
    case 'blob.get':
      return await client.blobGetBytes(args.key) // Buffer | null
    case 'blob.head': {
      const i = await client.blobHead(args.key)
      return i == null
        ? null
        : { key: i.key, size: i.size, content_type: i.contentType, etag: i.etag, metadata: i.metadata, last_modified_ms: i.lastModifiedMs }
    }
    case 'blob.delete':
      return client.blobDelete(args.key)
    case 'blob.list': {
      const page = await client.blobList(args.prefix, args.cursor ?? null, args.limit ?? 100)
      return {
        keys: page.items.map((i) => i.key),
        items: page.items.map((i) => ({ key: i.key, size: i.size, content_type: i.contentType, etag: i.etag })),
        cursor: page.cursor ?? null,
      }
    }
    case 'blob.presign_download':
      return parsePresign(await client.blobPresignDownload(args.key, args.expires_seconds), args.key, 'GET')
    case 'blob.presign_upload':
      return parsePresign(await client.blobPresignUpload(args.key, args.expires_seconds, args.max_bytes), args.key, 'PUT')
    case 'blob.verify_presigned':
      return await client.blobVerifyPresign(args.method, args.key, args.expires_epoch, args.max_bytes, args.sig)
    case 'pubsub.publish':
      return client.pubsubPublish(args.topic, valueToString(args.payload))
    case 'pubsub.subscribe': {
      const sub = await client.pubsubSubscribe(args.topic)
      subscriptions.set(captureAs, sub)
      return { subscribed: true }
    }
    case 'pubsub.receive': {
      const sub = subscriptions.get(args.from)
      if (!sub) throw new Error('pubsub.receive: no subscription captured as ' + JSON.stringify(args.from))
      let timer
      const bounded = new Promise((resolve) => {
        timer = setTimeout(() => resolve(TIMEOUT), 2000)
      })
      const next = sub.next().then((b) => (b == null ? null : { $bytes: Array.from(b) }))
      const r = await Promise.race([next, bounded])
      clearTimeout(timer)
      return r === TIMEOUT ? { timeout: true } : r
    }
    default:
      throw new Error('node conformance runner has no dispatch for op ' + op)
  }
}

function typeMatches(t, a) {
  switch (t) {
    case 'string': return typeof a === 'string'
    case 'number': return typeof a === 'number'
    case 'boolean': return typeof a === 'boolean'
    case 'array': return Array.isArray(a)
    case 'object': return a != null && typeof a === 'object' && !Array.isArray(a)
    case 'null': return a == null
    default: throw new Error('unknown $type matcher ' + t)
  }
}
function deepMatch(exp, act) {
  if (typeof exp === 'string' && typeof act !== 'string') {
    const b = asBytes(act)
    if (b != null) return Buffer.from(b).toString('utf8') === exp
  }
  if (exp && typeof exp === 'object' && !Array.isArray(exp)) {
    if (typeof exp.$type === 'string') return typeMatches(exp.$type, act)
    if (typeof exp.$approx === 'number') {
      const tol = exp.tol ?? 0
      return typeof act === 'number' && Math.abs(act - exp.$approx) <= tol
    }
    if (Array.isArray(exp.$bytes)) {
      const a = asBytes(act)
      return a != null && a.length === exp.$bytes.length && a.every((x, i) => x === exp.$bytes[i])
    }
    if (act == null || typeof act !== 'object') return false
    return Object.keys(exp).every((k) => deepMatch(exp[k], act[k]))
  }
  if (Array.isArray(exp)) {
    return Array.isArray(act) && exp.length === act.length && exp.every((e, i) => deepMatch(e, act[i]))
  }
  return exp === act
}
function checkValue(exp, act) {
  if (typeof exp === 'string') {
    const b = asBytes(act)
    if (b != null) {
      const got = Buffer.from(b).toString('utf8')
      if (got !== exp) throw new Error(`expected ${JSON.stringify(exp)}, got ${JSON.stringify(got)}`)
      return
    }
  }
  if (!deepMatch(exp, act)) throw new Error(`expected ${JSON.stringify(exp)}, got ${JSON.stringify(act)}`)
}
function checkBytes(exp, act) {
  const a = asBytes(act)
  if (a == null) throw new Error('expected a byte value, got ' + JSON.stringify(act))
  const want = exp.$bytes
  if (a.length !== want.length || !want.every((x, i) => x === a[i])) {
    throw new Error(`byte mismatch: expected [${want}], got [${a}]`)
  }
}
async function check(expect, outcome) {
  if (typeof expect.error === 'string') {
    if (outcome.ok) throw new Error(`expected error ${expect.error}, got value ${JSON.stringify(outcome.value)}`)
    if (outcome.code !== expect.error) throw new Error(`expected error ${expect.error}, got ${outcome.code}`)
    return
  }
  if (!outcome.ok) throw new Error(`expected a value, got error ${outcome.code}`)
  if ('value' in expect) return checkValue(expect.value, outcome.value)
  if ('bytes' in expect) return checkBytes(expect.bytes, outcome.value)
  if ('shape' in expect) {
    if (!deepMatch(expect.shape, outcome.value)) {
      throw new Error(`shape mismatch: expected ${JSON.stringify(expect.shape)}, got ${JSON.stringify(outcome.value)}`)
    }
    return
  }
  throw new Error('expect block has none of value/bytes/shape/error')
}

function resolve(v, captures) {
  if (v && typeof v === 'object' && !Array.isArray(v)) {
    if (typeof v.$ref === 'string') {
      return v.$ref.split('.').reduce((acc, k) => acc[k], captures)
    }
    if (typeof v.$now_ms === 'number') {
      return Date.now() + v.$now_ms
    }
    const out = {}
    for (const [k, x] of Object.entries(v)) out[k] = resolve(x, captures)
    return out
  }
  if (Array.isArray(v)) return v.map((x) => resolve(x, captures))
  return v
}

async function runScenario(scenario) {
  return withTestDb(async (url) => {
    const clients = new Map()
    const captures = {}
    // Live pubsub subscriptions held across steps, keyed by a subscribe step's `as` name;
    // a later pubsub.receive reads the next message off the named handle.
    const subscriptions = new Map()
    for (let i = 0; i < scenario.steps.length; i++) {
      const step = scenario.steps[i]
      const ns = step.namespace ?? ''
      if (!clients.has(ns)) {
        clients.set(ns, await ForgeClient.connectWith(url, { kvNamespace: ns, signingSecret: SIGNING_SECRET }))
      }
      const client = clients.get(ns)
      const args = resolve(step.args ?? {}, captures)
      let outcome
      try {
        outcome = { ok: true, value: await dispatch(client, step.op, args, subscriptions, step.as ?? null) }
      } catch (e) {
        outcome = { ok: false, code: canonicalErrorCode(e), err: e }
      }
      if (step.as && outcome.ok) captures[step.as] = outcome.value
      if (step.expect) {
        try {
          await check(step.expect, outcome)
        } catch (e) {
          throw new Error(`step ${i} (${step.op}): ${e.message}`)
        }
      } else if (!outcome.ok) {
        throw new Error(`step ${i} (${step.op}): unexpected error ${outcome.code}`)
      }
    }
  })
}

function loadGaps() {
  const doc = JSON.parse(fs.readFileSync(GAPS_FILE, 'utf8'))
  const set = new Set()
  for (const g of doc.gaps) {
    if (g.languages.includes(LANG)) set.add(g.primitive + '/' + g.scenario)
  }
  return set
}

async function main() {
  const gaps = loadGaps()
  const problems = []
  let passed = 0
  const files = fs.readdirSync(SCENARIO_DIR).filter((f) => f.endsWith('.json')).sort()
  for (const file of files) {
    const doc = JSON.parse(fs.readFileSync(path.join(SCENARIO_DIR, file), 'utf8'))
    for (const scenario of doc.scenarios) {
      // A scenario may pin itself to a subset of backends (e.g. scheduler->queue delivery is
      // Postgres-only); the binding runs against Postgres, so skip ones that exclude it.
      if (Array.isArray(scenario.backends) && !scenario.backends.includes(BACKEND)) continue
      const key = doc.primitive + '/' + scenario.name
      const expectedFail = gaps.has(key)
      let err = null
      try {
        await runScenario(scenario)
      } catch (e) {
        err = e
      }
      if (!err && !expectedFail) { passed++; console.log('PASS  ' + key) }
      else if (err && expectedFail) { passed++; console.log('XFAIL ' + key + ': ' + err.message) }
      else if (!err && expectedFail) problems.push(`${key}: PASSED but is a registered node gap; remove it from known_gaps.json`)
      else problems.push(`${key}: ${err.message}`)
    }
  }
  try {
    await runBackendSelectionSmoke()
    passed++
  } catch (e) {
    problems.push('backend/env_backend_selection: ' + e.message)
  }
  console.log(`\nconformance(node): ${passed} ok, ${problems.length} unexpected`)
  if (problems.length) {
    console.error('unexpected conformance results:\n  ' + problems.join('\n  '))
    process.exit(1)
  }
}

main().then(() => process.exit(0)).catch((e) => { console.error(e); process.exit(1) })

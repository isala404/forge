# Forge — Node reference

The package is `forgelib`. Raw contract methods live directly on `ForgeClient` in
flat camelCase and return a `Promise`. Native JSON handles also hang off the client
(`forge.queue<T>()`, `forge.kv<T>()`, `forge.config<T>()`, `forge.topic<T>()`) for
app payloads. Optional trailing raw arguments are positional — pass `null` (or
`undefined`) to skip one and set a later one. Verified against
`bindings/node/client.d.ts`.

```ts
import { ForgeClient } from "forgelib";
const forge = await ForgeClient.init();          // reads ./forge.toml
const forge2 = await ForgeClient.initFrom("svc/forge.toml");
```

## Key/value

| Method | Notes |
| --- | --- |
| `kvGet()` | Value as a UTF-8 string, or `null`. Lossy for binary — use bytes. |
| `kvGetBytes()` | Value as a `Buffer`, or `null`. Lossless. |
| `kvSet()` | `kvSet(key, value, ttlSeconds?, ifNotExists?, ifExists?)`. Returns whether it wrote. |
| `kvSetBytes()` | `kvSetBytes(key, buf, ttlSeconds?, ifNotExists?)`. |
| `kvMget()` | One round-trip for many keys; result is per-key `string \| null`. |
| `kvIncr()` | `kvIncr(key, by)` → new value as a JS `number` (f64; see pitfalls). |
| `kvDelete()` | Returns whether the key existed. |
| `kvExists()` | Presence (and unexpired). |
| `kvExpire()` | `kvExpire(key, ttlSeconds)`; `false` if the key is absent. |
| `kvCompareAndSwap()` | `kvCompareAndSwap(key, old, newValue)`; `old = null` means "expected absent" — a default value never matches a missing key. |
| `kvScan()` | First page only: `kvScan(prefix, limit)` → `string[]`. |
| `kvScanPage()` | `kvScanPage(prefix, cursor, limit)` → `{ keys, cursor }`; `cursor` is absent (`undefined`) when done — loop while it's truthy. |

## Queue

| Method | Notes |
| --- | --- |
| `queueEnqueue()` | `queueEnqueue(queue, payload, maxAttempts?, dedupId?, delaySeconds?)` → job id. A `dedupId` seen in the last 5 min returns the existing job's id (no error). |
| `queueDequeue()` | `queueDequeue(queue, visibilitySeconds, waitSeconds)` → `JsJob \| null` (long-polls). |
| `queueAck()` | Ack by `receipt` (idempotent). |
| `queueNack()` | `queueNack(receipt, retrySeconds?)`. Throws `PRECONDITION` if the receipt is unknown. |
| `queueHeartbeat()` | Extend the lease by one visibility window; throws `PRECONDITION` if the lease is lost. |
| `queueDepth()` | `{ visible, inFlight, delayed }`. Pass `"<queue>.dlq"` to gauge the dead-letter backlog. |

Settle a job by its delivery-unique `receipt`, never its `id` (the `id` is stable
across redeliveries — that is your idempotency key).

## Pub/sub

| Method | Notes |
| --- | --- |
| `pubsubPublish()` | `pubsubPublish(topic, payload)`, fire-and-forget, at-most-once. |
| `pubsubSubscribe()` | Returns a `JsSubscription`; loop `next()` until it resolves `null`. |
| `pubsubChannel()` | The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. |

`JsSubscription` has `next()` (→ `Buffer \| null`) and `close()` (unsubscribe now —
call it when a client's socket closes; it also resolves any pending `next()` to
`null`). `pubsubPublish` takes a string; `next()` yields a `Buffer` — `toString()`
before parsing. Publishes after `subscribe()` resolves are delivered; earlier ones
are gone.

## Blob

| Method | Notes |
| --- | --- |
| `blobPut()` | `blobPut(key, data, contentType?)` — `data` is a string. |
| `blobPutBytes()` | `blobPutBytes(key, buf, contentType?)`. |
| `blobPutObject()` | `blobPutObject(key, buf, contentType?, metadata?)` when you also need user metadata. |
| `blobGet()` | Object as a UTF-8 string, or `null`. |
| `blobGetBytes()` | Object as a `Buffer`, or `null`. |
| `blobHead()` | `JsBlobInfo` (size, contentType, etag, lastModifiedMs, metadata), or `null`. |
| `blobList()` | `blobList(prefix, cursor, limit)` → `{ items, cursor }`. |
| `blobContentType()` | Stored content type, or `null`. |
| `blobDelete()` | Returns whether it existed. |
| `blobPresignDownload()` | `blobPresignDownload(key, expiresSeconds)` — needs `[blob].signing_secret`. |
| `blobPresignUpload()` | `blobPresignUpload(key, expiresSeconds, maxBytes)` — needs the secret. |
| `blobVerifyPresign()` | `blobVerifyPresign(method, key, expiresEpoch, maxBytes, sig)` → validity. |

Presigned URLs are relative paths (`/api/files?key=…&expires=…&max_bytes=…&sig=…`),
not absolute URLs: mount your own route and pass the query params to
`blobVerifyPresign` verbatim (a download URL carries `max_bytes=0` — echo it, don't
substitute). A method mismatch verifies to `false` rather than throwing.

## Auth

| Method | Notes |
| --- | --- |
| `hashPassword()` | argon2id PHC string to store in your users table. |
| `verifyPassword()` | `verifyPassword(plain, hash)`, constant-time. |
| `needsRehash()` | After a successful verify, re-hash if `true` (params below baseline). Synchronous. |
| `createSession()` | `createSession(userId, idleSeconds?, absoluteSeconds?)` → opaque token (shown once). |
| `validateSession()` | Token → `userId`, or `null`. |
| `validateSessionInfo()` | Token → `JsSession` (userId + times), or `null`. |
| `revokeSession()` | Log out one device (idempotent). |
| `revokeAllSessions()` | Log out everywhere; returns the count. |
| `createApiKey()` | `createApiKey(ownerId, label)` → `JsApiKey` (`secret` shown once). |
| `verifyApiKey()` | Key → `ownerId`, or `null`. |
| `verifyApiKeyInfo()` | Key → `JsApiKeyInfo` (id, owner, label), or `null`. |
| `revokeApiKey()` | Revoke by non-secret id. |
| `createToken()` | `createToken(userId, purpose, ttlSeconds)` → single-use token (shown once). `purpose` is any string you choose (`"password-reset"`); create/consume must match exactly. `userId` is any opaque string handed back on consume — for pre-account flows (invites) pass your own reference id. |
| `consumeToken()` | `consumeToken(token, purpose)` → `userId`, or `null` (used/expired/wrong purpose). First consume wins. |

## Rate limit

| Method | Notes |
| --- | --- |
| `rateLimitCheck()` | `rateLimitCheck(bucket, key, max, perSeconds, failOpen?, algo?)` → `JsDecision`. `algo` is `"token_bucket"` (default) or `"sliding_window"`. `perSeconds` must be ≥ 1 (a sub-second window is `INVALID`). |

`JsDecision`: `{ allowed, limit, remaining, resetAfterSeconds, retryAfterSeconds? }`.
Pass `false` for `failOpen` on credential, password-reset, invite, and API-key
verification paths. A limiter outage should deny those security-sensitive requests;
reserve fail-open behavior for explicitly low-risk, availability-first traffic.

## Schedule

| Method | Notes |
| --- | --- |
| `scheduleCron()` | `scheduleCron(name, expr, queue, payload, maxAttempts?)`; upsert by name. |
| `scheduleAt()` | `scheduleAt(whenEpochMs, queue, payload, maxAttempts?)` → future job id. `payload` is a raw string, passed through — `JSON.stringify` it yourself if a JSON-handle worker consumes the queue. |
| `scheduleCancel()` | Cancel a cron by name. |
| `scheduleCancelAt()` | Cancel a one-shot by the id `scheduleAt` returned (send-later recall). |
| `scheduleList()` | `scheduleList(cursor?, limit?)` → `{ items, cursor }`. |
| `runSchedulerOnce()` | Fire all due schedules once; returns jobs enqueued. Call on an interval. |

## Config and flags

| Method | Notes |
| --- | --- |
| `configGet()` | Resolve a value: env `FORGE_CFG_<KEY>` > store > `null`. |
| `configSet()` | `configSet(key, value)`. |
| `configDelete()` | Delete a stored value; env `FORGE_CFG_<KEY>` still shadows reads. |
| `flag()` | `flag(key, defaultValue, targetingKey?)`. Never throws; falls back to the default. |
| `setFlagPercent()` | Percentage rollout, `0..=100`. Bucketing is a stable hash of `(flag, targetingKey)` — deterministic across processes; without a `targetingKey`, a percent rule returns the caller default. |
| `setFlagOn()` / `setFlagOff()` | Always-on / always-off. |
| `setFlagAllowList()` | `setFlagAllowList(key, entries)` — on only for those targeting keys. |
| `deleteFlag()` | Delete a flag rule; later `flag()` calls use the caller default. |

## Client

| Method | Notes |
| --- | --- |
| `backendReport()` | Which provider powers each primitive (for a health page). Synchronous. |
| `postgresUrl()` | The resolved system DSN (embedded servers mint theirs at init) for the app's own pool. Contains credentials. Synchronous. |
| `maintain()` | One housekeeping sweep (expired kv, settled/dead jobs, stale buckets, expired sessions). |

## Native JSON handles

Bind a JSON codec once instead of stringifying at every call site. These are exported
from the main `forgelib` package and installed as methods on `ForgeClient`.

```ts
import { ForgeClient, forgeErrorCode, forgeErrorRetryable } from "forgelib";

const forge = await ForgeClient.init();

const emails = forge.queue<{ to: string }>("emails");
await emails.enqueue({ to: "a@b.c" }, { maxAttempts: 5 });

const profile = forge.kv<Profile>(`user:${id}`);
await profile.set(value, { ttlSeconds: 3600 });
```

- `Queue<T>` from `forge.queue<T>(name)`: `enqueue()`, `dequeue()`, `ack()`, `nack()`, `heartbeat()`, `depth()`, `worker()`. A handle/worker `job.payload` is ALREADY codec-decoded (an object) — `JSON.parse`-ing it again throws.
- `KvKey<T>` from `forge.kv<T>(key)`: `get()`, `getOrDefault(default)`, `set()`, `delete()`, `exists()`, `expire()`, `compareAndSwap()`.
- `ConfigKey<T>` from `forge.config<T>(key, defaultValue)`: `get()`, `getOrDefault()`, `set()`.
- `Topic<T>` from `forge.topic<T>(name)`: `publish()`, `subscribe()` (an `AsyncIterable<T>`; its iterator's `next()` resolves `{ value, done }`, unlike the raw `JsSubscription.next()` which resolves the payload directly).
- `forge.worker<T>(queue, handler, opts?)` / `runWorker(client, queue, handler, opts?)` — managed loop. Keep its `Promise`; abort `opts.signal` to begin draining, then await the promise before exiting. Calling `process.exit()` immediately after abort defeats the drain.
- `forgeErrorCode(err)` / `forgeErrorRetryable(err)` — parse the code Forge prefixes onto
  the message; a retryable backend error is prefixed `BACKEND(retryable):` and
  `forgeErrorRetryable` reports it (alongside `UNAVAILABLE`).

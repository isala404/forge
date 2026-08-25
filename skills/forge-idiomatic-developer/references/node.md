# Forge — Node reference

The package is `forgelib`. Raw contract methods live directly on `ForgeClient` in flat camelCase. Most are asynchronous, but methods explicitly marked **synchronous** below must not be awaited. Native JSON handles also hang off the client (`forge.queue<T>()`, `forge.kv<T>()`, `forge.config<T>()`, `forge.topic<T>()`) for app payloads. Optional trailing raw arguments are positional — pass `null` (or `undefined`) to skip one and set a later one. Check the installed `client.d.ts`, `index.d.ts`, and JavaScript helpers when exact behavior matters.

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
| `queueEnqueue()` | `queueEnqueue(queue, payload, maxAttempts?, dedupId?, delaySeconds?)` → job id. A `dedupId` seen inside the configured dedup window returns the existing id (no error); the default is 300 seconds. |
| `queueDequeue()` | `queueDequeue(queue, visibilitySeconds, waitSeconds)` → `JsJob \| null` (long-polls). |
| `queueAck()` | Ack by `receipt` (idempotent). |
| `queueNack()` | `queueNack(receipt, retrySeconds?)`. Throws `PRECONDITION` if the receipt is unknown. |
| `queueHeartbeat()` | Extend the lease by one visibility window; throws `PRECONDITION` if the lease is lost. |
| `queueDepth()` | Approximate point-in-time `{ visible, inFlight, delayed }`. Pass `"<queue>.dlq"` to gauge the dead-letter backlog. |

Settle a job by its delivery-unique `receipt`, never its `id`. The stable id is useful for redelivery idempotency, but scope the final key to the logical effect (for example, `jobId:recipientId`) or use a domain operation id when duplicate jobs are possible.

## Pub/sub

| Method | Notes |
| --- | --- |
| `pubsubPublish()` | `pubsubPublish(topic, payload)`, fire-and-forget, at-most-once. |
| `pubsubSubscribe()` | Returns a `JsSubscription`; loop `next()` until it resolves `null`. |
| `pubsubChannel()` | Backend channel mapping. With the Postgres pub/sub backend, this is its `LISTEN`/`NOTIFY` channel. **Synchronous.** |

`JsSubscription` has `next()` (→ `Buffer \| null`) and `close()` (unsubscribe now — call it when a client's socket closes; it also resolves any pending `next()` to `null`). `pubsubPublish` takes a string; `next()` yields a `Buffer` — `toString()` before parsing. Publishes after `subscribe()` resolves are delivered; earlier ones are gone. Payloads must be valid UTF-8 and at most 7,000 bytes.

## Blob

| Method | Notes |
| --- | --- |
| `blobPut()` | `blobPut(key, data, contentType?)` — `data` is a string. |
| `blobPutBytes()` | `blobPutBytes(key, buf, contentType?)`. |
| `blobPutObject()` | `blobPutObject(key, buf, options?)`; options include content type, metadata, conditions, web headers, SHA-256, and S3 encryption. |
| `blobPutFile()` | Streams a file path without buffering it in JavaScript. |
| `blobGet()` | Object as a UTF-8 string, or `null`. |
| `blobGetBytes()` | Object as a `Buffer`, or `null`. |
| `blobGetRange()` | Inclusive bounded byte range as a `Buffer`, or `null`. |
| `blobHead()` | `JsBlobInfo` (size, contentType, etag, lastModifiedMs, metadata), or `null`. |
| `blobList()` | `blobList(prefix, cursor, limit)` → `{ items, cursor }`. |
| `blobContentType()` | Stored content type, or `null`. |
| `blobDelete()` | Idempotent; resolves with no existence claim. |
| `blobPresignDownload()` | Returns `JsProxyPresign`; use `.url`. Needs `[blob].signing_secret`. |
| `blobPresignUpload()` | Returns a size-enforcing `JsProxyPresign`; use `.url`. |
| `blobPresignNativeGet()` / `blobPresignNativePut()` | S3-only provider presigns with required headers; native PUT has no portable size cap. |
| `blobVerifyPresign()` | `blobVerifyPresign(method, key, expiresEpoch, maxBytes, sig)` → validity. |

Proxy presigned URLs include a version, namespace, method, key, expiry, size constraint, and signature under `[blob].base_url`. Mount a matching route and pass the structured ticket fields to `blobVerifyPresign` verbatim. Expiry or a bad signature returns `false`; an unsupported method throws `INVALID`. Proxy and native presigned URLs are bearer credentials, so redact their query strings.

## Auth

| Method | Notes |
| --- | --- |
| `hashPassword()` | argon2id PHC string to store in your users table. |
| `verifyPassword()` | `verifyPassword(plain, hash)`; a malformed stored hash throws `INVALID`, not `false`. |
| `needsRehash()` | After a successful verify, re-hash if `true` (params below baseline). Synchronous. |
| `createSession()` | `createSession(userId, idleSeconds?, absoluteSeconds?)` → opaque token (shown once). |
| `validateSession()` | Token → `userId`, or `null`. |
| `validateSessionInfo()` | Token → `JsSession` (userId + times), or `null`. |
| `revokeSession()` | Log out one device (idempotent). |
| `revokeAllSessions()` | Log out everywhere; returns the count. |
| `createApiKey()` | `createApiKey(ownerId, label)` → `JsApiKey` (`secret` shown once). |
| `verifyApiKey()` | Key → `JsApiKeyInfo` (id, owner, label, expiry, scopes, metadata), or `null`. |
| `revokeApiKey()` | Revoke by non-secret id. |
| `createToken()` | `createToken(userId, purpose, ttlSeconds)` → single-use token (shown once). `purpose` is any string you choose (`"password-reset"`); create/consume must match exactly. `userId` is any opaque string handed back on consume — for pre-account flows (invites) pass your own reference id. |
| `consumeToken()` | `consumeToken(token, purpose)` → `userId`, or `null` (used/expired/wrong purpose). First consume wins. |

## Rate limit

| Method | Notes |
| --- | --- |
| `rateLimitCheck()` | `rateLimitCheck(bucket, key, max, perSeconds, failOpen?, algo?)` → `JsDecision`. `algo` is `"token_bucket"` (default) or `"sliding_window"`. `perSeconds` must be ≥ 1 (a sub-second window is `INVALID`). |

`JsDecision`: `{ allowed, limit, remaining, resetAfterSeconds, retryAfterSeconds? }`. Run credential limits before password work and choose `failOpen` deliberately. Pass `false` when bypass creates unacceptable security/financial risk; pass `true` with monitoring and defense in depth when availability takes precedence.

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
| `setFlagValue()` / `flagDetails()` | Store typed JSON and return its value type, stable variant, reason, and default/error reason. |
| `deleteFlag()` | Delete a flag rule; later `flag()` calls use the caller default. |

## Client

| Method | Notes |
| --- | --- |
| `backendCapabilities()` | Static provider and durability capabilities for each primitive. **Synchronous.** |
| `postgresUrl()` | Resolved Forge system DSN. Contains credentials. Use server-side to reach embedded Postgres from another process or intentionally seed an application-owned pool; choose production isolation deliberately. **Synchronous.** |
| `maintain()` | One housekeeping sweep (expired kv, settled/dead jobs, stale buckets, expired sessions). |

## Native JSON handles

Bind a JSON codec once instead of stringifying at every call site. These are exported from the main `forgelib` package and installed as methods on `ForgeClient`.

```ts
import { ForgeClient, forgeErrorCode, forgeErrorRetryable } from "forgelib";

const forge = await ForgeClient.init();

const emails = forge.queue<{ to: string }>("emails");
await emails.enqueue({ to: "a@b.c" }, { maxAttempts: 5 });

const profile = forge.kv<Profile>(`user:${id}`);
await profile.set(value, { ttlSeconds: 3600 });
```

- `Queue<T>` from `forge.queue<T>(name)`: `enqueue()`, `dequeue()`, `depth()`, `worker()`. A dequeued `QueueJob<T>` owns `ack()`, `nack()`, and `heartbeat()`. A handle/worker `job.payload` is ALREADY codec-decoded (an object) — `JSON.parse`-ing it again throws.
- `KvKey<T>` from `forge.kv<T>(key)`: `get()`, `getOrDefault(default)`, `set()`, `delete()`, `exists()`, `expire()`, `compareAndSwap()`.
- `ConfigKey<T>` from `forge.config<T>(key, defaultValue)`: `get()` returns the bound default when supplied and the key is missing; without a bound default it returns `null`, matching the declared `T | null`. `getOrDefault()` can override the bound default. It also has `set()`, `delete()`, and `flag()`.
- `Topic<T>` from `forge.topic<T>(name)`: `publish()`, asynchronous `subscribe()`, and synchronous `channel()`. Await the subscription before iterating: `for await (const event of await topic.subscribe())`. Its iterator's `next()` resolves `{ value, done }`, unlike the raw `JsSubscription.next()` which resolves the payload directly.
- `forge.worker<T>(queue, handler, opts?)` / `runWorker(client, queue, handler, opts?)` — managed loop. Keep its `Promise`. `forge.close(deadlineSeconds)` stops the loop, aborts `job.signal` for an active handler, releases its lease, and awaits helper cleanup before closing native resources. Await close before process exit.
- `forgeErrorCode(err)` / `forgeErrorRetryable(err)` — parse the code Forge prefixes onto the message; a retryable backend error is prefixed `BACKEND(retryable):` and `forgeErrorRetryable` reports it (alongside `UNAVAILABLE`).

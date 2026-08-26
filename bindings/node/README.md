# forgelib

Node.js bindings for [Forge](../..) via [napi-rs](https://napi.rs). A native addon exposing the full primitive surface (kv, queue, config, ratelimit, blob, auth, schedule, pubsub), capability inspection, readiness probes, per-instance metrics, and W3C queue trace propagation. Async Rust methods become JS `Promise`s; method names are camelCased.

Scheduler methods expose UTC schedule inspection, pause/resume, due lag/count, last successful tick, enqueue failures, and explicit bounded misfire policies. The scheduler enqueues ordinary deterministic queue jobs and does not own workflow state.

The top-level `encodeCloudEvent`/`decodeCloudEvent` and `importEnvConfig`/`exportEnvConfig` helpers are state-free. They add no transport, framework, or environment mutation to the native client; see the repository's integration recipes for Axum, Hono, Express, FastAPI, Django, Go HTTP, deployment, and MCP patterns.

The `1.x` npm package ships prebuilt addons for Linux x64 (glibc and musl), Linux arm64 (glibc), macOS arm64, and Windows x64. Intel macOS is not a supported prebuilt target.

## Build

The platform binary (`*.node`) is not committed; build it from source:

```sh
npm install          # installs @napi-rs/cli
npm run build:debug  # or `npm run build` for a release binary
```

This produces `forgelib.<platform>.node` next to the committed `index.js` / `index.d.ts` (the generated JS entry + TypeScript types).

## Use

Configuration lives in a `forge.toml` at the project root; `init()` reads it and instantiates the runtime. A minimal one:

```toml
[postgres]
url = "${DATABASE_URL:-postgres://localhost/myapp}"
```

```ts
import { ForgeClient } from 'forgelib';

const forge = await ForgeClient.init(); // reads ./forge.toml

// kv (Redis lineage), with a native JSON handle for app records
const profile = forge.kv<{ name: string }>('user:42:profile');
await profile.set({ name: 'Ada' }, { ttlSeconds: 3600 });
const user = await profile.get();

await forge.kvIncr('clicks:42', 1);

// queue (SQS lineage)
const emails = forge.queue<{ to: string }>('emails');
const id = await emails.enqueue({ to: 'a@b.c' }, { maxAttempts: 3 });
const job = await emails.dequeue({ visibilitySeconds: 30, waitSeconds: 1 });
if (job) {
  // job.payload is { to: string }
  await emails.ack(job.receipt);   // or emails.nack(job.receipt)
}
```

Leased jobs are held Rust-side and settled by `receipt`; the stable `id` remains the job's idempotency key and the opaque lease fence never crosses into JS. Every per-deployment knob (namespace, pool size, blob backend, ...) lives in `forge.toml`; `ForgeClient.initFrom(path)` loads a file outside the current directory. See `index.d.ts` for the full TypeScript surface, including the raw 1:1 methods (`kvSet`, `queueEnqueue`, ...) and the typed handles below.

### Native typed handles

The main `forgelib` import binds names to JSON value types directly on the client:

```ts
import { ForgeClient, forgeErrorCode } from 'forgelib';

interface SendEmail { to: string; template: string }
const emails = forge.queue<SendEmail>('emails');
await emails.enqueue({ to: 'a@b.c', template: 'welcome' }, { maxAttempts: 3 });
const job = await emails.dequeue({ waitSeconds: 1 });
if (job) { handle(job.payload); await emails.ack(job.receipt); }

// forgeErrorCode(e) -> 'INVALID' | 'LIMIT' | ... parses the code Forge prefixes
// onto the thrown error's message.
```

The raw methods are still present for string/byte contracts and exact cross-language parity. Use `forge.kv<T>(key)`, `forge.queue<T>(name)`, `forge.config<T>(key, default)`, and `forge.topic<T>(name)` when the payload is app JSON.

## Deterministic tests

`ForgeClient.initMemoryForTesting(toml, startMs, seed)` creates the normal memory profile with manual time and repeatable test-only entropy. Call `forge.advanceTestClock(seconds)` to drive expiry, delayed work, scheduling, and rate-limit refill without sleeping. The seeded tokens are predictable and must never leave tests.

Application-owned names can use the v1 helper: `scopeKvKey("billing", tenantId, userId, invoiceId)`. `parseScopedName` reverses the length-prefixed encoding. This is a naming aid only; authorize every component before calling Forge.

## OpenFeature, bulk reads, and snapshots

Install the official `@openfeature/server-sdk`, then import `ForgeProvider` and `telemetryHook` from `forgelib/openfeature`. The provider preserves stable variants and reasons, installs no global hooks, and exposes the official OpenTelemetry event hook for application-scoped registration. Startup code can use `configGetMany` and `flagDetailsMany` for ordered 256-item reads. `configSnapshot` returns a frozen, expiring view containing only requested keys; declare `no_secrets` or protect the encoded snapshot outside the trusted server boundary.

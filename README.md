# Forge

[![CI](https://img.shields.io/github/actions/workflow/status/isala404/forge/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/isala404/forge/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/forgelib?style=flat-square&logo=rust&logoColor=white)](https://crates.io/crates/forgelib) [![npm](https://img.shields.io/npm/v/forgelib?style=flat-square&logo=npm&logoColor=white)](https://www.npmjs.com/package/forgelib) [![PyPI](https://img.shields.io/pypi/v/forgelib?style=flat-square&logo=pypi&logoColor=white)](https://pypi.org/project/forgelib/) [![PostgreSQL 18](https://img.shields.io/badge/PostgreSQL-18-4169E1?style=flat-square&logo=postgresql&logoColor=white)](https://www.postgresql.org/) [![MIT License](https://img.shields.io/github/license/isala404/forge?style=flat-square&color=blue)](./LICENSE)

Forge is a backend infrastructure library for Rust, Node.js and Bun, Python, and Go. It provides queues, key/value storage, blobs, authentication helpers, rate limits, scheduling, configuration, and pub/sub through one versioned contract.

Most applications need the same backend plumbing:

- Redis for caching and sessions
- a queue for background jobs
- object storage for uploads
- an auth service for logins and sessions
- a cron runner for scheduled work
- a rate limiter

Each separate service adds another deployment, security boundary, test double, and failure mode.

Forge keeps these primitives inside the backend process. PostgreSQL is the shared durable backend, memory is the process-local test backend, filesystem storage is available for local blob data, and S3-compatible storage handles larger blob workloads. Forge 1.1 is the compatibility baseline; later 1.x releases move in lockstep and reserve breaking changes for 2.0.

## Ownership boundary

Forge owns its public contract, backend implementations, schema and migration validation, bounded payload formats, worker leases, retries, lifecycle, diagnostics, metrics, and trace propagation.

The application owns HTTP or RPC routes, business data, authorization policy, worker handlers, process signals, deployment, MCP transport, and observability export. It also owns every frontend choice: framework, browser transport, WebSocket or SSE client, cache, TanStack Query setup, optimistic state, reconnect behavior, and refetch policy. Forge can encode and publish a bounded server-side invalidation hint, but the application decides whether and how that hint reaches a client.

The exact consistency, durability, ordering, expiry, and retry rules are in the [semantics guide](https://tryforge.dev/semantics/). Security-sensitive ownership is in the [security guide](https://tryforge.dev/security/).

## One connection, eight backend primitives

```bash
npm install forgelib
```

Configuration lives in a `forge.toml` at your project root. `init()` reads it and instantiates the runtime; string values may reference the environment as `${VAR}`:

```toml
[postgres]
url = "${DATABASE_URL:-}"
embedded = true
```

Set `DATABASE_URL` to use a managed or shared PostgreSQL 18 server. Leave it unset and Forge downloads and runs PostgreSQL 18 for you (data persists in `.forge/pg`) — built into the Node and Python packages, behind the `embedded` cargo feature in Rust.

Omit settings you do not need; Forge applies production-safe defaults for the rest.

Development and test startup migrate automatically. Production defaults to validation-only startup: run `ForgeClient.migrate()` (or the Rust, Python, or Go equivalent) in the deployment job, require every structured report to be `applied`, then start the application. `migration_lock_timeout_secs` bounds advisory-lock waiting, and [`contract/schema-ownership.json`](contract/schema-ownership.json) lists every Forge-owned object.

```ts
import { ForgeClient } from "forgelib";

const forge = await ForgeClient.init(); // instantiates the runtime from ./forge.toml

// auth: argon2 password hashing, sessions, one-time tokens (password reset, magic links)
const hash = await forge.hashPassword(password);
const session = await forge.createSession(userId);
const reset = await forge.createToken(userId, "password-reset", 900); // consumeToken later, once

// rate limit: 20 attempts per minute, keyed by email
const limit = await forge.rateLimitCheck("login", email, 20, 60);
if (!limit.allowed) throw new Error("slow down");

// key/value: TTL cache and atomic counters
await forge.kv<typeof profile>(`user:${userId}`).set(profile, { ttlSeconds: 3600 });
const views = await forge.kvIncr(`views:${userId}`, 1);

// queue: background jobs
await forge.queue<{ to: string }>("emails").enqueue({ to: email });

// pub/sub: fan an event out to subscribers
await forge.topic<{ userId: string }>("user.created").publish({ userId });

// blob: store a file, hand back a link that expires in an hour
await forge.blobPut(`exports/${userId}.csv`, csv, "text/csv");
const link = (await forge.blobPresignDownload(`exports/${userId}.csv`, 3600)).url;

// schedule: recurring work on a cron
await forge.scheduleCron("nightly-report", "0 0 * * *", "reports", "{}");

// feature flags: roll a feature out to 25% of users
await forge.setFlagPercent("new-ui", 25);
const newUi = await forge.flag("new-ui", false, userId);
```

<details>
<summary>The same in Rust</summary>

```bash
cargo add forgelib
```

```rust
use std::time::Duration;
use forgelib::{
    Forge, Bytes, SetOpts, EnqueueOpts, PutOpts,
    ScheduleOpts, SessionOpts, Limit, FlagRule, EvalCtx,
};

let forge = Forge::init().await?; // instantiates the runtime from ./forge.toml

// auth
let hash = forge.auth().hash_password(&password).await?;
let session = forge.auth().create_session(&user_id, SessionOpts::default()).await?;
let reset = forge.auth().create_token(&user_id, "password-reset", Duration::from_secs(900)).await?;

// rate limit
let limit = forge.ratelimit()
    .check("login", &email, Limit::per_duration(20, Duration::from_secs(60)))
    .await?;

// key/value
forge.kv().set(&format!("user:{user_id}"), profile.into(),
    SetOpts::new().with_ttl(Duration::from_secs(3600))).await?;
let views = forge.kv().incr(&format!("views:{user_id}"), 1).await?;

// queue
forge.queue().enqueue("emails", payload.into(), EnqueueOpts::new()).await?;

// pub/sub
forge.pubsub().publish("user.created", Bytes::from(event)).await?;

// blob
forge.blob().put(&key, body, PutOpts::new()).await?;
let link = forge.blob().presign_download(&key, Duration::from_secs(3600)).await?.url;

// schedule
forge.schedule().cron("nightly-report", "0 0 * * *", "reports", Bytes::new(), ScheduleOpts::new()).await?;

// feature flags
forge.config().set_flag("new-ui", FlagRule::Percent(25)).await?;
let new_ui = forge.config().flag("new-ui", false, &EvalCtx::user(user_id)).await;
```
</details>

<details>
<summary>The same in Go</summary>

```bash
go get github.com/isala404/forge/bindings/go
```

```go
import (
    "context"
    "time"

    forge "github.com/isala404/forge/bindings/go"
)

client, err := forge.InitFrom(context.Background(), "forge.toml")
if err != nil { return err }
defer client.Close(context.Background())

hash, err := client.HashPassword(context.Background(), password)
session, err := client.CreateSession(context.Background(), userID, forge.SessionOptions{})
reset, err := client.CreateToken(context.Background(), userID, "password-reset", 15*time.Minute)

limit, err := client.RateLimitCheck(context.Background(), "login", email, forge.RateLimitOptions{Max: 20, Per: time.Minute})
if !limit.Allowed { return errTooManyRequests }

ttl := time.Hour
_, err = client.KVSet(context.Background(), "user:"+userID, profile, forge.SetOptions{TTL: &ttl})
views, err := client.KVIncr(context.Background(), "views:"+userID, 1)
_, err = client.Enqueue(context.Background(), "emails", payload, forge.EnqueueOptions{})
err = client.Publish(context.Background(), "user.created", event)
err = client.BlobPut(context.Background(), "exports/"+userID+".csv", csv, forge.PutOptions{ContentType: "text/csv"})
err = client.ScheduleCron(context.Background(), "nightly-report", "0 0 * * *", "reports", nil, forge.ScheduleOptions{})
err = client.SetFlag(context.Background(), "new-ui", forge.FlagRule{Kind: forge.FlagPercent, Percent: 25})
newUI := client.Flag(context.Background(), "new-ui", false, userID)
```
</details>

<details>
<summary>The same in Python</summary>

```bash
pip install forgelib
```

```python
import forgelib

forge = await forgelib.ForgeClient.init()  # instantiates the runtime from ./forge.toml

# auth
hash = await forge.hash_password(password)
session = await forge.create_session(user_id)
reset = await forge.create_token(user_id, "password-reset", 900)  # consume_token later, once

# rate limit
limit = await forge.rate_limit_check("login", email, 20, 60)
if not limit.allowed:
    raise RuntimeError("slow down")

# key/value
await forge.kv(f"user:{user_id}").set(profile, ttl_seconds=3600)
views = await forge.kv_incr(f"views:{user_id}", 1)

# queue
await forge.queue("emails").enqueue({"to": email})

# pub/sub
await forge.topic("user.created").publish({"user_id": user_id})

# blob
await forge.blob_put(f"exports/{user_id}.csv", csv, "text/csv")
link = (await forge.blob_presign_download(f"exports/{user_id}.csv", 3600)).url

# schedule
await forge.schedule_cron("nightly-report", "0 0 * * *", "reports", "{}")

# feature flags
await forge.set_flag_percent("new-ui", 25)
new_ui = await forge.flag("new-ui", False, user_id)
```
</details>

## What you get

| Primitive | What it does |
| --- | --- |
| key/value | get, set, mget, incr, compare-and-swap, prefix scan, TTLs |
| queue | deterministic enqueue, managed workers, retries, dead-letter operations, depth/age, transactional outbox relay |
| pub/sub | publish and subscribe on topics |
| blob | put, get, delete, presigned upload and download URLs |
| auth | password hashing, sessions, API keys |
| rate limit | token-bucket or sliding-window checks |
| schedule | cron and one-off jobs |
| config | settings and feature flags with percentage rollout |

Typed flags include stable variants and OpenFeature evaluation details. Official provider adapters ship for Rust, Node, Python, and Go, with application-scoped OpenTelemetry evaluation hooks. Bulk reads avoid per-key startup queries, while explicitly expiring snapshots support bounded disconnected reads without turning stale config into a silent source of truth.

Schedules use canonical UTC instants, deterministic occurrence job IDs, named pause/resume/inspect controls, and bounded diagnostics. Late work follows an explicit `skip`, `run_once` (default), or `catch_up` policy capped at 100; Forge does not model workflow dependencies or multi-step state.

State-free CloudEvents 1.0 structured-JSON and explicit environment alias adapters ship in all four languages. Framework, deployment, MCP, artifact, cancellation, trace-correlation, and authenticated diagnostics patterns live in the [integration recipes](https://tryforge.dev/integrations/) so protocol and runtime dependencies stay outside Forge core.

By default every primitive runs on one Postgres database, so there's nothing else to operate. When measurements justify a bulkhead, a hot primitive can use a separate PostgreSQL target while keeping the same coordinated schema and behavior.

## Test on memory, ship on Postgres

The explicit memory and PostgreSQL profiles pass the same conformance suite, so tests run in-process without a database while application code keeps the same primitive APIs. Memory is process-local and non-durable; PostgreSQL is the normal production profile.

```toml
[forge]
mode = "memory"
environment = "test"
```

For expiry and retry tests, every language also exposes a memory test factory with a manual clock and seeded entropy. Advancing the test clock drives TTLs, delayed jobs, rate-limit refill, schedules, sessions, and tokens without sleeping; seeded entropy must never mint production credentials.

## Agent integration

The repository includes a `forge-idiomatic-developer` skill with the supported contract and language conventions.

```bash
npx skills add isala404/forge
```

The skill is a development aid. It does not add runtime code or change the backend contract.

## Not in scope

- The APIs are a shared subset, not a full reimplementation. Key/value does not expose Lua or sorted sets. Blob does not manage S3 lifecycle, replication, legal hold, or CDN behavior.
- Auth provides password hashing, sessions, API keys, and one-time tokens. OAuth, OIDC, MFA, email delivery, account policy, cookies, CSRF protection, and authorization remain application concerns.
- Forge ships a PostgreSQL production profile, a complete memory test profile, and local disk for blob development. Rust supports compile-time trait injection as an advanced extension; Forge has no dynamic backend loader.
- Forge is not an ORM, web framework, frontend package, durable event log, or workflow engine. Application tables, routes, transports, and user interfaces stay outside the library.

## Examples

The repository keeps one canonical application example per language and runs each in CI: Rust todo, Node chat, Python links, and the pure-Go worker example.

Performance work is also executable: the [performance and scaling guide](https://tryforge.dev/performance/) covers release-mode backend workloads, all five supported runtime boundaries, JSON regression budgets, multi-process PostgreSQL contention, and role-specific pool sizing.

## License

MIT. Do whatever you want.

<p align="center">
  <strong>Postgres is enough.</strong><br>
  <a href="#one-connection-the-backend-primitives-most-applications-need">Quick Start</a> ·
  <a href="#examples">Examples</a> ·
  <a href="https://github.com/isala404/forge/discussions">Discussions</a>
</p>

# Forge - the standard library for agent-built SaaS

Every app needs the same plumbing, and the usual answer is a separate service for each piece:

- Redis for caching and sessions
- a queue for background jobs
- object storage for uploads
- an auth service for logins and sessions
- a cron runner for scheduled work
- a rate limiter

Six things to provision, secure, mock in tests, and learn before you ship a feature. It's worse for an AI agent building the app, where each service is more to wire up and another API to get wrong.

Forge does all of it in one library, with the same API in Rust, Node, and Python.

## One connection, eight primitives

```bash
npm install forgelib
```

Configuration lives in a `forge.toml` at your project root. `init()` reads it and
instantiates the runtime; string values may reference the environment as `${VAR}`:

```toml
[postgres]
url = "${DATABASE_URL:-postgres://localhost/myapp}"
```

Omit settings you do not need; Forge applies production-safe defaults for the rest.

```ts
import { ForgeClient } from "forgelib";

const forge = await ForgeClient.init(); // instantiates the runtime from ./forge.toml

// auth: argon2 password hashing and sessions
const hash = await forge.hashPassword(password);
const session = await forge.createSession(userId);

// rate limit: 20 attempts per minute, keyed by email
const limit = await forge.rateLimitCheck("login", email, 20, 60);
if (!limit.allowed) throw new Error("slow down");

// key/value: TTL cache and atomic counters
await forge.kvSet(`user:${userId}`, JSON.stringify(profile), 3600);
const views = await forge.kvIncr(`views:${userId}`, 1);

// queue: background jobs
await forge.queueEnqueue("emails", JSON.stringify({ to: email }));

// pub/sub: fan an event out to subscribers
await forge.pubsubPublish("user.created", JSON.stringify({ userId }));

// blob: store a file, hand back a link that expires in an hour
await forge.blobPut(`exports/${userId}.csv`, csv, "text/csv");
const link = await forge.blobPresignDownload(`exports/${userId}.csv`, 3600);

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
let link = forge.blob().presign_download(&key, Duration::from_secs(3600)).await?;

// schedule
forge.schedule().cron("nightly-report", "0 0 * * *", "reports", Bytes::new(), ScheduleOpts::new()).await?;

// feature flags
forge.config().set_flag("new-ui", FlagRule::Percent(25)).await?;
let new_ui = forge.config().flag("new-ui", false, &EvalCtx::user(user_id)).await;
```
</details>

<details>
<summary>The same in Python</summary>

```bash
pip install forgelib
```

```python
import json
import forgelib

forge = await forgelib.ForgeClient.init()  # instantiates the runtime from ./forge.toml

# auth
hash = await forge.hash_password(password)
session = await forge.create_session(user_id)

# rate limit
limit = await forge.rate_limit_check("login", email, 20, 60)
if not limit.allowed:
    raise RuntimeError("slow down")

# key/value
await forge.kv_set(f"user:{user_id}", json.dumps(profile), 3600)
views = await forge.kv_incr(f"views:{user_id}", 1)

# queue
await forge.queue_enqueue("emails", json.dumps({"to": email}))

# pub/sub
await forge.pubsub_publish("user.created", json.dumps({"user_id": user_id}))

# blob
await forge.blob_put(f"exports/{user_id}.csv", csv, "text/csv")
link = await forge.blob_presign_download(f"exports/{user_id}.csv", 3600)

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
| queue | enqueue and dequeue with acks, retries, delays, dedup, depth |
| pub/sub | publish and subscribe on topics |
| blob | put, get, delete, presigned upload and download URLs |
| auth | password hashing, sessions, API keys |
| rate limit | token-bucket or sliding-window checks |
| schedule | cron and one-off jobs |
| config | settings and feature flags with percentage rollout |

By default every primitive runs on one Postgres database, so there's nothing else to operate. When one needs to scale, move that piece onto its own backend without changing your code.

## Test on memory, ship on Postgres

The memory and Postgres backends pass the same conformance suite, so tests run in-process with no database and behave the way production will. Pick the backend in `forge.toml`, using a `${VAR}` reference so the same file flips between tests and production from the environment.

```toml
[backends]
default = "${FORGE_BACKEND:-postgres}"  # FORGE_BACKEND=memory in tests

[postgres]
url = "${DATABASE_URL:-postgres://localhost/myapp}"
```

## Build it with an agent

Forge's API isn't in any model's training data, so a coding agent won't know it cold. Install the `forge-idiomatic-developer` skill so your agent learns the conventions before it writes any Forge code.

```bash
npx skills add isala404/forge
```

It teaches which primitive fits which task and the idioms for each language, so you get working code instead of guesses.

## Not in scope

- The APIs are a shared subset, not a full reimplementation. key/value covers the Redis commands most apps use, not Lua scripts or sorted sets. blob does CRUD and presigned URLs, not S3 lifecycle rules.
- auth is the essentials: password hashing, sessions, and API keys, enough to have working auth in minutes while you prototype. For OAuth, 2FA, or email verification, bring Clerk, Firebase, or a dedicated provider.
- Forge ships two backends, memory and Postgres, plus local disk for blob. If you need Redis or S3, implement the primitive's trait and inject it, or use that service directly.
- Not an ORM or a web framework. Your tables and routes stay yours.

## Examples

Three full apps in [`examples/`](./examples), each built on the Rust, Node, and Python backends with a React frontend.

- todo: accounts, lists, and a background audit queue
- links: a URL shortener with click counts, QR images, and analytics
- chat: real-time messaging with presence

## License

MIT. Do whatever you want.

<p align="center">
  <strong>Postgres is enough.</strong><br>
  <a href="#one-connection-eight-primitives">Quick Start</a> ·
  <a href="#examples">Examples</a> ·
  <a href="https://github.com/isala404/forge/discussions">Discussions</a>
</p>

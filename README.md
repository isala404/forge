# FORGE

**The full-stack Rust framework that compiles your backend into one binary, powered by PostgreSQL.**

Queries, mutations, background jobs, cron, durable workflows, real-time subscriptions, webhooks, and MCP tools — all written as plain Rust functions, all served from a single process, all backed by the database you already know.

> [!IMPORTANT]
> **Forge is changing direction.** The full-stack framework described below is no longer where the project is headed. Forge is being rebuilt as a *standard library for agent-built SaaS*: one crate, one Postgres connection, and a small frozen set of hardened infrastructure primitives — `kv`, `queue`, `schedule`, `blob`, `config`/flags, `ratelimit`, `auth`, and `llm` — that an AI agent targets once and reuses everywhere, with pluggable backends (Postgres by default) behind a stable interface.
>
> Active development now happens on the [`rewrite`](../../tree/rewrite) branch. This `main` branch stays available but is no longer maintained.

```bash
curl -fsSL https://tryforge.dev/install.sh | sh  # or: cargo install forgex
forge new my-app --template with-svelte/minimal && cd my-app
docker compose up --build
```

[![Crates.io](https://img.shields.io/crates/v/forgex.svg)](https://crates.io/crates/forgex)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-tryforge.dev-blue)](https://tryforge.dev/docs)

![Real-time sync between iOS and web](docs/static/demo/web-ios-sync.gif)

One mutation. Both clients update instantly. No manual cache busting, no fetch wrappers, no pub/sub to configure.

---

## What You Get

- **One binary, one database.** Gateway, workers, scheduler, and daemons run in the same process. PostgreSQL is the only moving part.
- **Type safety from SQL to UI.** `sqlx` checks your queries at compile time. `#[forge::model]` generates the matching TypeScript or Rust types for your frontend.
- **Real-time by default.** Compile-time SQL parsing extracts table dependencies. PostgreSQL `LISTEN/NOTIFY` invalidates affected subscriptions. SSE pushes diffs to clients.
- **Durable by design.** Jobs and workflow state live in PostgreSQL. They survive restarts, deployments, and crashes.
- **Frontends as first-class targets.** SvelteKit and Dioxus today, more to come. Same Rust source of truth generates bindings for whichever you pick.

---

## Write a Function, Get an API

### Queries and Mutations

```rust
#[forge::query(cache = "30s")]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<User> {
    sqlx::query_as!(User, "SELECT * FROM users WHERE id = $1", id)
        .fetch_one(ctx.db())
        .await
        .map_err(Into::into)
}

#[forge::mutation(transactional)]
pub async fn create_user(ctx: &MutationContext, input: CreateUser) -> Result<User> {
    let mut conn = ctx.conn().await?;
    let user = sqlx::query_as!(User, "INSERT INTO users (email) VALUES ($1) RETURNING *", &input.email)
        .fetch_one(&mut *conn)
        .await?;

    ctx.dispatch_job("send_welcome_email", json!({ "user_id": user.id })).await?;

    Ok(user)
}
```

These become typed RPC endpoints automatically. The same Rust source generates frontend bindings — TypeScript for SvelteKit, Rust plus hooks for Dioxus — so your client is always in sync. Transactional mutations buffer `dispatch_job` calls and insert them atomically when the transaction commits. If the mutation fails, the job never exists.

### Background Jobs

```rust
#[forge::job(retry(max_attempts = 3, backoff = "exponential"))]
pub async fn send_welcome_email(ctx: &JobContext, input: EmailInput) -> Result<()> {
    ctx.progress(0, "Starting...")?;

    let user = fetch_user(ctx.db(), input.user_id).await?;
    send_email(&user.email, "Welcome!").await?;

    ctx.progress(100, "Sent")?;
    Ok(())
}
```

Persisted in PostgreSQL, claimed with `SKIP LOCKED`, bounded by a worker semaphore. Survive restarts. Retry with backoff. Report progress in real-time to any client that wants to watch.

### Cron

```rust
#[forge::cron("0 9 * * *")]
#[timezone = "America/New_York"]
pub async fn daily_digest(ctx: &CronContext) -> Result<()> {
    if ctx.is_late() {
        ctx.log.warn("Running late", json!({ "delay": ctx.delay() }));
    }

    generate_and_send_digest(ctx.db()).await
}
```

Cron expressions validated at compile time. Timezone-aware. Leader-elected so it runs exactly once across all instances, with catch-up for missed runs.

### Durable Workflows

```rust
#[forge::workflow(name = "free_trial", version = "2026-03", active, timeout = "60d")]
pub async fn free_trial_flow(ctx: &WorkflowContext, user: User) -> Result<()> {
    ctx.step("start_trial")
        .run(|| activate_trial(&user))
        .compensate(|_| deactivate_trial(&user))
        .await?;

    ctx.step("send_welcome").run(|| send_email(&user, "Welcome!")).await?;

    ctx.sleep(Duration::from_days(45)).await;  // Survives deployments.

    ctx.step("trial_ending").run(|| send_email(&user, "3 days left!")).await?;

    let decision: Value = ctx
        .wait_for_event("plan_selected", Some(Duration::from_days(3)))
        .await?;

    ctx.step("convert_or_expire")
        .run(|| resolve_trial(&user, &decision))
        .await?;

    Ok(())
}
```

Workflows are versioned and signature-guarded. New runs pin to the active version; in-flight runs resume only on exact version and signature match. Sleep for 45 days, deploy new code, restart servers, scale up — the workflow picks up exactly where it left off. Compensation runs automatically in reverse order if a later step fails.

### Real-Time Subscriptions

```svelte
<script lang="ts">
  import { listUsersStore$ } from '$lib/forge';

  const users = listUsersStore$();
</script>

{#each $users.data ?? [] as user}
  <div>{user.email}</div>
{/each}
```

Compile-time SQL parsing extracts table dependencies (including JOINs and subqueries). PostgreSQL triggers fire `NOTIFY` on changes. Forge re-runs affected queries, hashes the results, and pushes diffs to subscribed clients over SSE. No cache to invalidate, no channels to wire up.

### Webhooks

```rust
#[forge::webhook(
    path = "/hooks/stripe",
    signature = WebhookSignature::hmac_sha256("Stripe-Signature", "STRIPE_WEBHOOK_SECRET"),
    idempotency = "header:Idempotency-Key",
)]
pub async fn stripe(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> {
    ctx.dispatch_job("process_payment", payload.clone()).await?;
    Ok(WebhookResult::Accepted)
}
```

Signature validation, idempotency tracking, and job dispatch in a single handler.

### MCP Tools

```rust
#[forge::mcp_tool(name = "tickets.list", title = "List Support Tickets", read_only)]
pub async fn list_tickets(ctx: &McpToolContext) -> Result<Vec<Ticket>> {
    sqlx::query_as("SELECT * FROM tickets")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}
```

Expose any function as an MCP tool with the same auth, rate limiting, and validation as your API. AI agents get first-class access alongside your human users.

---

## Type Safety, End to End

```rust
#[forge::model]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub created_at: DateTime<Utc>,
}

#[forge::model]
pub enum UserRole { Admin, Member, Guest }
```

```typescript
// Generated automatically
export interface User {
  id: string;
  email: string;
  role: UserRole;
  created_at: string;
}

export type UserRole = "Admin" | "Member" | "Guest";

import { api } from "$lib/forge";
const user = await api.get_user({ id: "..." }); // Fully typed
```

If your Rust code compiles, your frontend types are correct and your SQL is valid.

`forge migrate prepare` runs pending migrations and then refreshes the `.sqlx/` offline cache so CI can build without a live database. `forge check` verifies that the cache is up to date.

---

## Built for Real Workloads

Forge ships an adaptive capacity benchmark that ramps concurrent users until the system breaks. Every user holds a live SSE subscription while continuously making RPC calls; 30% of traffic is writes that trigger the full reactivity pipeline.

On a 12-core laptop with PostgreSQL 18 in Docker and two Forge instances:

- **12,535 req/s** peak throughput with p90 under 50ms
- **2,250 concurrent SSE users** with zero errors, each maintaining a live subscription plus 10 req/s
- **30% writes**, each propagated through `NOTIFY` → invalidation → re-execution → SSE fan-out

Scaling to ~10,000 concurrent SSE users on dedicated infrastructure (4× Forge + primary + 2 replicas) projects to roughly $1,200/month on AWS on-demand pricing. Full methodology, tuning knobs, and a reproducible benchmark are in [`benchmarks/app/`](benchmarks/app) and [the performance docs](https://tryforge.dev/docs/scale/performance).

---

## Architecture

```
┌──────────────────────────────────────────┐
│                forge run                 │
├─────────────┬─────────────┬──────────────┤
│   Gateway   │   Workers   │  Scheduler   │
│ (HTTP/SSE)  │   (Jobs)    │    (Cron)    │
└──────┬──────┴──────┬──────┴──────┬───────┘
       │             │             │
       └─────────────┼─────────────┘
                     │
              ┌──────▼──────┐
              │ PostgreSQL  │
              └─────────────┘
```

One process, multiple subsystems:

- **Gateway** — HTTP and SSE server built on [Axum](https://github.com/tokio-rs/axum)
- **Workers** — Pull jobs from PostgreSQL using `FOR UPDATE SKIP LOCKED`
- **Scheduler** — Leader-elected cron runner via advisory locks
- **Daemons** — Long-running singleton processes with leader election

Scale horizontally by running more instances. They coordinate through PostgreSQL: `SKIP LOCKED` for queues, `LISTEN/NOTIFY` for fan-out, advisory locks for leadership. No service mesh, no gossip protocol, no extra cluster to operate.

```
forge              → Public API, Forge::builder(), prelude, CLI
├── forge-runtime  → Gateway, function router, job worker, workflow executor, cron scheduler
│   ├── forge-core → Types, traits, errors, contexts, schema definitions
│   └── forge-macros → #[query], #[mutation], #[job], #[workflow], #[cron], ...
└── forge-codegen  → Framework binding generators (SvelteKit, Dioxus)
```

---

## CLI

Development runs through `docker compose up --build`, which starts PostgreSQL, a cargo-watch backend, and the selected frontend. `forge new` takes an explicit template id such as `with-svelte/minimal`, `with-svelte/demo`, or `with-dioxus/realtime-todo-list`.

```bash
forge generate                   # generate frontend bindings from backend code
forge generate --target dioxus   # force a specific target when detection isn't enough
forge check                      # validate config, migrations, project health
forge migrate status             # check which migrations have run
forge migrate up                 # apply pending migrations (forward-only)
forge migrate prepare            # refresh the .sqlx offline cache
```

### Deploy

```bash
cargo build --release
./target/release/my-app
```

One binary, embedding the frontend build and the entire runtime. Point it at PostgreSQL and it runs. See [the deployment guide](https://tryforge.dev/docs/ship/deploy) for Docker, Kubernetes, graceful shutdown, and rolling updates.

---

## Debugging

Everything runs through PostgreSQL, which means everything is queryable.

### Health Endpoints

```
GET /_api/health    → { "status": "healthy", "version": "0.4.1" }
GET /_api/ready     → { "ready": true, "database": true, "reactor": true, "workflows": true }
```

### Inspect Jobs and Workflows

```sql
-- pending jobs
SELECT id, job_type, status, attempts, scheduled_at
FROM forge_jobs WHERE status = 'pending' ORDER BY scheduled_at;

-- in-flight workflows
SELECT id, workflow_name, workflow_version, status, current_step, started_at
FROM forge_workflow_runs WHERE status IN ('created', 'running');

-- blocked workflows (version/signature mismatches after a deploy)
SELECT id, workflow_name, blocking_reason
FROM forge_workflow_runs WHERE status LIKE 'blocked_%';
```

### System Tables

| Table                        | What it tracks                      |
| ---------------------------- | ----------------------------------- |
| `forge_jobs`                 | Job queue, status, errors, progress |
| `forge_cron_runs`            | Cron execution history              |
| `forge_workflow_definitions` | Registered workflow versions        |
| `forge_workflow_runs`        | Workflow instances and state        |
| `forge_workflow_steps`       | Individual step results             |
| `forge_nodes`                | Cluster node registry               |
| `forge_leaders`              | Leader election state               |
| `forge_daemons`              | Long-running process status         |
| `forge_sessions`             | Active SSE connections              |
| `forge_subscriptions`        | Live query subscriptions            |
| `forge_rate_limits`          | Token bucket state                  |
| `forge_webhook_events`       | Webhook idempotency tracking        |

Distributed tracing is built in via OpenTelemetry (OTLP over HTTP). Queries slower than 500ms are logged as warnings automatically. Signals — built-in product analytics — correlate every frontend event to the backend RPC call that caused it via a shared `x-correlation-id`.

---

## Who It's For

Forge is opinionated. It's a great fit if you're:

- A **solo developer or small team** shipping a SaaS product and want to spend your time on the product
- A team that **values correctness** and wants errors at compile time rather than 3 AM
- Someone who prefers **boring, well-understood infrastructure** (a database, a binary) over a distributed system you have to operate

Less of a fit if you:

- Need to integrate deeply with cloud-native primitives like Lambda, DynamoDB, or Pub/Sub
- Are building for millions of concurrent connections out of the gate (Forge targets tens of thousands of concurrent SSE users per cluster)
- Have a platform team that wants fine-grained control over each component in isolation

---

## AI Agents

Building with an AI coding agent? Install the [`forge-idiomatic-engineer`](docs/skills/forge-idiomatic-engineer) skill for Forge-aware code generation:

```bash
bunx skills add https://github.com/isala404/forge/tree/main/docs/skills/forge-idiomatic-engineer
```

It's installed automatically when you run `forge new`.

---

## Project Maturity

Forge is pre-1.0. Breaking changes happen between releases and are documented in [CHANGELOG.md](CHANGELOG.md) — pin your version if you need stability. Great for side projects, internal tools, and early-stage products. Once the core API settles, we cut 1.0 and commit to semver.

[Contributions welcome](CONTRIBUTING.md).

---

## License

MIT. Do whatever you want.

---

<p align="center">
  <strong>PostgreSQL is enough.</strong><br>
  <a href="https://tryforge.dev/docs/quick-start">Get Started</a> ·
  <a href="https://tryforge.dev/docs">Documentation</a> ·
  <a href="https://github.com/isala404/forge/discussions">Discussions</a>
</p>

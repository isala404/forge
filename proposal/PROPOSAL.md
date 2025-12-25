# FORGE: The Full-Stack Framework for the Impatient

> *"From Schema to Ship in a Single Day"*

---

## Executive Summary

**FORGE** is a full-stack application framework that combines the developer experience of modern Backend-as-a-Service platforms (like Convex and SpacetimeDB) with the reliability and control of battle-tested infrastructure (Rust, PostgreSQL, Svelte).

The core innovation: **a single binary that does everything**, backed by **PostgreSQL as the only external dependency**.

```
┌─────────────────────────────────────────────────────────┐
│                    FORGE STACK                          │
│                                                         │
│   Frontend: Svelte 5 + Auto-generated Stores            │
│   Backend:  Rust (compiled to single binary)            │
│   Database: PostgreSQL (the only dependency)            │
│                                                         │
│   Jobs, Crons, Observability, Dashboard: Built-in       │
└─────────────────────────────────────────────────────────┘
```

---

## The Problem

Building a modern SaaS application requires:

| Component | Traditional Approach | Time Investment |
|-----------|---------------------|-----------------|
| Database | PostgreSQL + ORM + Migrations | 2-3 days |
| API Layer | REST/GraphQL + Validation | 3-4 days |
| Real-time | WebSockets + Pub/Sub (Redis) | 2-3 days |
| Background Jobs | Redis + Bull/Sidekiq | 2 days |
| Scheduled Tasks | Cron service | 1 day |
| Observability | Prometheus + Grafana + Jaeger | 2-3 days |
| Deployment | Docker + K8s configs | 2-3 days |
| **Total Infrastructure** | | **15-20 days** |

That's 15-20 days before writing any business logic. And you're now managing 5+ services.

---

## The Solution

FORGE eliminates this complexity through three key principles:

### 1. Schema-Driven Everything

Write your data models once. Everything else is generated:

```rust
// schema/models.rs - You write THIS

#[forge::model]
pub struct User {
    #[id]
    pub id: Uuid,
    
    #[indexed]
    pub email: Email,
    
    #[relation(has_many = "Project")]
    pub projects: Vec<Project>,
}
```

**Generated automatically:**
- PostgreSQL migrations
- TypeScript types
- Svelte stores (reactive!)
- Validation logic
- OpenAPI documentation

### 2. Functions, Not Endpoints

Instead of designing REST endpoints, you write functions:

```rust
// Queries: Read-only, cacheable, subscribable
#[forge::query]
pub async fn get_projects(ctx: &QueryContext, user_id: Uuid) -> Result<Vec<Project>> {
    ctx.db.query::<Project>().filter(|p| p.owner_id == user_id).fetch_all().await
}

// Mutations: Transactional writes
#[forge::mutation]
pub async fn create_project(ctx: &MutationContext, input: CreateProject) -> Result<Project> {
    ctx.db.insert(Project { ... }).await
}

// Actions: Side effects, external APIs
#[forge::action]
pub async fn sync_with_stripe(ctx: &ActionContext, user_id: Uuid) -> Result<()> {
    let customer = stripe::Customer::retrieve(...).await?;
    ctx.mutate(update_subscription, ...).await
}
```

### 3. Single Binary, Infinite Scale

One Rust binary contains everything:
- HTTP/WebSocket gateway
- Function executor
- Job worker
- Scheduler
- Metrics/Logs/Traces
- Dashboard

Deploy one node for development. Deploy 100 nodes for production. **Same binary, same configuration.**

---

## What FORGE Provides

### Core Features

| Feature | Description | Documentation |
|---------|-------------|---------------|
| **Schema System** | Define models, generate everything | [→ Schema](core/SCHEMA.md) |
| **Functions** | Queries, Mutations, Actions | [→ Functions](core/FUNCTIONS.md) |
| **Background Jobs** | Persistent, retryable, prioritized | [→ Jobs](core/JOBS.md) |
| **Cron Jobs** | Scheduled tasks with timezone support | [→ Crons](core/CRONS.md) |
| **Workflows** | Multi-step processes with compensation | [→ Workflows](core/WORKFLOWS.md) |
| **Real-time** | Automatic subscriptions, live updates | [→ Reactivity](core/REACTIVITY.md) |

### Infrastructure

| Feature | Description | Documentation |
|---------|-------------|---------------|
| **Clustering** | Self-organizing node mesh | [→ Clustering](cluster/CLUSTERING.md) |
| **Observability** | Built-in metrics, logs, traces | [→ Observability](observability/OBSERVABILITY.md) |
| **Dashboard** | Web UI for everything | [→ Dashboard](observability/DASHBOARD.md) |

### Developer Experience

| Feature | Description | Documentation |
|---------|-------------|---------------|
| **Hot Reload** | Instant feedback during development | [→ Local Dev](development/DEVELOPMENT.md) |
| **Type Safety** | End-to-end, Rust to Svelte | [→ Schema](core/SCHEMA.md) |
| **Dashboard** | Web UI for migrations, jobs, logs, debugging | [→ Dashboard](observability/DASHBOARD.md) |
| **CLI** | Scaffolding (new, add model, add function) | [→ CLI](reference/CLI.md) |

---

## Why These Technology Choices?

### Rust (Backend)

- **Performance**: Near-C speed, crucial for real-time systems
- **Reliability**: If it compiles, it (usually) works
- **Single Binary**: No runtime dependencies, easy deployment
- **Ecosystem**: Tokio for async, sqlx for DB, excellent libraries

### PostgreSQL (Database)

- **Battle-tested**: 30+ years of production use
- **Feature-rich**: JSONB, LISTEN/NOTIFY, advisory locks, partitioning
- **One Dependency**: Jobs, events, sessions—all in Postgres
- **Scalable**: Read replicas, Citus for sharding if needed

### Svelte 5 (Frontend)

- **Runes**: Fine-grained reactivity, perfect for real-time
- **Performance**: Compile-time optimization, tiny bundles
- **Simplicity**: Less boilerplate than React
- **TypeScript**: First-class support

---

## Architecture at a Glance

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              CLIENT                                      │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │  Svelte 5 App                                                    │    │
│  │  • Auto-generated stores ($projects, $currentUser)               │    │
│  │  • Type-safe RPC client                                          │    │
│  │  • WebSocket for real-time                                       │    │
│  └─────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ HTTP / WebSocket
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                         FORGE CLUSTER                                    │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │
│  │   Node 1    │◄►│   Node 2    │◄►│   Node 3    │◄►│   Node N    │    │
│  │  (all roles)│  │  (all roles)│  │  (workers)  │  │  (workers)  │    │
│  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘    │
│                         ▲                                                │
│                         │ gRPC mesh                                      │
│                         ▼                                                │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │  Built-in: Metrics │ Logs │ Traces │ Dashboard │ Job Queue      │    │
│  └─────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                           POSTGRESQL                                     │
│  Tables │ Jobs │ Events │ Metrics │ Logs │ Traces │ Sessions            │
└─────────────────────────────────────────────────────────────────────────┘
```

→ See [Architecture Overview](architecture/OVERVIEW.md) for details.

---

## Quick Start

```bash
# Install FORGE CLI (scaffolding only)
curl -fsSL https://forge.dev/install.sh | sh

# Create new project
forge new my-saas
cd my-saas

# Start backend (uses cargo)
cargo watch -x run

# In another terminal: Start frontend (uses bun)
cd frontend && bun install && bun run dev

# Open in browser
# App:       http://localhost:5173
# Dashboard: http://localhost:8080/_forge/
```

**The CLI is for scaffolding only.** For migrations, jobs, logs, and debugging—use the built-in dashboard at `/_forge/`.

→ See [Local Development](development/DEVELOPMENT.md) for the full guide.

---

## Project Structure

```
my-saas/
├── forge.toml                 # Configuration
├── schema/
│   └── models.rs              # Data models (single source of truth)
├── functions/
│   ├── queries/               # Read operations
│   ├── mutations/             # Write operations
│   ├── actions/               # External calls
│   ├── jobs/                  # Background work
│   ├── crons/                 # Scheduled tasks
│   └── workflows/             # Multi-step processes
├── frontend/
│   ├── src/
│   │   ├── lib/
│   │   │   └── forge/         # Auto-generated client
│   │   └── routes/
│   └── svelte.config.js
├── generated/                 # Don't edit, auto-generated
│   ├── migrations/
│   ├── typescript/
│   └── rust/
└── tests/
```

---

## Documentation Map

### Getting Started
- [Local Development](development/DEVELOPMENT.md) — Set up your dev environment
- [Schema Guide](core/SCHEMA.md) — Define your data models
- [Functions Guide](core/FUNCTIONS.md) — Write your first query/mutation

### Core Concepts
- [Architecture Overview](architecture/OVERVIEW.md)
- [Single Binary Design](architecture/SINGLE_BINARY.md)
- [Data Flow](architecture/DATA_FLOW.md)
- [Resilience](architecture/RESILIENCE.md) — Error recovery, graceful degradation

### Advanced Topics
- [Background Jobs](core/JOBS.md)
- [Workflows & Sagas](core/WORKFLOWS.md)
- [Real-time Subscriptions](core/REACTIVITY.md)
- [Migrations](development/MIGRATIONS.md) — Schema evolution

### Operations
- [Deployment Guide](deployment/DEPLOYMENT.md)
- [Kubernetes](deployment/KUBERNETES.md)
- [Observability](observability/OBSERVABILITY.md)

### Reference
- [CLI Commands](reference/CLI.md)
- [Configuration](reference/CONFIGURATION.md)
- [Security](reference/SECURITY.md)

---

## Philosophy

1. **Batteries Included** — Everything you need, nothing you don't
2. **Progressive Complexity** — Simple things simple, complex things possible
3. **Type Safety Everywhere** — Catch errors at compile time
4. **Observable by Default** — You can't fix what you can't see
5. **PostgreSQL is Enough** — Stop managing 10 services

---

## License

FORGE is open source under the [MIT License](LICENSE).

---

## Next Steps

1. **[Read the Architecture Overview](architecture/OVERVIEW.md)** — Understand how FORGE works
2. **[Set Up Local Development](development/DEVELOPMENT.md)** — Build your first app
3. **[Explore the Dashboard](observability/DASHBOARD.md)** — Learn the tools

---

*Built with frustration by developers tired of gluing services together.*

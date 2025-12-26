# FORGE Usage Guide

This guide explains how to build, run, and deploy applications using the FORGE framework.

## 1. Installation

Install the CLI tool. This is used only for scaffolding (creating files); it is not the runtime.

```bash
curl -fsSL https://forge.dev/install.sh | sh
```

## 2. Creating a New Project

Create a new full-stack project (Rust backend + Svelte frontend):

```bash
forge new my-app
cd my-app
```

This creates:
- `src/schema/`: Your data models (Postgres tables).
- `src/functions/`: Your business logic (Queries, Mutations, Jobs).
- `frontend/`: Svelte 5 application.
- `forge.toml`: Configuration.

## 3. Development Workflow

Start the backend (in one terminal):
```bash
# Uses standard cargo commands
cargo watch -x run
```

Start the frontend (in another terminal):
```bash
cd frontend
bun install
bun run dev
```

Visit:
- **App**: `http://localhost:5173`
- **Dashboard**: `http://localhost:8080/_forge/` (Migrations, Jobs, Logs)

## 4. Defining Data (Schema)

FORGE is schema-driven. You define Rust structs, and FORGE generates SQL migrations, TypeScript types, and Svelte stores.

Use the CLI to create a model:
```bash
forge add model Post
```

This creates `src/schema/post.rs`. Edit it to define your fields:

```rust
use forge::prelude::*;

#[forge::model]
pub struct Post {
    #[id]
    pub id: Uuid,

    #[indexed]
    pub title: String,

    pub content: String,

    #[default = "false"]
    pub published: bool,

    #[default = "now()"]
    pub created_at: Timestamp,

    #[updated_at]
    pub updated_at: Timestamp,
}
```

### Supported Types
- `Uuid`
- `String`
- `i32`, `i64`, `f64`
- `bool`
- `Timestamp` (chrono::DateTime<Utc>)
- `Option<T>` (nullable)
- `Vec<T>` (if configured as JSON or relation)
- Enums (via `#[forge::forge_enum]`)

### Applying Changes
1. Run `cargo run` (or keep `cargo watch` running).
2. Go to the **Dashboard** (`http://localhost:8080/_forge/`).
3. Click **Migrations** -> **Generate** -> **Apply**.

## 5. Writing Business Logic (Functions)

Functions are your API. No REST/GraphQL boilerplate.

### Queries (Read-Only)
```bash
forge add query get_posts
```

```rust
// src/functions/get_posts.rs
use forge::prelude::*;
use crate::schema::Post;

#[forge::query]
pub async fn get_posts(ctx: QueryContext) -> Result<Vec<Post>> {
    ctx.db.fetch_all("SELECT * FROM posts WHERE published = true").await
}
```

### Mutations (Writes)
```bash
forge add mutation create_post
```

```rust
// src/functions/create_post.rs
use forge::prelude::*;
use crate::schema::Post;

#[forge::mutation]
pub async fn create_post(ctx: MutationContext, title: String, content: String) -> Result<Post> {
    let post = Post {
        id: Uuid::new_v4(),
        title,
        content,
        published: false,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    ctx.db.insert("posts", &post).await?;
    Ok(post)
}
```

## 6. Frontend Integration

After modifying models or functions, regenerate the client code:

```bash
forge generate
```

This updates `frontend/src/lib/forge/`. Use them in Svelte components:

```svelte
<script lang="ts">
    import { query, mutate } from '@forge/svelte';
    import { getPosts, createPost } from '$lib/forge/api';

    // Auto-subscribes and updates in real-time!
    const posts = query(getPosts, {});

    async function handleSubmit() {
        await mutate(createPost, {
            title: "Hello World",
            content: "My first post"
        });
    }
</script>

{#if $posts.loading}
    <p>Loading...</p>
{:else}
    <ul>
        {#each $posts.data ?? [] as post}
            <li>{post.title}</li>
        {/each}
    </ul>
{/if}
```

## 7. Background & Scheduled Tasks

### Jobs (Background Processing)
```bash
forge add job send_email
```

```rust
// src/functions/send_email_job.rs
#[forge::job(timeout = "5m", max_attempts = 3)]
pub async fn send_email(ctx: JobContext, args: EmailArgs) -> Result<()> {
    // Send email logic...
    Ok(())
}
```

Enqueueing a job from a mutation:
```rust
ctx.enqueue_job(send_email, EmailArgs { ... }).await?;
```

### Crons (Scheduled Tasks)
```bash
forge add cron daily_report
```

```rust
// src/functions/daily_report_cron.rs
#[forge::cron("0 0 * * *")] // Daily at midnight
pub async fn daily_report(ctx: CronContext) -> Result<()> {
    // Report logic...
    Ok(())
}
```

## 8. Workflows (Durable Sagas)

For complex, multi-step processes that must survive restarts.

```bash
forge add workflow onboarding
```

```rust
#[forge::workflow]
pub async fn onboarding(ctx: WorkflowContext, input: UserInput) -> Result<()> {
    let user = ctx.step("create_user")
        .run(|| ctx.mutate(create_user, input.clone()))
        .compensate(|user| ctx.mutate(delete_user, user.id))
        .await?;

    ctx.step("send_welcome")
        .run(|| send_email(user.email))
        .await?;

    Ok(())
}
```

## 9. Dashboard

The built-in dashboard is your control center. Access it at `http://localhost:8080/_forge/`.

Features:
- **Migrations**: Review and apply SQL changes.
- **Jobs**: Inspect, retry, and cancel background jobs.
- **Crons**: View schedule and manually trigger tasks.
- **Logs**: Real-time structured logging.
- **Metrics**: CPU, memory, and custom metrics.
- **SQL**: Run raw queries against the database.

use anyhow::Result;
use clap::Parser;
use console::style;
use std::fs;
use std::path::Path;

/// Create a new FORGE project.
#[derive(Parser)]
pub struct NewCommand {
    /// Project name.
    pub name: String,

    /// Use minimal template (no frontend).
    #[arg(long)]
    pub minimal: bool,

    /// Output directory (defaults to project name).
    #[arg(short, long)]
    pub output: Option<String>,
}

impl NewCommand {
    /// Execute the new project command.
    pub async fn execute(self) -> Result<()> {
        let project_dir = self.output.as_ref().unwrap_or(&self.name);
        let path = Path::new(project_dir);

        if path.exists() {
            anyhow::bail!("Directory already exists: {}", project_dir);
        }

        fs::create_dir_all(path)?;
        create_project(path, &self.name, self.minimal)?;

        println!();
        println!(
            "{} Created new FORGE project: {}",
            style("✅").green(),
            style(&self.name).cyan()
        );
        println!();
        println!("Next steps:");
        println!("  {} {}", style("cd").dim(), project_dir);
        println!("  {} to start the server", style("cargo run").dim());
        if !self.minimal {
            println!(
                "  {} to start the frontend",
                style("cd frontend && bun dev").dim()
            );
        }
        println!();

        Ok(())
    }
}

/// Create project files in the given directory.
pub fn create_project(dir: &Path, name: &str, minimal: bool) -> Result<()> {
    // Create directory structure
    fs::create_dir_all(dir.join("src/schema"))?;
    fs::create_dir_all(dir.join("src/functions"))?;
    fs::create_dir_all(dir.join("migrations"))?;

    // Create first migration (user tables)
    let migration_0001 = r#"-- Migration: Create users table
-- This migration is automatically applied on startup.

CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    email VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email ON users(email);

-- Enable real-time reactivity for this table
-- This creates a trigger that notifies the FORGE runtime of changes
SELECT forge_enable_reactivity('users');
"#;
    fs::write(dir.join("migrations/0001_create_users.sql"), migration_0001)?;

    // Create Cargo.toml with dotenvy for .env loading
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[dependencies]
forge = "0.1"
tokio = {{ version = "1", features = ["full"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
uuid = {{ version = "1", features = ["v4", "serde"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
sqlx = {{ version = "0.8", features = ["runtime-tokio", "postgres", "chrono", "uuid"] }}
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
dotenvy = "0.15"

# Pin transitive dependency for Rust < 1.88 compatibility
home = ">=0.5,<0.5.12"
"#
    );
    fs::write(dir.join("Cargo.toml"), cargo_toml)?;

    // Create forge.toml
    let forge_toml = format!(
        r#"# FORGE Configuration
# See https://forge.dev/docs/configuration for all options

[project]
name = "{name}"

[database]
url = "${{DATABASE_URL}}"

[gateway]
port = 8080

[observability]
enabled = true
"#
    );
    fs::write(dir.join("forge.toml"), forge_toml)?;

    // Create main.rs
    let main_rs = r#"use forge::prelude::*;

mod schema;
mod functions;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file (must be called before ForgeConfig::from_file)
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let config = ForgeConfig::from_file("forge.toml")?;

    let mut builder = Forge::builder();

    // Register queries (read operations, support real-time subscriptions)
    builder.function_registry_mut().register_query::<functions::GetUsersQuery>();
    builder.function_registry_mut().register_query::<functions::GetUserQuery>();

    // Register mutations (write operations, trigger subscription updates)
    builder.function_registry_mut().register_mutation::<functions::CreateUserMutation>();
    builder.function_registry_mut().register_mutation::<functions::UpdateUserMutation>();
    builder.function_registry_mut().register_mutation::<functions::DeleteUserMutation>();

    // Migrations are loaded from ./migrations directory automatically
    builder
        .config(config)
        .build()?
        .run()
        .await
}
"#;
    fs::write(dir.join("src/main.rs"), main_rs)?;

    // Create schema/mod.rs
    let schema_mod = r#"// Schema definitions
// Use `forge add model <name>` to add new models

pub mod user;

pub use user::User;
"#;
    fs::write(dir.join("src/schema/mod.rs"), schema_mod)?;

    // Create schema/user.rs (example model)
    let user_rs = r#"use forge::prelude::*;

/// User model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
"#;
    fs::write(dir.join("src/schema/user.rs"), user_rs)?;

    // Create functions/mod.rs
    let functions_mod = r#"// Function definitions
// Use `forge add query|mutation|action <name>` to add new functions

pub mod users;

pub use users::*;
"#;
    fs::write(dir.join("src/functions/mod.rs"), functions_mod)?;

    // Create functions/users.rs (example functions showing queries and mutations)
    let users_rs = r#"use forge::prelude::*;
use crate::schema::User;

// ============================================================================
// QUERIES - Read operations that support real-time subscriptions
// ============================================================================

/// Get all users (supports real-time subscription).
/// Frontend can use `subscribe(getUsers, {})` for live updates.
#[forge::query]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}

/// Get a single user by ID.
#[forge::query]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<Option<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(ctx.db())
        .await
        .map_err(Into::into)
}

// ============================================================================
// MUTATIONS - Write operations that trigger subscription updates
// ============================================================================

/// Create a new user.
/// After this mutation, all `get_users` subscriptions will automatically refresh.
#[forge::mutation]
pub async fn create_user(
    ctx: &MutationContext,
    email: String,
    name: String,
) -> Result<User> {
    let id = Uuid::new_v4();
    let now = Utc::now();

    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email, name, created_at, updated_at) VALUES ($1, $2, $3, $4, $5) RETURNING *"
    )
        .bind(id)
        .bind(&email)
        .bind(&name)
        .bind(now)
        .bind(now)
        .fetch_one(ctx.db())
        .await?;

    Ok(user)
}

/// Update an existing user.
#[forge::mutation]
pub async fn update_user(
    ctx: &MutationContext,
    id: Uuid,
    email: Option<String>,
    name: Option<String>,
) -> Result<User> {
    let now = Utc::now();

    let user = sqlx::query_as::<_, User>(
        "UPDATE users SET
            email = COALESCE($2, email),
            name = COALESCE($3, name),
            updated_at = $4
         WHERE id = $1
         RETURNING *"
    )
        .bind(id)
        .bind(email)
        .bind(name)
        .bind(now)
        .fetch_one(ctx.db())
        .await?;

    Ok(user)
}

/// Delete a user by ID.
#[forge::mutation]
pub async fn delete_user(ctx: &MutationContext, id: Uuid) -> Result<bool> {
    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(ctx.db())
        .await?;

    Ok(result.rows_affected() > 0)
}
"#;
    fs::write(dir.join("src/functions/users.rs"), users_rs)?;

    // Create .gitignore
    let gitignore = r#"/target
/node_modules
/.env
/frontend/dist
"#;
    fs::write(dir.join(".gitignore"), gitignore)?;

    // Create .env file with default database URL (ready to run)
    let env_file = r#"# Database connection string
DATABASE_URL=postgres://forge:forge@localhost:5432/forge_dev

# Optional: JWT secret for authentication
# FORGE_SECRET=your-secret-key-here
"#;
    fs::write(dir.join(".env"), env_file)?;

    // Create frontend if not minimal
    if !minimal {
        create_frontend(dir, name)?;
    }

    Ok(())
}

/// Create frontend scaffolding.
fn create_frontend(dir: &Path, name: &str) -> Result<()> {
    let frontend_dir = dir.join("frontend");
    fs::create_dir_all(&frontend_dir)?;
    fs::create_dir_all(frontend_dir.join("src/routes"))?;
    fs::create_dir_all(frontend_dir.join("src/lib/forge"))?;
    fs::create_dir_all(frontend_dir.join("src/lib/forge/runtime"))?;

    // Create package.json
    // Note: vite-plugin-svelte 6.x and vite 7.x are required for proper Svelte 5 hydration
    let package_json = format!(
        r#"{{
  "name": "{name}-frontend",
  "version": "0.1.0",
  "type": "module",
  "scripts": {{
    "dev": "vite dev",
    "build": "vite build",
    "preview": "vite preview",
    "check": "svelte-kit sync && svelte-check --tsconfig ./tsconfig.json"
  }},
  "devDependencies": {{
    "@sveltejs/adapter-static": "^3.0.0",
    "@sveltejs/kit": "^2.49.0",
    "@sveltejs/vite-plugin-svelte": "^6.0.0",
    "@types/node": "^20.0.0",
    "svelte": "^5.45.0",
    "svelte-check": "^4.0.0",
    "typescript": "^5.0.0",
    "vite": "^7.0.0"
  }},
  "dependencies": {{}}
}}
"#
    );
    fs::write(frontend_dir.join("package.json"), package_json)?;

    // Create svelte.config.js
    let svelte_config = r#"import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      pages: 'dist',
      assets: 'dist',
      fallback: 'index.html'
    })
  }
};

export default config;
"#;
    fs::write(frontend_dir.join("svelte.config.js"), svelte_config)?;

    // Create vite.config.ts
    let vite_config = r#"import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()]
});
"#;
    fs::write(frontend_dir.join("vite.config.ts"), vite_config)?;

    // Create tsconfig.json
    let tsconfig = r#"{
  "extends": "./.svelte-kit/tsconfig.json",
  "compilerOptions": {
    "strict": true,
    "moduleResolution": "bundler",
    "skipLibCheck": true
  }
}
"#;
    fs::write(frontend_dir.join("tsconfig.json"), tsconfig)?;

    // Create app.html
    let app_html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    %sveltekit.head%
</head>
<body>
    <div>%sveltekit.body%</div>
</body>
</html>
"#;
    fs::write(frontend_dir.join("src/app.html"), app_html)?;

    // Create +layout.svelte with ForgeProvider
    let layout_svelte = r#"<script lang="ts">
    import { ForgeProvider } from '$lib/forge/runtime';

    let { children } = $props();
</script>

<ForgeProvider url="http://localhost:8080">
    {@render children()}
</ForgeProvider>
"#;
    fs::write(
        frontend_dir.join("src/routes/+layout.svelte"),
        layout_svelte,
    )?;

    // Create +layout.ts to disable SSR (required for ForgeProvider context)
    let layout_ts = r#"// Disable SSR for the entire app - ForgeProvider requires client-side context
export const ssr = false;
export const csr = true;
"#;
    fs::write(frontend_dir.join("src/routes/+layout.ts"), layout_ts)?;

    // Create +page.svelte demonstrating all 3 patterns: Query, Mutation, Subscription
    let page_svelte = r#"<script lang="ts">
    import { subscribe, mutate, query } from '$lib/forge/runtime';
    import { getUsers, getUser, createUser, updateUser, deleteUser } from '$lib/forge/api';
    import type { User } from '$lib/forge/types';

    // =========================================================================
    // SUBSCRIPTION - Real-time updates via WebSocket
    // The list auto-refreshes when any user is created, updated, or deleted
    // =========================================================================
    const users = subscribe(getUsers, {});

    // Form state
    let name = $state('');
    let email = $state('');
    let isSubmitting = $state(false);
    let selectedUser = $state<User | null>(null);

    // =========================================================================
    // MUTATION - Create a new user
    // After mutation completes, the subscription automatically refreshes
    // =========================================================================
    async function handleCreateUser(e: Event) {
        e.preventDefault();
        if (!name || !email) return;

        isSubmitting = true;
        try {
            await mutate(createUser, { name, email });
            // Subscription auto-updates - no manual refetch needed!
            name = '';
            email = '';
        } catch (err) {
            console.error('Failed to create user:', err);
        }
        isSubmitting = false;
    }

    // =========================================================================
    // QUERY - One-time fetch (for on-demand data)
    // Use this for data you don't need real-time updates for
    // =========================================================================
    async function handleSelectUser(id: string) {
        const result = await query(getUser, { id });
        if (result.data) {
            selectedUser = result.data;
        }
    }

    // =========================================================================
    // MUTATION - Delete a user
    // =========================================================================
    async function handleDeleteUser(id: string) {
        if (!confirm('Delete this user?')) return;
        await mutate(deleteUser, { id });
        if (selectedUser?.id === id) {
            selectedUser = null;
        }
    }
</script>

<main>
    <h1>🔥 FORGE Demo</h1>
    <p class="subtitle">
        Backend: <a href="http://localhost:8080/health" target="_blank">localhost:8080</a> |
        Dashboard: <a href="http://localhost:8080/_dashboard" target="_blank">/_dashboard</a>
    </p>

    <div class="grid">
        <section class="card">
            <h2>➕ Create User <span class="badge">mutation</span></h2>
            <form onsubmit={handleCreateUser}>
                <input type="text" placeholder="Name" bind:value={name} required />
                <input type="email" placeholder="Email" bind:value={email} required />
                <button type="submit" disabled={isSubmitting}>
                    {isSubmitting ? 'Creating...' : 'Create'}
                </button>
            </form>
        </section>

        <section class="card">
            <h2>👤 User Detail <span class="badge">query</span></h2>
            {#if selectedUser}
                <div class="user-detail">
                    <p><strong>ID:</strong> {selectedUser.id}</p>
                    <p><strong>Name:</strong> {selectedUser.name}</p>
                    <p><strong>Email:</strong> {selectedUser.email}</p>
                    <p><strong>Created:</strong> {new Date(selectedUser.created_at).toLocaleString()}</p>
                    <button class="secondary" onclick={() => selectedUser = null}>Close</button>
                </div>
            {:else}
                <p class="muted">Click a user below to view details</p>
            {/if}
        </section>
    </div>

    <section class="card full-width">
        <h2>📋 Users <span class="badge live">subscription (live)</span></h2>
        <p class="hint">This list updates automatically when data changes - no refresh needed!</p>

        {#if $users.loading}
            <p>Loading...</p>
        {:else if $users.error}
            <p class="error">Error: {$users.error.message}</p>
        {:else if !$users.data || $users.data.length === 0}
            <p class="muted">No users yet. Create one above!</p>
        {:else}
            <table>
                <thead>
                    <tr>
                        <th>Name</th>
                        <th>Email</th>
                        <th>Actions</th>
                    </tr>
                </thead>
                <tbody>
                    {#each $users.data as user (user.id)}
                        <tr>
                            <td>{user.name}</td>
                            <td>{user.email}</td>
                            <td>
                                <button class="small" onclick={() => handleSelectUser(user.id)}>
                                    View
                                </button>
                                <button class="small danger" onclick={() => handleDeleteUser(user.id)}>
                                    Delete
                                </button>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {/if}
    </section>
</main>

<style>
    main {
        max-width: 900px;
        margin: 0 auto;
        padding: 2rem;
        font-family: system-ui, -apple-system, sans-serif;
    }

    h1 { color: #1a1a1a; margin-bottom: 0.25rem; }
    h2 { color: #333; margin: 0 0 1rem 0; font-size: 1.1rem; }

    .subtitle { color: #666; margin-bottom: 2rem; }
    .subtitle a { color: #0066cc; }

    .grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 1.5rem;
        margin-bottom: 1.5rem;
    }

    .card {
        background: #fff;
        border: 1px solid #e0e0e0;
        border-radius: 8px;
        padding: 1.5rem;
    }

    .full-width { grid-column: 1 / -1; }

    .badge {
        font-size: 0.7rem;
        padding: 0.2rem 0.5rem;
        border-radius: 4px;
        background: #e0e0e0;
        color: #666;
        font-weight: normal;
        vertical-align: middle;
    }

    .badge.live {
        background: #dcfce7;
        color: #166534;
    }

    .hint {
        font-size: 0.85rem;
        color: #666;
        margin-bottom: 1rem;
    }

    .muted { color: #999; font-style: italic; }

    form {
        display: flex;
        gap: 0.5rem;
    }

    input {
        flex: 1;
        padding: 0.5rem;
        border: 1px solid #ccc;
        border-radius: 4px;
        font-size: 0.95rem;
    }

    button {
        padding: 0.5rem 1rem;
        background: #0066cc;
        color: white;
        border: none;
        border-radius: 4px;
        cursor: pointer;
        font-size: 0.95rem;
    }

    button:hover:not(:disabled) { background: #0055aa; }
    button:disabled { background: #999; cursor: not-allowed; }

    button.secondary {
        background: #666;
    }

    button.small {
        padding: 0.25rem 0.5rem;
        font-size: 0.85rem;
    }

    button.danger {
        background: #dc2626;
    }

    button.danger:hover { background: #b91c1c; }

    table {
        width: 100%;
        border-collapse: collapse;
    }

    th, td {
        text-align: left;
        padding: 0.75rem;
        border-bottom: 1px solid #eee;
    }

    th { font-weight: 600; color: #555; }

    .user-detail p { margin: 0.5rem 0; }

    .error { color: #dc2626; }

    @media (max-width: 600px) {
        .grid { grid-template-columns: 1fr; }
    }
</style>
"#;
    fs::write(frontend_dir.join("src/routes/+page.svelte"), page_svelte)?;

    // Create $lib/forge/types.ts - Generated types from Rust schema
    let types_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
// Run `forge generate` to regenerate this file

/** User model */
export interface User {
    id: string;
    email: string;
    name: string;
    created_at: string;
    updated_at: string;
}

/** Input for creating a user */
export interface CreateUserInput {
    email: string;
    name: string;
}

/** Input for updating a user */
export interface UpdateUserInput {
    id: string;
    email?: string;
    name?: string;
}

/** Input for deleting a user */
export interface DeleteUserInput {
    id: string;
}
"#;
    fs::write(frontend_dir.join("src/lib/forge/types.ts"), types_ts)?;

    // Create $lib/forge/api.ts - Type-safe API bindings
    let api_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
// Run `forge generate` to regenerate this file

import { createQuery, createMutation } from './runtime';
import type { User, CreateUserInput, UpdateUserInput, DeleteUserInput } from './types';

// ============================================================================
// QUERIES - Use with `query()` for one-time fetch or `subscribe()` for real-time
// ============================================================================

/** Get all users - use subscribe(getUsers, {}) for real-time updates */
export const getUsers = createQuery<Record<string, never>, User[]>('get_users');

/** Get a single user by ID */
export const getUser = createQuery<{ id: string }, User | null>('get_user');

// ============================================================================
// MUTATIONS - Use with `mutate()` to modify data
// ============================================================================

/** Create a new user */
export const createUser = createMutation<CreateUserInput, User>('create_user');

/** Update an existing user */
export const updateUser = createMutation<UpdateUserInput, User>('update_user');

/** Delete a user */
export const deleteUser = createMutation<DeleteUserInput, boolean>('delete_user');
"#;
    fs::write(frontend_dir.join("src/lib/forge/api.ts"), api_ts)?;

    // Create $lib/forge/index.ts - Re-export everything
    let index_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
export * from './types';
export * from './api';
"#;
    fs::write(frontend_dir.join("src/lib/forge/index.ts"), index_ts)?;

    // Create runtime files (embedded @forge/svelte)
    create_runtime_files(&frontend_dir)?;

    Ok(())
}

/// Create the embedded runtime files (equivalent to @forge/svelte).
fn create_runtime_files(frontend_dir: &Path) -> Result<()> {
    let runtime_dir = frontend_dir.join("src/lib/forge/runtime");

    // runtime/types.ts
    let types_ts = r#"/** FORGE error type returned from the server. */
export interface ForgeError {
  code: string;
  message: string;
  details?: Record<string, unknown>;
}

/** Result of a query operation. */
export interface QueryResult<T> {
  loading: boolean;
  data: T | null;
  error: ForgeError | null;
}

/** Result of a subscription operation. */
export interface SubscriptionResult<T> extends QueryResult<T> {
  stale: boolean;
}

/** WebSocket connection state. */
export type ConnectionState = 'connecting' | 'connected' | 'reconnecting' | 'disconnected';

/** Auth state for the current user. */
export interface AuthState {
  user: unknown | null;
  token: string | null;
  loading: boolean;
}

/** Function type definitions for type-safe RPC calls. */
export interface QueryFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'query';
}

export interface MutationFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'mutation';
}

export interface ActionFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'action';
}

/** FORGE client interface for making RPC calls. */
export interface ForgeClientInterface {
  call<T>(functionName: string, args: unknown): Promise<T>;
  subscribe<T>(functionName: string, args: unknown, callback: (data: T) => void): () => void;
  getConnectionState(): ConnectionState;
  connect(): Promise<void>;
  disconnect(): void;
}
"#;
    fs::write(runtime_dir.join("types.ts"), types_ts)?;

    // runtime/client.ts
    let client_ts = r#"import type { ForgeError, ConnectionState, ForgeClientInterface } from './types.js';

export interface ForgeClientConfig {
  url: string;
  getToken?: () => string | null | Promise<string | null>;
  onAuthError?: (error: ForgeError) => void;
  timeout?: number;
}

interface RpcResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: ForgeError;
}

interface WsMessage {
  type: string;
  id?: string;
  data?: unknown;
  error?: ForgeError;
}

export class ForgeClientError extends Error {
  code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = 'ForgeClientError';
    this.code = code;
  }
}

export class ForgeClient implements ForgeClientInterface {
  private config: ForgeClientConfig;
  private ws: WebSocket | null = null;
  private connectionState: ConnectionState = 'disconnected';
  private subscriptions = new Map<string, (data: unknown) => void>();
  private pendingSubscriptions = new Map<string, { functionName: string; args: unknown }>();
  private connectionListeners = new Set<(state: ConnectionState) => void>();

  constructor(config: ForgeClientConfig) {
    this.config = config;
  }

  getConnectionState(): ConnectionState {
    return this.connectionState;
  }

  onConnectionStateChange(listener: (state: ConnectionState) => void): () => void {
    this.connectionListeners.add(listener);
    return () => this.connectionListeners.delete(listener);
  }

  async connect(): Promise<void> {
    if (this.ws?.readyState === WebSocket.OPEN) return;

    return new Promise((resolve) => {
      const wsUrl = this.config.url.replace(/^http/, 'ws') + '/ws';
      this.setConnectionState('connecting');

      try {
        this.ws = new WebSocket(wsUrl);
      } catch {
        this.setConnectionState('disconnected');
        resolve();
        return;
      }

      this.ws.onopen = async () => {
        const token = await this.getToken();
        if (token) this.ws?.send(JSON.stringify({ type: 'auth', token }));
        this.setConnectionState('connected');
        this.flushPendingSubscriptions();
        resolve();
      };

      this.ws.onerror = () => {
        this.setConnectionState('disconnected');
        resolve();
      };

      this.ws.onclose = () => this.setConnectionState('disconnected');
      this.ws.onmessage = (event) => this.handleMessage(event.data);
    });
  }

  disconnect(): void {
    this.ws?.close();
    this.ws = null;
    this.setConnectionState('disconnected');
    this.subscriptions.clear();
  }

  async call<T>(functionName: string, args: unknown): Promise<T> {
    const token = await this.getToken();
    const normalizedArgs = args && typeof args === 'object' && Object.keys(args).length === 0 ? null : args;

    const response = await fetch(`${this.config.url}/rpc/${functionName}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { 'Authorization': `Bearer ${token}` } : {}),
      },
      body: JSON.stringify(normalizedArgs),
    });

    const result: RpcResponse<T> = await response.json();
    if (!result.success || result.error) {
      const error = result.error || { code: 'UNKNOWN', message: 'Unknown error' };
      throw new ForgeClientError(error.code, error.message);
    }
    return result.data as T;
  }

  subscribe<T>(functionName: string, args: unknown, callback: (data: T) => void): () => void {
    const subscriptionId = Math.random().toString(36).substring(2, 15);
    this.subscriptions.set(subscriptionId, callback as (data: unknown) => void);

    const normalizedArgs = args && typeof args === 'object' && Object.keys(args).length === 0 ? null : args;

    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'subscribe', id: subscriptionId, function: functionName, args: normalizedArgs }));
    } else {
      this.pendingSubscriptions.set(subscriptionId, { functionName, args: normalizedArgs });
    }

    return () => {
      this.subscriptions.delete(subscriptionId);
      this.pendingSubscriptions.delete(subscriptionId);
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify({ type: 'unsubscribe', id: subscriptionId }));
      }
    };
  }

  private flushPendingSubscriptions(): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    for (const [id, { functionName, args }] of this.pendingSubscriptions) {
      this.ws.send(JSON.stringify({ type: 'subscribe', id, function: functionName, args }));
    }
    this.pendingSubscriptions.clear();
  }

  private async getToken(): Promise<string | null> {
    return this.config.getToken?.() ?? null;
  }

  private setConnectionState(state: ConnectionState): void {
    this.connectionState = state;
    this.connectionListeners.forEach(listener => listener(state));
  }

  private handleMessage(data: string): void {
    try {
      const message: WsMessage = JSON.parse(data);
      if ((message.type === 'data' || message.type === 'delta') && message.id) {
        const callback = this.subscriptions.get(message.id);
        if (callback) callback(message.data);
      }
    } catch {}
  }
}

export function createForgeClient(config: ForgeClientConfig): ForgeClient {
  return new ForgeClient(config);
}
"#;
    fs::write(runtime_dir.join("client.ts"), client_ts)?;

    // runtime/context.ts
    let context_ts = r#"import { getContext, setContext } from 'svelte';
import type { ForgeClient } from './client.js';
import type { AuthState } from './types.js';

const FORGE_CLIENT_KEY = Symbol('forge-client');
const FORGE_AUTH_KEY = Symbol('forge-auth');
let globalClient: ForgeClient | null = null;

export function getForgeClient(): ForgeClient {
  try {
    const client = getContext<ForgeClient>(FORGE_CLIENT_KEY);
    if (client) return client;
  } catch {}
  if (globalClient) return globalClient;
  throw new Error('FORGE client not found. Wrap your component with ForgeProvider.');
}

export function setForgeClient(client: ForgeClient): void {
  setContext(FORGE_CLIENT_KEY, client);
  globalClient = client;
}

export function getAuthState(): AuthState {
  const auth = getContext<AuthState>(FORGE_AUTH_KEY);
  if (!auth) throw new Error('Auth state not found.');
  return auth;
}

export function setAuthState(auth: AuthState): void {
  setContext(FORGE_AUTH_KEY, auth);
}
"#;
    fs::write(runtime_dir.join("context.ts"), context_ts)?;

    // runtime/stores.ts
    let stores_ts = r#"import { getForgeClient } from './context.js';
import type { QueryResult, SubscriptionResult, ForgeError, QueryFn, MutationFn } from './types.js';

export interface Readable<T> {
  subscribe: (run: (value: T) => void) => () => void;
}

export interface SubscriptionStore<T> extends Readable<SubscriptionResult<T>> {
  refetch: () => Promise<void>;
  unsubscribe: () => void;
}

/** One-time async query - returns a promise with the result */
export async function query<TArgs, TResult>(fn: QueryFn<TArgs, TResult>, args: TArgs): Promise<QueryResult<TResult>> {
  const client = getForgeClient();
  try {
    const data = await fn(client, args);
    return { loading: false, data, error: null };
  } catch (e) {
    return { loading: false, data: null, error: e as ForgeError };
  }
}

export function subscribe<TArgs, TResult>(fn: QueryFn<TArgs, TResult>, args: TArgs): SubscriptionStore<TResult> {
  const client = getForgeClient();
  const subscribers = new Set<(value: SubscriptionResult<TResult>) => void>();
  let unsubscribeFn: (() => void) | null = null;
  let state: SubscriptionResult<TResult> = { loading: true, data: null, error: null, stale: false };

  const notify = () => subscribers.forEach(run => run(state));

  const startSubscription = async () => {
    if (unsubscribeFn) { unsubscribeFn(); unsubscribeFn = null; }
    state = { ...state, loading: true, error: null, stale: false };
    notify();

    try {
      const initialData = await fn(client, args);
      state = { loading: false, data: initialData, error: null, stale: false };
      notify();

      unsubscribeFn = client.subscribe(fn.functionName, args, (data: TResult) => {
        state = { loading: false, data, error: null, stale: false };
        notify();
      });
    } catch (e) {
      state = { loading: false, data: null, error: e as ForgeError, stale: false };
      notify();
    }
  };

  startSubscription();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => {
        subscribers.delete(run);
        if (subscribers.size === 0 && unsubscribeFn) { unsubscribeFn(); unsubscribeFn = null; }
      };
    },
    refetch: startSubscription,
    unsubscribe: () => { if (unsubscribeFn) { unsubscribeFn(); unsubscribeFn = null; } },
  };
}

export async function mutate<TArgs, TResult>(fn: MutationFn<TArgs, TResult>, args: TArgs): Promise<TResult> {
  const client = getForgeClient();
  return fn(client, args);
}
"#;
    fs::write(runtime_dir.join("stores.ts"), stores_ts)?;

    // runtime/api.ts (helpers for generated code)
    let api_ts = r#"import type { ForgeClientInterface, QueryFn, MutationFn } from './types.js';

export function createQuery<TArgs, TResult>(name: string): QueryFn<TArgs, TResult> {
  const fn = async (client: ForgeClientInterface, args: TArgs): Promise<TResult> => {
    return client.call(name, args);
  };
  (fn as QueryFn<TArgs, TResult>).functionName = name;
  (fn as QueryFn<TArgs, TResult>).functionType = 'query';
  return fn as QueryFn<TArgs, TResult>;
}

export function createMutation<TArgs, TResult>(name: string): MutationFn<TArgs, TResult> {
  const fn = async (client: ForgeClientInterface, args: TArgs): Promise<TResult> => {
    return client.call(name, args);
  };
  (fn as MutationFn<TArgs, TResult>).functionName = name;
  (fn as MutationFn<TArgs, TResult>).functionType = 'mutation';
  return fn as MutationFn<TArgs, TResult>;
}
"#;
    fs::write(runtime_dir.join("api.ts"), api_ts)?;

    // runtime/ForgeProvider.svelte
    let provider_svelte = r#"<script lang="ts">
  import { onMount, onDestroy, type Snippet } from 'svelte';
  import { createForgeClient } from './client.js';
  import { setForgeClient, setAuthState } from './context.js';
  import type { AuthState, ConnectionState } from './types.js';

  interface Props {
    url: string;
    getToken?: () => string | null | Promise<string | null>;
    onConnectionChange?: (state: ConnectionState) => void;
    children: Snippet;
  }

  let props: Props = $props();

  const client = createForgeClient({
    url: props.url,
    getToken: props.getToken,
  });

  setForgeClient(client);

  const authState: AuthState = $state({ user: null, token: null, loading: true });
  setAuthState(authState);

  onMount(() => {
    const unsubscribe = client.onConnectionStateChange((state) => {
      props.onConnectionChange?.(state);
    });

    (async () => {
      try { await client.connect(); } catch {}
      if (props.getToken) {
        authState.token = await props.getToken();
      }
      authState.loading = false;
    })();

    return unsubscribe;
  });

  onDestroy(() => client.disconnect());
</script>

{@render props.children()}
"#;
    fs::write(runtime_dir.join("ForgeProvider.svelte"), provider_svelte)?;

    // runtime/index.ts - Re-export everything
    let index_ts = r#"export { default as ForgeProvider } from './ForgeProvider.svelte';
export { ForgeClient, ForgeClientError, createForgeClient, type ForgeClientConfig } from './client.js';
export { getForgeClient, setForgeClient, getAuthState, setAuthState } from './context.js';
export { query, subscribe, mutate, type Readable, type SubscriptionStore } from './stores.js';
export { createQuery, createMutation } from './api.js';
export type { ForgeError, QueryResult, SubscriptionResult, ConnectionState, AuthState, QueryFn, MutationFn, ForgeClientInterface } from './types.js';
"#;
    fs::write(runtime_dir.join("index.ts"), index_ts)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-project");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-project", false).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("frontend/package.json").exists());
        assert!(path.join("frontend/src/lib/forge/types.ts").exists());
        assert!(path.join("frontend/src/lib/forge/api.ts").exists());
        assert!(path.join("frontend/src/routes/+layout.ts").exists());
        assert!(path.join("migrations/0001_create_users.sql").exists());
    }

    #[test]
    fn test_create_minimal_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-minimal");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-minimal", true).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(!path.join("frontend").exists());
    }
}

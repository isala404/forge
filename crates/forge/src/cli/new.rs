use anyhow::{Context, Result};
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

    // Create Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

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
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let config = ForgeConfig::from_file("forge.toml")?;

    let mut builder = Forge::builder();

    // Register functions
    builder.function_registry_mut().register_query::<functions::GetUsersQuery>();
    builder.function_registry_mut().register_query::<functions::GetUserQuery>();
    builder.function_registry_mut().register_mutation::<functions::CreateUserMutation>();

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

    // Create functions/users.rs (example functions)
    let users_rs = r#"use forge::prelude::*;
use crate::schema::User;

/// Get all users.
#[forge::query]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}

/// Get a user by ID.
#[forge::query]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<Option<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(ctx.db())
        .await
        .map_err(Into::into)
}

/// Create a new user.
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
"#;
    fs::write(dir.join("src/functions/users.rs"), users_rs)?;

    // Create .gitignore
    let gitignore = r#"/target
/node_modules
/.env
/frontend/dist
"#;
    fs::write(dir.join(".gitignore"), gitignore)?;

    // Create .env.example
    let env_example = r#"DATABASE_URL=postgres://postgres:postgres@localhost:5432/forge_dev
FORGE_SECRET=your-secret-key-here
"#;
    fs::write(dir.join(".env.example"), env_example)?;

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
    "moduleResolution": "bundler"
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
    import { ForgeProvider } from '@forge/svelte';

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

    // Create +page.svelte using reactive stores
    let page_svelte = r#"<script lang="ts">
    import { subscribe, mutate } from '@forge/svelte';
    import { getUsers, createUser } from '$lib/forge/api';
    import type { User } from '$lib/forge/types';

    // Real-time subscription - automatically updates when data changes
    const users = subscribe(getUsers, {});

    // Form state
    let name = $state('');
    let email = $state('');
    let isSubmitting = $state(false);

    async function handleCreateUser(e: Event) {
        e.preventDefault();
        if (!name || !email) return;

        isSubmitting = true;
        try {
            await mutate(createUser, { name, email });
            // No need to refetch - subscription auto-updates!
            // Clear form
            name = '';
            email = '';
        } catch (err) {
            console.error('Failed to create user:', err);
        }
        isSubmitting = false;
    }
</script>

<main>
    <h1>Welcome to FORGE</h1>
    <p>Backend is running at <a href="http://localhost:8080/health" target="_blank">http://localhost:8080</a></p>

    <section>
        <h2>Create User</h2>
        <form onsubmit={handleCreateUser}>
            <input type="text" placeholder="Name" bind:value={name} required />
            <input type="email" placeholder="Email" bind:value={email} required />
            <button type="submit" disabled={isSubmitting}>
                {isSubmitting ? 'Creating...' : 'Create User'}
            </button>
        </form>
    </section>

    <section>
        <h2>Users</h2>
        {#if $users.loading}
            <p>Loading...</p>
        {:else if $users.error}
            <p class="error">Error: {$users.error.message}</p>
        {:else if !$users.data || $users.data.length === 0}
            <p>No users found. Create one above!</p>
        {:else}
            <ul>
                {#each $users.data as user (user.id)}
                    <li>
                        <strong>{user.name}</strong>
                        <span class="email">({user.email})</span>
                    </li>
                {/each}
            </ul>
        {/if}
    </section>
</main>

<style>
    main {
        max-width: 800px;
        margin: 0 auto;
        padding: 2rem;
        font-family: system-ui, sans-serif;
    }

    h1 { color: #333; }
    h2 { color: #555; margin-top: 2rem; }

    a { color: #0066cc; }

    form {
        display: flex;
        gap: 0.5rem;
        margin-bottom: 1rem;
    }

    input {
        padding: 0.5rem;
        border: 1px solid #ccc;
        border-radius: 4px;
        font-size: 1rem;
    }

    button {
        padding: 0.5rem 1rem;
        background: #0066cc;
        color: white;
        border: none;
        border-radius: 4px;
        cursor: pointer;
        font-size: 1rem;
    }

    button:disabled {
        background: #999;
        cursor: not-allowed;
    }

    button:hover:not(:disabled) {
        background: #0055aa;
    }

    ul {
        list-style: none;
        padding: 0;
    }

    li {
        padding: 0.75rem;
        border-bottom: 1px solid #eee;
    }

    .email {
        color: #666;
        margin-left: 0.5rem;
    }

    .error {
        color: #c00;
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
"#;
    fs::write(frontend_dir.join("src/lib/forge/types.ts"), types_ts)?;

    // Create $lib/forge/api.ts - Type-safe API bindings
    let api_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
// Run `forge generate` to regenerate this file

import { createQuery, createMutation } from '@forge/svelte';
import type { User, CreateUserInput } from './types';

// Queries
export const getUsers = createQuery<Record<string, never>, User[]>('get_users');
export const getUser = createQuery<{ id: string }, User | null>('get_user');

// Mutations
export const createUser = createMutation<CreateUserInput, User>('create_user');
"#;
    fs::write(frontend_dir.join("src/lib/forge/api.ts"), api_ts)?;

    // Create $lib/forge/index.ts - Re-export everything
    let index_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
export * from './types';
export * from './api';
"#;
    fs::write(frontend_dir.join("src/lib/forge/index.ts"), index_ts)?;

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

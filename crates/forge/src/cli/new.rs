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

    // Create initial migration (user tables and app_stats)
    let migration_0001 = r#"-- Initial schema

-- @up
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    email VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email ON users(email);

CREATE TABLE IF NOT EXISTS app_stats (
    id VARCHAR(64) PRIMARY KEY,
    stat_name VARCHAR(128) NOT NULL,
    stat_value TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT forge_enable_reactivity('users');
SELECT forge_enable_reactivity('app_stats');

-- @down
SELECT forge_disable_reactivity('app_stats');
SELECT forge_disable_reactivity('users');
DROP TABLE IF EXISTS app_stats;
DROP INDEX IF EXISTS idx_users_email;
DROP TABLE IF EXISTS users;
"#;
    fs::write(dir.join("migrations/0001_initial.sql"), migration_0001)?;

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

    let main_rs = r#"use forge::prelude::*;

mod functions;
mod schema;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("info").init();

    let config = ForgeConfig::from_file("forge.toml")?;
    let mut builder = Forge::builder();

    // Queries
    builder.function_registry_mut().register_query::<functions::GetUsersQuery>();
    builder.function_registry_mut().register_query::<functions::GetUserQuery>();
    builder.function_registry_mut().register_query::<functions::GetAppStatsQuery>();

    // Mutations
    builder.function_registry_mut().register_mutation::<functions::CreateUserMutation>();
    builder.function_registry_mut().register_mutation::<functions::UpdateUserMutation>();
    builder.function_registry_mut().register_mutation::<functions::DeleteUserMutation>();

    // Jobs
    builder.job_registry_mut().register::<functions::ExportUsersJob>();

    // Crons
    builder.cron_registry_mut().register::<functions::HeartbeatStatsCron>();

    // Workflows
    builder.workflow_registry_mut().register::<functions::AccountVerificationWorkflow>();

    builder.config(config).build()?.run().await
}
"#;
    fs::write(dir.join("src/main.rs"), main_rs)?;

    let schema_mod = r#"pub mod user;
pub use user::User;
"#;
    fs::write(dir.join("src/schema/mod.rs"), schema_mod)?;

    let user_rs = r#"use forge::prelude::*;

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

    let functions_mod = r#"pub mod users;
pub mod app_stats;
pub mod export_users_job;
pub mod heartbeat_stats_cron;
pub mod account_verification_workflow;

#[allow(unused_imports)]
pub use account_verification_workflow::*;
pub use app_stats::*;
#[allow(unused_imports)]
pub use export_users_job::*;
#[allow(unused_imports)]
pub use heartbeat_stats_cron::*;
pub use users::*;
"#;
    fs::write(dir.join("src/functions/mod.rs"), functions_mod)?;

    let users_rs = r#"use crate::schema::User;
use forge::prelude::*;

#[forge::query]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}

#[forge::query]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<Option<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(ctx.db())
        .await
        .map_err(Into::into)
}

#[forge::mutation]
pub async fn create_user(ctx: &MutationContext, email: String, name: String) -> Result<User> {
    let id = Uuid::new_v4();
    let now = Utc::now();

    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email, name, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
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

#[forge::mutation]
pub async fn update_user(
    ctx: &MutationContext,
    id: Uuid,
    email: Option<String>,
    name: Option<String>,
) -> Result<User> {
    let now = Utc::now();

    let user = sqlx::query_as::<_, User>(
        "UPDATE users SET \
         email = COALESCE($2, email), \
         name = COALESCE($3, name), \
         updated_at = $4 \
         WHERE id = $1 \
         RETURNING *",
    )
    .bind(id)
    .bind(email)
    .bind(name)
    .bind(now)
    .fetch_one(ctx.db())
    .await?;

    Ok(user)
}

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

    let export_users_job = r#"use crate::schema::User;
use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportUsersInput {
    pub format: String,
    #[allow(dead_code)]
    pub include_inactive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportUsersOutput {
    pub user_count: usize,
    pub data: String,
    pub format: String,
}

#[forge::job]
#[timeout = "10m"]
#[retry(max_attempts = 3)]
pub async fn export_users(ctx: &JobContext, input: ExportUsersInput) -> Result<ExportUsersOutput> {
    use std::time::Duration;

    tracing::info!(
        job_id = %ctx.job_id,
        format = %input.format,
        "Starting user export"
    );

    // Step 1: Initialize (0-10%)
    let _ = ctx.progress(0, "Initializing export...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = ctx.progress(10, "Fetching users from database...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 2: Fetch users (10-30%)
    let users: Vec<User> =
        sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC")
            .fetch_all(ctx.db())
            .await?;

    let total = users.len();
    let _ = ctx.progress(30, format!("Found {} users, preparing export...", total));
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 3: Generate export with progress updates (30-80%)
    let data = match input.format.as_str() {
        "json" => {
            let _ = ctx.progress(50, "Serializing to JSON...");
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _ = ctx.progress(70, "Formatting JSON output...");
            tokio::time::sleep(Duration::from_millis(500)).await;
            serde_json::to_string_pretty(&users).map_err(|e| ForgeError::Job(e.to_string()))?
        }
        _ => {
            // CSV format (default) - simulate processing each user
            let mut csv = String::from("id,email,name,created_at,updated_at\n");
            let step_count = 5; // Report progress in 5 steps
            let users_per_step = (total / step_count).max(1);

            for (i, user) in users.iter().enumerate() {
                csv.push_str(&format!(
                    "{},{},{},{},{}\n",
                    user.id, user.email, user.name, user.created_at, user.updated_at
                ));

                // Update progress at each step
                if total > 0 && (i + 1) % users_per_step == 0 {
                    let percent = 30 + ((i as f64 / total as f64) * 50.0) as u8;
                    let _ = ctx.progress(
                        percent,
                        format!("Processing user {} of {}...", i + 1, total),
                    );
                    tokio::time::sleep(Duration::from_millis(600)).await;
                }
            }
            csv
        }
    };

    // Finalize
    let _ = ctx.progress(85, "Validating export data...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = ctx.progress(95, "Finalizing export...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    tracing::info!(
        job_id = %ctx.job_id,
        user_count = total,
        format = %input.format,
        "User export completed"
    );

    let _ = ctx.progress(100, format!("Export complete! {} users exported.", total));

    Ok(ExportUsersOutput {
        user_count: total,
        data,
        format: input.format,
    })
}
"#;
    fs::write(
        dir.join("src/functions/export_users_job.rs"),
        export_users_job,
    )?;

    let heartbeat_cron = r#"/// Updates app_stats every minute for frontend live updates
#[forge::cron("* * * * *")]
#[timezone = "UTC"]
pub async fn heartbeat_stats(ctx: &forge::prelude::CronContext) -> forge::prelude::Result<()> {
    let now = chrono::Utc::now();
    tracing::debug!(run_id = %ctx.run_id, "Running heartbeat stats cron");

    sqlx::query(
        "INSERT INTO app_stats (id, stat_name, stat_value, updated_at)
         VALUES ('heartbeat', 'last_heartbeat', $1, NOW())
         ON CONFLICT (id) DO UPDATE SET stat_value = $1, updated_at = NOW()",
    )
    .bind(now.to_rfc3339())
    .execute(ctx.db())
    .await?;

    let user_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(ctx.db())
        .await?;

    sqlx::query(
        "INSERT INTO app_stats (id, stat_name, stat_value, updated_at)
         VALUES ('user_count', 'total_users', $1, NOW())
         ON CONFLICT (id) DO UPDATE SET stat_value = $1, updated_at = NOW()",
    )
    .bind(user_count.0.to_string())
    .execute(ctx.db())
    .await?;

    tracing::debug!(
        run_id = %ctx.run_id,
        user_count = user_count.0,
        "Heartbeat stats updated"
    );

    Ok(())
}
"#;
    fs::write(
        dir.join("src/functions/heartbeat_stats_cron.rs"),
        heartbeat_cron,
    )?;

    let app_stats_query = r#"use forge::prelude::*;
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct AppStat {
    pub id: String,
    pub stat_name: String,
    pub stat_value: String,
    pub updated_at: Timestamp,
}

#[forge::query]
pub async fn get_app_stats(ctx: &QueryContext) -> Result<HashMap<String, String>> {
    let stats: Vec<AppStat> = sqlx::query_as(
        "SELECT id, stat_name, stat_value, updated_at FROM app_stats ORDER BY stat_name",
    )
    .fetch_all(ctx.db())
    .await?;

    let map: HashMap<String, String> = stats.into_iter().map(|s| (s.id, s.stat_value)).collect();

    Ok(map)
}
"#;
    fs::write(dir.join("src/functions/app_stats.rs"), app_stats_query)?;

    let verification_workflow = r#"use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVerificationInput {
    pub user_id: String,
    #[serde(default)]
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVerificationOutput {
    pub verified: bool,
    pub verification_token: Option<String>,
    pub verified_at: Option<String>,
}

#[forge::workflow]
#[version = 1]
#[timeout = "24h"]
pub async fn account_verification(
    ctx: &WorkflowContext,
    input: AccountVerificationInput,
) -> Result<AccountVerificationOutput> {
    use std::time::Duration;

    tracing::info!(
        workflow_id = %ctx.run_id,
        user_id = %input.user_id,
        email = %input.email,
        "Starting account verification workflow"
    );

    // Step 1: Generate verification token
    let token = if ctx.is_step_completed("generate_token") {
        ctx.get_step_result::<String>("generate_token")
            .unwrap_or_else(|| format!("verify_{}", Uuid::new_v4()))
    } else {
        ctx.record_step_start("generate_token");
        tracing::info!("Generating verification token");
        tokio::time::sleep(Duration::from_secs(1)).await;

        let token = format!("verify_{}", Uuid::new_v4());
        ctx.record_step_complete("generate_token", serde_json::json!(token));
        token
    };

    // Step 2: Store token in database
    if !ctx.is_step_completed("store_token") {
        ctx.record_step_start("store_token");
        tracing::info!(user_id = %input.user_id, "Storing verification token");
        tokio::time::sleep(Duration::from_secs(1)).await;

        // In a real app:
        // sqlx::query("INSERT INTO verification_tokens (user_id, token) VALUES ($1, $2)")
        //     .bind(&input.user_id)
        //     .bind(&token)
        //     .execute(ctx.db())
        //     .await?;

        ctx.record_step_complete(
            "store_token",
            serde_json::json!({
                "stored": true,
                "user_id": input.user_id
            }),
        );
    }

    // Step 3: Send verification email
    if !ctx.is_step_completed("send_email") {
        ctx.record_step_start("send_email");
        tracing::info!(email = %input.email, "Sending verification email");

        // Retry logic for transient failures
        let mut attempts = 0;
        let max_attempts = 3;
        loop {
            attempts += 1;
            match send_email_simulation(&input.email, &token).await {
                Ok(_) => break,
                Err(e) if attempts < max_attempts => {
                    tracing::warn!(attempt = attempts, "Email send failed, retrying: {}", e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => {
                    ctx.record_step_failure("send_email", e.to_string());
                    return Err(e);
                }
            }
        }

        ctx.record_step_complete(
            "send_email",
            serde_json::json!({
                "sent_to": input.email,
                "sent_at": chrono::Utc::now().to_rfc3339()
            }),
        );
    }

    // Step 4: Mark account as verified
    let verified_at = if ctx.is_step_completed("mark_verified") {
        ctx.get_step_result::<Option<String>>("mark_verified")
            .flatten()
    } else {
        ctx.record_step_start("mark_verified");
        tracing::info!("Marking account as verified");
        tokio::time::sleep(Duration::from_secs(1)).await;

        // In a real app:
        // sqlx::query("UPDATE users SET verified = true WHERE id = $1")
        //     .bind(&input.user_id)
        //     .execute(ctx.db())
        //     .await?;

        let verified_at = Some(chrono::Utc::now().to_rfc3339());
        ctx.record_step_complete("mark_verified", serde_json::json!(verified_at));
        verified_at
    };

    tracing::info!(
        workflow_id = %ctx.run_id,
        user_id = %input.user_id,
        "Account verification workflow completed"
    );

    Ok(AccountVerificationOutput {
        verified: true,
        verification_token: Some(token),
        verified_at,
    })
}

/// Simulated email sending (for demo purposes)
async fn send_email_simulation(email: &str, token: &str) -> Result<()> {
    use std::time::Duration;

    // Simulate occasional failure for demo (1 in 3 chance)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // In a real app, this would call an email service
    tracing::debug!(email = %email, token = %token, "Simulated email sent");
    Ok(())
}
"#;
    fs::write(
        dir.join("src/functions/account_verification_workflow.rs"),
        verification_workflow,
    )?;

    // Create .gitignore
    let gitignore = r#"# Rust
/target
Cargo.lock

# Node/Frontend
node_modules/
.svelte-kit/
/frontend/dist
/frontend/build

# Environment
.env
.env.local
.env.*.local

# IDE
.vscode/
.idea/
*.swp
*.swo

# OS
.DS_Store
Thumbs.db

# Logs
*.log
npm-debug.log*
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

    // Create package.json with @forge/svelte dependency
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
  "dependencies": {{
    "@forge/svelte": "file:./.forge/svelte"
  }}
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

    // Create +layout.svelte with ForgeProvider (reads URL from env)
    let layout_svelte = r#"<script lang="ts">
    import { ForgeProvider } from '@forge/svelte';

    interface Props {
        children: import('svelte').Snippet;
    }

    let { children }: Props = $props();

    // Read API URL from environment or use default
    const apiUrl = import.meta.env.VITE_API_URL || 'http://localhost:8080';
</script>

<ForgeProvider url={apiUrl}>
    {@render children()}
</ForgeProvider>
"#;
    fs::write(
        frontend_dir.join("src/routes/+layout.svelte"),
        layout_svelte,
    )?;

    let layout_ts = r#"export const ssr = false;
export const csr = true;
"#;
    fs::write(frontend_dir.join("src/routes/+layout.ts"), layout_ts)?;

    let page_svelte = r#"<script lang="ts">
    import { subscribe, mutate, query } from '@forge/svelte';
    import {
        getUsers, getUser, createUser, updateUser, deleteUser, getAppStats,
        createExportUsersJob, createAccountVerificationWorkflow
    } from '$lib/forge';
    import type { User } from '$lib/forge/types';
    import { onMount, onDestroy } from 'svelte';

    const ACTIVE_JOB_KEY = 'forge_active_job_id';
    const ACTIVE_WORKFLOW_KEY = 'forge_active_workflow_id';
    const apiUrl = import.meta.env.VITE_API_URL || 'http://localhost:8080';

    const users = subscribe(getUsers, {});
    const stats = subscribe(getAppStats, {});

    let name = $state('');
    let email = $state('');
    let isSubmitting = $state(false);
    let selectedUser = $state<User | null>(null);

    const exportUsersJob = createExportUsersJob();
    const verifyAccountWorkflow = createAccountVerificationWorkflow();

    onMount(() => {
        const savedJobId = localStorage.getItem(ACTIVE_JOB_KEY);
        if (savedJobId) exportUsersJob.resume(savedJobId);

        const savedWorkflowId = localStorage.getItem(ACTIVE_WORKFLOW_KEY);
        if (savedWorkflowId) verifyAccountWorkflow.resume(savedWorkflowId);
    });

    onDestroy(() => {
        exportUsersJob.cleanup();
        verifyAccountWorkflow.cleanup();
    });

    async function handleCreateUser(e: Event) {
        e.preventDefault();
        if (!name || !email) return;

        isSubmitting = true;
        try {
            await mutate(createUser, { name, email });
            name = '';
            email = '';
        } catch (err) {
            console.error('Failed to create user:', err);
        }
        isSubmitting = false;
    }

    async function handleSelectUser(id: string) {
        const result = await query(getUser, { id });
        if (result.data) selectedUser = result.data;
    }

    async function handleDeleteUser(id: string) {
        if (!confirm('Delete this user?')) return;
        await mutate(deleteUser, { id });
        if (selectedUser?.id === id) selectedUser = null;
    }

    async function startExportJob() {
        try {
            const jobId = await exportUsersJob.start({ format: 'csv', include_inactive: false });
            localStorage.setItem(ACTIVE_JOB_KEY, jobId);
        } catch (err) {
            console.error('Failed to start job:', err);
        }
    }

    async function startVerificationWorkflow() {
        try {
            const workflowId = await verifyAccountWorkflow.start({
                user_id: selectedUser?.id || 'demo-user'
            });
            localStorage.setItem(ACTIVE_WORKFLOW_KEY, workflowId);
        } catch (err) {
            console.error('Failed to start workflow:', err);
        }
    }

    function formatTime(timestamp: string) {
        if (!timestamp) return '-';
        const date = new Date(timestamp);
        return date.toLocaleTimeString();
    }

    function stepIcon(status: string) {
        switch (status) {
            case 'completed': return '\u2713';
            case 'running': return '\u25B6';
            case 'failed': return '\u2717';
            default: return '\u25CB';
        }
    }
</script>

<main>
    <h1>FORGE Demo</h1>
    <p class="subtitle">
        Backend: <a href="{apiUrl}/health" target="_blank">{apiUrl}</a> |
        Dashboard: <a href="{apiUrl}/_dashboard" target="_blank">/_dashboard</a>
    </p>

    <!-- System Status Panel - Shows live stats from heartbeat cron -->
    <section class="card full-width status-panel">
        <h2>System Status <span class="badge live">live</span></h2>
        <p class="hint">Updated every minute by heartbeat cron. Subscribe to get real-time updates!</p>

        {#if $stats.loading}
            <p>Loading stats...</p>
        {:else if $stats.data}
            <div class="stats-grid">
                <div class="stat-item">
                    <span class="stat-label">Last Heartbeat</span>
                    <span class="stat-value">{formatTime($stats.data.heartbeat || '')}</span>
                </div>
                <div class="stat-item">
                    <span class="stat-label">Total Users</span>
                    <span class="stat-value">{$stats.data.user_count || '0'}</span>
                </div>
            </div>
        {:else}
            <p class="muted">Stats will appear after first cron run (up to 1 minute)</p>
        {/if}
    </section>

    <div class="grid">
        <!-- Job Demo Panel -->
        <section class="card">
            <h2>Background Job <span class="badge">demo</span></h2>
            <p class="hint">Export users to CSV with real-time progress tracking via WebSocket</p>

            {#if $exportUsersJob}
                <div class="progress-container">
                    <div class="progress-bar">
                        <div class="progress-fill" style="width: {$exportUsersJob.progress_percent || 0}%"></div>
                    </div>
                    <p class="progress-text">{$exportUsersJob.progress_percent || 0}% - {$exportUsersJob.progress_message || $exportUsersJob.status}</p>
                    {#if $exportUsersJob.status === 'completed' || $exportUsersJob.status === 'failed' || $exportUsersJob.status === 'pending'}
                        <button onclick={startExportJob} style="margin-top: 0.5rem;">Start New Job</button>
                    {/if}
                </div>
            {:else}
                <button onclick={startExportJob}>Start Export Job</button>
            {/if}
        </section>

        <!-- Workflow Demo Panel -->
        <section class="card">
            <h2>Workflow <span class="badge">demo</span></h2>
            <p class="hint">Multi-step account verification with step tracking via WebSocket</p>

            {#if $verifyAccountWorkflow}
                <div class="steps-list">
                    {#each $verifyAccountWorkflow.steps as step}
                        <div class="step-item {step.status}">
                            <span class="step-icon">{stepIcon(step.status)}</span>
                            <span class="step-name">{step.name}</span>
                            <span class="step-status">{step.status}</span>
                        </div>
                    {/each}
                </div>
                {#if $verifyAccountWorkflow.status === 'completed' || $verifyAccountWorkflow.status === 'failed' || $verifyAccountWorkflow.status === 'pending'}
                    <button onclick={startVerificationWorkflow} style="margin-top: 0.5rem;">Start New Workflow</button>
                {/if}
            {:else}
                <button onclick={startVerificationWorkflow}>Start Verification</button>
            {/if}
        </section>
    </div>

    <div class="grid">
        <section class="card">
            <h2>Create User <span class="badge">mutation</span></h2>
            <form onsubmit={handleCreateUser}>
                <input type="text" placeholder="Name" bind:value={name} required />
                <input type="email" placeholder="Email" bind:value={email} required />
                <button type="submit" disabled={isSubmitting}>
                    {isSubmitting ? 'Creating...' : 'Create'}
                </button>
            </form>
        </section>

        <section class="card">
            <h2>User Detail <span class="badge">query</span></h2>
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
        <h2>Users <span class="badge live">subscription (live)</span></h2>
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

    .status-panel {
        background: linear-gradient(135deg, #f0f9ff 0%, #e0f2fe 100%);
        border-color: #bae6fd;
    }

    .stats-grid {
        display: flex;
        gap: 2rem;
    }

    .stat-item {
        display: flex;
        flex-direction: column;
    }

    .stat-label {
        font-size: 0.8rem;
        color: #666;
        margin-bottom: 0.25rem;
    }

    .stat-value {
        font-size: 1.5rem;
        font-weight: 600;
        color: #0369a1;
    }

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

    /* Progress bar styles */
    .progress-container {
        margin-top: 0.5rem;
    }

    .progress-bar {
        width: 100%;
        height: 20px;
        background: #e0e0e0;
        border-radius: 10px;
        overflow: hidden;
    }

    .progress-fill {
        height: 100%;
        background: linear-gradient(90deg, #0066cc, #00aaff);
        transition: width 0.3s ease;
    }

    .progress-text {
        font-size: 0.85rem;
        color: #666;
        margin-top: 0.5rem;
    }

    /* Workflow steps styles */
    .steps-list {
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
    }

    .step-item {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        padding: 0.5rem;
        background: #f8f8f8;
        border-radius: 4px;
    }

    .step-item.completed { background: #dcfce7; }
    .step-item.running { background: #dbeafe; }
    .step-item.failed { background: #fee2e2; }

    .step-icon {
        width: 20px;
        text-align: center;
        font-weight: bold;
    }

    .step-item.completed .step-icon { color: #166534; }
    .step-item.running .step-icon { color: #1d4ed8; }
    .step-item.failed .step-icon { color: #dc2626; }

    .step-name { flex: 1; }

    .step-status {
        font-size: 0.75rem;
        color: #666;
        text-transform: uppercase;
    }

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
        .stats-grid { flex-direction: column; gap: 1rem; }
    }
</style>
"#;
    fs::write(frontend_dir.join("src/routes/+page.svelte"), page_svelte)?;

    let types_ts = r#"// Auto-generated by FORGE - DO NOT EDIT

export interface User {
    id: string;
    email: string;
    name: string;
    created_at: string;
    updated_at: string;
}

export interface CreateUserInput {
    email: string;
    name: string;
}

export interface UpdateUserInput {
    id: string;
    email?: string;
    name?: string;
}

export interface DeleteUserInput {
    id: string;
}

export type JobStatus = 'pending' | 'claimed' | 'running' | 'completed' | 'retry' | 'failed' | 'dead_letter';

export interface Job {
    id: string;
    job_type: string;
    status: JobStatus;
    priority: number;
    attempts: number;
    max_attempts: number;
    progress_percent: number | null;
    progress_message: string | null;
    input: unknown;
    output: unknown;
    scheduled_at: string;
    created_at: string;
    started_at: string | null;
    completed_at: string | null;
    last_error: string | null;
}

export type WorkflowStatus = 'created' | 'running' | 'waiting' | 'completed' | 'compensating' | 'compensated' | 'failed';

export interface WorkflowStep {
    name: string;
    status: string;
    result: unknown;
    started_at: string | null;
    completed_at: string | null;
    error: string | null;
}

export interface Workflow {
    id: string;
    workflow_name: string;
    version: string | null;
    status: WorkflowStatus;
    input: unknown;
    output: unknown;
    current_step: string | null;
    steps: WorkflowStep[];
    started_at: string;
    completed_at: string | null;
    error: string | null;
}

export interface JobProgress {
    job_id: string;
    status: JobStatus;
    progress_percent: number | null;
    progress_message: string | null;
    output: unknown;
    error: string | null;
}

export interface WorkflowProgress {
    workflow_id: string;
    status: WorkflowStatus;
    current_step: string | null;
    steps: Array<{ name: string; status: string; error: string | null }>;
    output: unknown;
    error: string | null;
}

export interface JobStats {
    pending: number;
    running: number;
    completed: number;
    failed: number;
    retrying: number;
    dead_letter: number;
}

export interface WorkflowStats {
    running: number;
    completed: number;
    waiting: number;
    failed: number;
    compensating: number;
}
"#;
    fs::write(frontend_dir.join("src/lib/forge/types.ts"), types_ts)?;

    let api_ts = r#"// Auto-generated by FORGE - DO NOT EDIT

import { createQuery, createMutation } from '@forge/svelte';
import type { User, CreateUserInput, UpdateUserInput, DeleteUserInput, Job, Workflow, JobStats, WorkflowStats } from './types';

export const getUsers = createQuery<Record<string, never>, User[]>('get_users');
export const getUser = createQuery<{ id: string }, User | null>('get_user');
export const getAppStats = createQuery<Record<string, never>, Record<string, string>>('get_app_stats');

export const createUser = createMutation<CreateUserInput, User>('create_user');
export const updateUser = createMutation<UpdateUserInput, User>('update_user');
export const deleteUser = createMutation<DeleteUserInput, boolean>('delete_user');

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:8080';

interface ApiResponse<T> {
    success: boolean;
    data?: T;
    error?: string;
}

export async function getJobStatus(jobId: string): Promise<ApiResponse<Job>> {
    const response = await fetch(`${API_URL}/_api/jobs/${jobId}`);
    return response.json();
}

export async function getWorkflowStatus(workflowId: string): Promise<ApiResponse<Workflow>> {
    const response = await fetch(`${API_URL}/_api/workflows/${workflowId}`);
    return response.json();
}

export async function getJobStats(): Promise<ApiResponse<JobStats>> {
    const response = await fetch(`${API_URL}/_api/jobs/stats`);
    return response.json();
}

export async function getWorkflowStats(): Promise<ApiResponse<WorkflowStats>> {
    const response = await fetch(`${API_URL}/_api/workflows/stats`);
    return response.json();
}

export async function pollJobUntilComplete(
    jobId: string,
    options?: {
        onProgress?: (job: Job) => void;
        pollInterval?: number;
        timeout?: number;
    }
): Promise<Job | null> {
    const { onProgress, pollInterval = 500, timeout = 300000 } = options || {};
    const startTime = Date.now();

    while (Date.now() - startTime < timeout) {
        const { data: job } = await getJobStatus(jobId);
        if (!job) return null;

        onProgress?.(job);

        if (job.status === 'completed' || job.status === 'failed' || job.status === 'dead_letter') {
            return job;
        }

        await new Promise(resolve => setTimeout(resolve, pollInterval));
    }

    return null;
}

export async function pollWorkflowUntilComplete(
    workflowId: string,
    options?: {
        onStepChange?: (workflow: Workflow) => void;
        pollInterval?: number;
        timeout?: number;
    }
): Promise<Workflow | null> {
    const { onStepChange, pollInterval = 500, timeout = 300000 } = options || {};
    const startTime = Date.now();
    let lastStepsJson = '';

    while (Date.now() - startTime < timeout) {
        const { data: workflow } = await getWorkflowStatus(workflowId);
        if (!workflow) return null;

        const stepsJson = JSON.stringify(workflow.steps?.map(s => ({ name: s.name, status: s.status })) || []);
        if (stepsJson !== lastStepsJson) {
            lastStepsJson = stepsJson;
            onStepChange?.(workflow);
        }

        if (workflow.status === 'completed' || workflow.status === 'failed' || workflow.status === 'compensated') {
            onStepChange?.(workflow);
            return workflow;
        }

        await new Promise(resolve => setTimeout(resolve, pollInterval));
    }

    return null;
}

export async function dispatchJob<T = Record<string, unknown>>(
    jobType: string,
    args?: T
): Promise<ApiResponse<{ job_id: string }>> {
    const response = await fetch(`${API_URL}/_api/jobs/${jobType}/dispatch`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ args: args || {} }),
    });
    return response.json();
}

export async function startWorkflow<T = Record<string, unknown>>(
    workflowName: string,
    input?: T
): Promise<ApiResponse<{ workflow_id: string }>> {
    const response = await fetch(`${API_URL}/_api/workflows/${workflowName}/start`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ input: input || {} }),
    });
    return response.json();
}

import { createJobTracker, createWorkflowTracker, type JobTracker, type WorkflowTracker } from '@forge/svelte';

export interface ExportUsersJobArgs {
    format: 'csv' | 'json';
    include_inactive?: boolean;
}

export interface AccountVerificationWorkflowArgs {
    user_id: string;
}

export function createExportUsersJob(): JobTracker<ExportUsersJobArgs> {
    return createJobTracker<ExportUsersJobArgs>('export_users', API_URL);
}

export function createAccountVerificationWorkflow(): WorkflowTracker<AccountVerificationWorkflowArgs> {
    return createWorkflowTracker<AccountVerificationWorkflowArgs>('account_verification', API_URL);
}
"#;
    fs::write(frontend_dir.join("src/lib/forge/api.ts"), api_ts)?;

    let index_ts = r#"// Auto-generated by FORGE - DO NOT EDIT

export * from './types';
export * from './api';
export { ForgeProvider, createJobTracker, createWorkflowTracker } from '@forge/svelte';
export type { JobTracker, WorkflowTracker, JobProgress, WorkflowProgress } from '@forge/svelte';
"#;
    fs::write(frontend_dir.join("src/lib/forge/index.ts"), index_ts)?;

    // Create .env.example for frontend configuration
    let env_example = r#"# FORGE Frontend Environment Variables
# Copy this file to .env and adjust values as needed

# API URL for the FORGE backend
VITE_API_URL=http://localhost:8080

# Add your environment-specific variables below
"#;
    fs::write(frontend_dir.join(".env.example"), env_example)?;

    // Generate @forge/svelte runtime package in .forge/svelte/
    super::runtime_generator::generate_runtime(&frontend_dir)?;

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
        assert!(path.join("migrations/0001_initial.sql").exists());
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

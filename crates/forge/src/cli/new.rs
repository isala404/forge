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

    // Create app_stats table for cron-driven live stats
    let migration_0002 = r#"-- Migration: Create app_stats table for system status
-- This table stores stats updated by the heartbeat cron job.
-- The frontend can subscribe to this for live system updates.

CREATE TABLE IF NOT EXISTS app_stats (
    id VARCHAR(64) PRIMARY KEY,
    stat_name VARCHAR(128) NOT NULL,
    stat_value TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Enable real-time reactivity so frontend subscriptions auto-update
SELECT forge_enable_reactivity('app_stats');
"#;
    fs::write(
        dir.join("migrations/0002_create_app_stats.sql"),
        migration_0002,
    )?;

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

    // =========================================================================
    // QUERIES - Read operations that support real-time subscriptions
    // =========================================================================
    builder.function_registry_mut().register_query::<functions::GetUsersQuery>();
    builder.function_registry_mut().register_query::<functions::GetUserQuery>();
    builder.function_registry_mut().register_query::<functions::GetAppStatsQuery>();

    // =========================================================================
    // MUTATIONS - Write operations that trigger subscription updates
    // =========================================================================
    builder.function_registry_mut().register_mutation::<functions::CreateUserMutation>();
    builder.function_registry_mut().register_mutation::<functions::UpdateUserMutation>();
    builder.function_registry_mut().register_mutation::<functions::DeleteUserMutation>();

    // =========================================================================
    // JOBS - Background tasks with progress tracking (see /_dashboard/jobs)
    // Frontend: Use getJobStatus() or pollJobUntilComplete() to track progress
    // =========================================================================
    builder.job_registry_mut().register::<functions::ExportUsersJob>();

    // =========================================================================
    // CRONS - Scheduled tasks (see /_dashboard/crons for history)
    // Heartbeat cron updates app_stats every minute for the System Status panel
    // =========================================================================
    builder.cron_registry_mut().register::<functions::HeartbeatStatsCron>();
    // builder.cron_registry_mut().register::<functions::CleanupInactiveUsersCron>();

    // =========================================================================
    // WORKFLOWS - Multi-step durable processes (see /_dashboard/workflows)
    // Frontend: Use getWorkflowStatus() or pollWorkflowUntilComplete() to track steps
    // =========================================================================
    builder.workflow_registry_mut().register::<functions::AccountVerificationWorkflow>();

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
// Use `forge add query|mutation|action|job|cron|workflow <name>` to add new functions

// User CRUD operations (queries and mutations)
pub mod users;

// App stats query for real-time system status
pub mod app_stats;

// Background job example - export users with progress tracking
pub mod export_users_job;

// Scheduled task example - cleanup inactive users
pub mod cleanup_inactive_users_cron;

// Heartbeat cron - updates app_stats every minute for live UI updates
pub mod heartbeat_stats_cron;

// Durable workflow example - account verification flow
pub mod account_verification_workflow;

// Re-export all function types
pub use users::*;
pub use app_stats::*;
#[allow(unused_imports)]
pub use export_users_job::*;
#[allow(unused_imports)]
pub use cleanup_inactive_users_cron::*;
#[allow(unused_imports)]
pub use heartbeat_stats_cron::*;
#[allow(unused_imports)]
pub use account_verification_workflow::*;
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

    // Create sample job demonstrating background processing with progress
    let export_users_job = r#"//! Background Job: Export Users to CSV
//!
//! Jobs are used for async processing with automatic retry logic.
//! This example demonstrates real progress tracking - perfect for long-running
//! operations like data exports, report generation, or bulk operations.
//!
//! ## Dispatching this job
//!
//! From a mutation or action:
//! ```rust
//! let job_id = ctx.dispatch_job::<ExportUsersJob>(ExportUsersInput {
//!     format: "csv".to_string(),
//!     include_inactive: false,
//! }).await?;
//! // Return the job_id so the frontend can track progress
//! ```
//!
//! ## Tracking Progress in the Frontend
//!
//! ```typescript
//! import { pollJobUntilComplete, getJobStatus } from '$lib/forge/api';
//!
//! // Poll until complete with progress updates
//! const job = await pollJobUntilComplete(jobId, {
//!     onProgress: (job) => {
//!         console.log(`${job.progress_percent}% - ${job.progress_message}`);
//!     }
//! });
//!
//! // Or check status once
//! const { data: job } = await getJobStatus(jobId);
//! ```

use forge::prelude::*;
use crate::schema::User;

/// Input for the export_users job.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportUsersInput {
    /// Export format: "csv" or "json"
    pub format: String,
    /// Whether to include inactive users
    pub include_inactive: bool,
}

/// Output from the export_users job.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportUsersOutput {
    /// Number of users exported
    pub user_count: usize,
    /// The exported data as a string
    pub data: String,
    /// Format used
    pub format: String,
}

/// Export users background job.
///
/// Demonstrates:
/// - Real progress tracking visible in dashboard and frontend
/// - Processing data in batches with percentage updates
/// - Returning useful output data
#[forge::job]
#[timeout = "10m"]
#[retry(max_attempts = 3)]
pub async fn export_users(
    ctx: &JobContext,
    input: ExportUsersInput,
) -> Result<ExportUsersOutput> {
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
    let users: Vec<User> = sqlx::query_as::<_, User>(
        "SELECT * FROM users ORDER BY created_at DESC"
    )
    .fetch_all(ctx.db())
    .await?;

    let total = users.len();
    let _ = ctx.progress(30, &format!("Found {} users, preparing export...", total));
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 3: Generate export with progress updates (30-80%)
    let data = match input.format.as_str() {
        "json" => {
            let _ = ctx.progress(50, "Serializing to JSON...");
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _ = ctx.progress(70, "Formatting JSON output...");
            tokio::time::sleep(Duration::from_millis(500)).await;
            serde_json::to_string_pretty(&users)
                .map_err(|e| ForgeError::Job(e.to_string()))?
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
                    let _ = ctx.progress(percent, &format!("Processing user {} of {}...", i + 1, total));
                    tokio::time::sleep(Duration::from_millis(600)).await;
                }
            }
            csv
        }
    };

    // Step 4: Finalize (80-100%)
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

    let _ = ctx.progress(100, &format!("Export complete! {} users exported.", total));

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

    // Create sample cron demonstrating scheduled tasks
    let cleanup_cron = r#"//! Scheduled Task: Cleanup Inactive Users
//!
//! Cron tasks run on a schedule defined by a cron expression.
//! This example shows how to run periodic maintenance tasks on user data.
//!
//! ## Cron Expression Reference
//!
//! - `* * * * *`     - Every minute
//! - `0 * * * *`     - Every hour at :00
//! - `0 0 * * *`     - Daily at midnight
//! - `0 9 * * 1-5`   - Weekdays at 9 AM
//! - `0 0 1 * *`     - Monthly on the 1st
//!
//! ## Monitoring
//!
//! Cron executions are visible in the FORGE dashboard at /_dashboard/crons.
//! You can see execution history, success/failure rates, and trigger manual runs.

/// Cleanup inactive users scheduled task.
///
/// Runs every day at 2 AM UTC (off-peak hours).
/// Identifies users who haven't been active in 30 days and logs them.
/// In production, you might send reminder emails or mark them as inactive.
#[forge::cron("0 2 * * *")]
#[timezone = "UTC"]
pub async fn cleanup_inactive_users(ctx: &CronContext) -> Result<()> {
    tracing::info!(run_id = %ctx.run_id, "Starting inactive user cleanup");

    let pool = ctx.db();

    // Find users who haven't been updated in 30 days
    // In a real app, you'd track last_login_at instead of updated_at
    let inactive_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM users WHERE updated_at < NOW() - INTERVAL '30 days'"
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(
        run_id = %ctx.run_id,
        inactive_users = inactive_count.0,
        "Found inactive users"
    );

    // Example: Send reminder emails to inactive users
    // let inactive_users = sqlx::query_as::<_, User>(
    //     "SELECT * FROM users WHERE updated_at < NOW() - INTERVAL '30 days'"
    // )
    // .fetch_all(pool)
    // .await?;
    //
    // for user in inactive_users {
    //     // Dispatch a job to send a "we miss you" email
    //     ctx.dispatch_job::<SendReminderEmailJob>(SendReminderEmailInput {
    //         user_id: user.id,
    //         email: user.email,
    //     }).await?;
    // }

    // Example: Clean up old sessions for better performance
    let deleted_sessions = sqlx::query(
        "DELETE FROM forge_sessions WHERE last_active_at < NOW() - INTERVAL '7 days'"
    )
    .execute(pool)
    .await?
    .rows_affected();

    tracing::info!(
        run_id = %ctx.run_id,
        deleted_sessions = deleted_sessions,
        inactive_users = inactive_count.0,
        "Cleanup completed"
    );

    Ok(())
}
"#;
    fs::write(
        dir.join("src/functions/cleanup_inactive_users_cron.rs"),
        cleanup_cron,
    )?;

    // Create heartbeat cron that updates app_stats every minute
    let heartbeat_cron = r#"//! Scheduled Task: Heartbeat Stats
//!
//! This cron runs every minute and updates the app_stats table with current
//! system statistics. Since app_stats has reactivity enabled, any frontend
//! subscriptions to get_app_stats will automatically receive updates.
//!
//! ## Monitoring
//!
//! This cron is visible in the FORGE dashboard at /_dashboard/crons.
//! The live stats it produces are shown in the frontend System Status panel.

/// Heartbeat stats cron - runs every minute to update live stats.
///
/// Updates app_stats table with:
/// - last_heartbeat: Current timestamp (proves cron is running)
/// - user_count: Total number of users
///
/// The frontend subscribes to app_stats for real-time updates.
#[forge::cron("* * * * *")]
#[timezone = "UTC"]
pub async fn heartbeat_stats(ctx: &CronContext) -> Result<()> {
    let now = chrono::Utc::now();
    tracing::debug!(run_id = %ctx.run_id, "Running heartbeat stats cron");

    // Update last heartbeat timestamp - this triggers reactivity for subscribers
    sqlx::query(
        "INSERT INTO app_stats (id, stat_name, stat_value, updated_at)
         VALUES ('heartbeat', 'last_heartbeat', $1, NOW())
         ON CONFLICT (id) DO UPDATE SET stat_value = $1, updated_at = NOW()"
    )
    .bind(now.to_rfc3339())
    .execute(ctx.db())
    .await?;

    // Count users and store
    let user_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(ctx.db())
        .await?;

    sqlx::query(
        "INSERT INTO app_stats (id, stat_name, stat_value, updated_at)
         VALUES ('user_count', 'total_users', $1, NOW())
         ON CONFLICT (id) DO UPDATE SET stat_value = $1, updated_at = NOW()"
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

    // Create app_stats query for frontend subscription
    let app_stats_query = r#"//! App Stats Query
//!
//! This query returns the current app statistics from the app_stats table.
//! The heartbeat_stats cron updates these values every minute.
//!
//! ## Usage in Frontend
//!
//! ```typescript
//! // Subscribe for real-time updates
//! const stats = subscribe(getAppStats, {});
//!
//! // Use in component
//! {#if $stats.data}
//!     <p>Last Heartbeat: {$stats.data.last_heartbeat}</p>
//!     <p>Total Users: {$stats.data.user_count}</p>
//! {/if}
//! ```

use forge::prelude::*;
use std::collections::HashMap;

/// App stat entry from the database.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct AppStat {
    pub id: String,
    pub stat_name: String,
    pub stat_value: String,
    pub updated_at: Timestamp,
}

/// Get all app stats as a map.
/// Returns stats like last_heartbeat, user_count, etc.
/// Subscribe to this query for real-time system status updates.
#[forge::query]
pub async fn get_app_stats(ctx: &QueryContext) -> Result<HashMap<String, String>> {
    let stats: Vec<AppStat> = sqlx::query_as(
        "SELECT id, stat_name, stat_value, updated_at FROM app_stats ORDER BY stat_name"
    )
    .fetch_all(ctx.db())
    .await?;

    let map: HashMap<String, String> = stats
        .into_iter()
        .map(|s| (s.id, s.stat_value))
        .collect();

    Ok(map)
}
"#;
    fs::write(dir.join("src/functions/app_stats.rs"), app_stats_query)?;

    // Create sample workflow demonstrating durable multi-step processes
    let verification_workflow = r#"//! Workflow: Account Verification
//!
//! Workflows are durable multi-step processes with automatic state persistence.
//! Each step is checkpointed - if the workflow fails, it resumes from where it left off.
//!
//! This example shows a real-world email verification flow with steps that can
//! be tracked in the frontend.
//!
//! ## Starting this workflow
//!
//! ```rust
//! let workflow_id = ctx.start_workflow::<AccountVerificationWorkflow>(
//!     AccountVerificationInput { user_id, email: user.email.clone() }
//! ).await?;
//! // Return the workflow_id so frontend can track progress
//! ```
//!
//! ## Tracking Progress in the Frontend
//!
//! ```typescript
//! import { pollWorkflowUntilComplete, getWorkflowStatus } from '$lib/forge/api';
//!
//! // Poll until complete with step updates
//! const workflow = await pollWorkflowUntilComplete(workflowId, {
//!     onStepChange: (workflow) => {
//!         console.log(`Current step: ${workflow.current_step}`);
//!         workflow.steps.forEach(step =>
//!             console.log(`  ${step.name}: ${step.status}`)
//!         );
//!     }
//! });
//!
//! // Or check status once
//! const { data: workflow } = await getWorkflowStatus(workflowId);
//! ```
//!
//! ## Key Concepts
//!
//! - **Steps**: Each step runs exactly once (idempotent)
//! - **Compensation**: Steps can define rollback logic for failures
//! - **Durability**: Workflow state survives restarts

use forge::prelude::*;

/// Input for the account verification workflow.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVerificationInput {
    /// User ID (can be UUID string or any identifier)
    pub user_id: String,
    /// Email to send verification to (optional for demo)
    #[serde(default)]
    pub email: String,
}

/// Output from the account verification workflow.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVerificationOutput {
    pub verified: bool,
    pub verification_token: Option<String>,
    pub verified_at: Option<String>,
}

/// Account verification workflow.
///
/// Multi-step process for verifying a user's email:
/// 1. Generate verification token
/// 2. Store token in database
/// 3. Send verification email (simulated)
/// 4. Mark account as verified (in real app, this waits for user click)
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
    let token = if !ctx.is_step_completed("generate_token") {
        ctx.record_step_start("generate_token");
        tracing::info!("Step 1: Generating verification token");

        // Simulate cryptographic token generation
        tokio::time::sleep(Duration::from_secs(1)).await;

        let token = format!("verify_{}", Uuid::new_v4());
        ctx.record_step_complete("generate_token", serde_json::json!({
            "token": token,
            "generated_at": chrono::Utc::now().to_rfc3339()
        }));
        token
    } else {
        ctx.get_step_result::<serde_json::Value>("generate_token")
            .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(String::from))
            .unwrap_or_default()
    };

    // Step 2: Store token in database
    if !ctx.is_step_completed("store_token") {
        ctx.record_step_start("store_token");
        tracing::info!("Step 2: Storing verification token");

        // Simulate database write
        tokio::time::sleep(Duration::from_secs(1)).await;

        // In a real app, you'd store this in a verification_tokens table:
        // sqlx::query(
        //     "INSERT INTO verification_tokens (user_id, token, expires_at) VALUES ($1, $2, NOW() + INTERVAL '24 hours')"
        // )
        // .bind(input.user_id)
        // .bind(&token)
        // .execute(ctx.db())
        // .await?;

        ctx.record_step_complete("store_token", serde_json::json!({
            "stored": true,
            "user_id": input.user_id
        }));
    }

    // Step 3: Send verification email
    if !ctx.is_step_completed("send_email") {
        ctx.record_step_start("send_email");
        tracing::info!("Step 3: Sending verification email to {}", input.email);

        // Simulate email API call (typically takes 1-3 seconds)
        tokio::time::sleep(Duration::from_secs(2)).await;

        // In a real app, dispatch an email job:
        // ctx.dispatch_job::<SendEmailJob>(SendEmailInput {
        //     to: input.email.clone(),
        //     subject: "Verify your account".to_string(),
        //     body: format!("Click here to verify: https://app.example.com/verify?token={}", token),
        // }).await?;

        ctx.record_step_complete("send_email", serde_json::json!({
            "sent_to": input.email,
            "sent_at": chrono::Utc::now().to_rfc3339()
        }));
    }

    // Step 4: Mark as verified (in a real app, this would wait for the user to click the link)
    // For demo purposes, we auto-verify after a delay
    let verified_at = if !ctx.is_step_completed("mark_verified") {
        ctx.record_step_start("mark_verified");
        tracing::info!("Step 4: Marking account as verified");

        // Simulate verification check and database update
        tokio::time::sleep(Duration::from_secs(1)).await;

        // In a real app, you'd update the user record:
        // sqlx::query("UPDATE users SET verified = true, verified_at = NOW() WHERE id = $1")
        //     .bind(input.user_id)
        //     .execute(ctx.db())
        //     .await?;

        let now = chrono::Utc::now().to_rfc3339();
        ctx.record_step_complete("mark_verified", serde_json::json!({
            "verified": true,
            "verified_at": now
        }));
        Some(now)
    } else {
        ctx.get_step_result::<serde_json::Value>("mark_verified")
            .and_then(|v| v.get("verified_at").and_then(|t| t.as_str()).map(String::from))
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

    // Create +layout.svelte with ForgeProvider (reads URL from env)
    let layout_svelte = r#"<script lang="ts">
    import { ForgeProvider } from '$lib/forge/runtime';

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

    // Create +layout.ts to disable SSR (required for ForgeProvider context)
    let layout_ts = r#"// Disable SSR for the entire app - ForgeProvider requires client-side context
export const ssr = false;
export const csr = true;
"#;
    fs::write(frontend_dir.join("src/routes/+layout.ts"), layout_ts)?;

    // Create +page.svelte demonstrating all 3 patterns: Query, Mutation, Subscription
    // Plus: System Status (cron-driven), Job Demo (progress), Workflow Demo (steps)
    let page_svelte = r#"<script lang="ts">
    import { subscribe, mutate, query } from '$lib/forge/runtime';
    import {
        getUsers, getUser, createUser, updateUser, deleteUser, getAppStats,
        dispatchJob, startWorkflow, pollJobUntilComplete, pollWorkflowUntilComplete
    } from '$lib/forge/api';
    import type { User } from '$lib/forge/types';

    // Read API URL from environment (same as layout)
    const apiUrl = import.meta.env.VITE_API_URL || 'http://localhost:8080';

    // =========================================================================
    // SUBSCRIPTION - Real-time updates via WebSocket
    // The list auto-refreshes when any user is created, updated, or deleted
    // =========================================================================
    const users = subscribe(getUsers, {});

    // App stats subscription - updated by heartbeat cron every minute
    const stats = subscribe(getAppStats, {});

    // Form state
    let name = $state('');
    let email = $state('');
    let isSubmitting = $state(false);
    let selectedUser = $state<User | null>(null);

    // Job demo state
    let jobProgress = $state<{ percent: number; message: string } | null>(null);
    let jobId = $state<string | null>(null);

    // Workflow demo state
    let workflowSteps = $state<Array<{ name: string; status: string }>>([]);
    let workflowId = $state<string | null>(null);

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

    // =========================================================================
    // JOB DEMO - Dispatch a job and track progress
    // =========================================================================
    async function startExportJob() {
        jobProgress = { percent: 0, message: 'Dispatching job...' };

        const { data, error } = await dispatchJob('export_users', {
            format: 'csv',
            include_inactive: false
        });

        if (error || !data?.job_id) {
            jobProgress = { percent: 0, message: `Error: ${error || 'Failed to dispatch job'}` };
            setTimeout(() => { jobProgress = null; }, 3000);
            return;
        }

        // Poll for progress updates from backend
        const finalJob = await pollJobUntilComplete(data.job_id, {
            onProgress: (job) => {
                jobProgress = {
                    percent: job.progress_percent || 0,
                    message: job.progress_message || job.status
                };
            }
        });

        if (finalJob?.status === 'failed') {
            jobProgress = { percent: 0, message: `Failed: ${finalJob.error || 'Unknown error'}` };
        }
        setTimeout(() => { jobProgress = null; }, 2000);
    }

    // =========================================================================
    // WORKFLOW DEMO - Start a workflow and track steps
    // =========================================================================
    async function startVerificationWorkflow() {
        workflowSteps = [{ name: 'Starting...', status: 'running' }];

        const { data, error } = await startWorkflow('account_verification', {
            user_id: selectedUser?.id || 'demo-user'
        });

        if (error || !data?.workflow_id) {
            workflowSteps = [{ name: `Error: ${error || 'Failed to start workflow'}`, status: 'failed' }];
            setTimeout(() => { workflowSteps = []; }, 3000);
            return;
        }

        // Poll for step progress from backend
        const finalWorkflow = await pollWorkflowUntilComplete(data.workflow_id, {
            onStepChange: (workflow) => {
                if (workflow.steps && workflow.steps.length > 0) {
                    workflowSteps = workflow.steps.map((step: { name: string; status: string }) => ({
                        name: step.name,
                        status: step.status
                    }));
                }
            }
        });

        if (finalWorkflow?.status === 'failed') {
            workflowSteps = [...workflowSteps, { name: `Error: ${finalWorkflow.error || 'Workflow failed'}`, status: 'failed' }];
        }
        setTimeout(() => { workflowSteps = []; }, 2000);
    }

    // Format timestamp
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
            <p class="hint">Export users to CSV with real-time progress tracking</p>

            {#if jobProgress}
                <div class="progress-container">
                    <div class="progress-bar">
                        <div class="progress-fill" style="width: {jobProgress.percent}%"></div>
                    </div>
                    <p class="progress-text">{jobProgress.percent}% - {jobProgress.message}</p>
                </div>
            {:else}
                <button onclick={startExportJob}>Start Export Job</button>
            {/if}
        </section>

        <!-- Workflow Demo Panel -->
        <section class="card">
            <h2>Workflow <span class="badge">demo</span></h2>
            <p class="hint">Multi-step account verification with step tracking</p>

            {#if workflowSteps.length > 0}
                <div class="steps-list">
                    {#each workflowSteps as step}
                        <div class="step-item {step.status}">
                            <span class="step-icon">{stepIcon(step.status)}</span>
                            <span class="step-name">{step.name}</span>
                            <span class="step-status">{step.status}</span>
                        </div>
                    {/each}
                </div>
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

// ============================================================================
// FORGE System Types - Job, Workflow, and Progress tracking
// ============================================================================

/** Job status enum */
export type JobStatus = 'pending' | 'claimed' | 'running' | 'completed' | 'retry' | 'failed' | 'dead_letter';

/** Job detail with progress info */
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

/** Workflow status enum */
export type WorkflowStatus = 'created' | 'running' | 'waiting' | 'completed' | 'compensating' | 'compensated' | 'failed';

/** Workflow step detail */
export interface WorkflowStep {
    name: string;
    status: string;
    result: unknown;
    started_at: string | null;
    completed_at: string | null;
    error: string | null;
}

/** Workflow detail with steps */
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

/** Job stats summary */
export interface JobStats {
    pending: number;
    running: number;
    completed: number;
    failed: number;
    retrying: number;
    dead_letter: number;
}

/** Workflow stats summary */
export interface WorkflowStats {
    running: number;
    completed: number;
    waiting: number;
    failed: number;
    compensating: number;
}
"#;
    fs::write(frontend_dir.join("src/lib/forge/types.ts"), types_ts)?;

    // Create $lib/forge/api.ts - Type-safe API bindings
    let api_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
// Run `forge generate` to regenerate this file

import { createQuery, createMutation } from './runtime';
import type { User, CreateUserInput, UpdateUserInput, DeleteUserInput, Job, Workflow, JobStats, WorkflowStats } from './types';

// ============================================================================
// QUERIES - Use with `query()` for one-time fetch or `subscribe()` for real-time
// ============================================================================

/** Get all users - use subscribe(getUsers, {}) for real-time updates */
export const getUsers = createQuery<Record<string, never>, User[]>('get_users');

/** Get a single user by ID */
export const getUser = createQuery<{ id: string }, User | null>('get_user');

/** Get app stats - subscribe for real-time updates from heartbeat cron */
export const getAppStats = createQuery<Record<string, never>, Record<string, string>>('get_app_stats');

// ============================================================================
// MUTATIONS - Use with `mutate()` to modify data
// ============================================================================

/** Create a new user */
export const createUser = createMutation<CreateUserInput, User>('create_user');

/** Update an existing user */
export const updateUser = createMutation<UpdateUserInput, User>('update_user');

/** Delete a user */
export const deleteUser = createMutation<DeleteUserInput, boolean>('delete_user');

// ============================================================================
// FORGE SYSTEM API - Job and Workflow monitoring
// These functions fetch data from the FORGE system API (/_api/*)
// ============================================================================

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:8080';

interface ApiResponse<T> {
    success: boolean;
    data?: T;
    error?: string;
}

/**
 * Get job status and progress by ID.
 * Useful for tracking background job progress in the UI.
 *
 * @example
 * const { data: job } = await getJobStatus('job-uuid');
 * console.log(`Progress: ${job?.progress_percent}% - ${job?.progress_message}`);
 */
export async function getJobStatus(jobId: string): Promise<ApiResponse<Job>> {
    const response = await fetch(`${API_URL}/_api/jobs/${jobId}`);
    return response.json();
}

/**
 * Get workflow status and steps by ID.
 * Useful for tracking multi-step workflow progress in the UI.
 *
 * @example
 * const { data: workflow } = await getWorkflowStatus('workflow-uuid');
 * console.log(`Current step: ${workflow?.current_step}`);
 * workflow?.steps.forEach(step => console.log(`${step.name}: ${step.status}`));
 */
export async function getWorkflowStatus(workflowId: string): Promise<ApiResponse<Workflow>> {
    const response = await fetch(`${API_URL}/_api/workflows/${workflowId}`);
    return response.json();
}

/**
 * Get job statistics (counts by status).
 * Useful for dashboard summaries.
 */
export async function getJobStats(): Promise<ApiResponse<JobStats>> {
    const response = await fetch(`${API_URL}/_api/jobs/stats`);
    return response.json();
}

/**
 * Get workflow statistics (counts by status).
 * Useful for dashboard summaries.
 */
export async function getWorkflowStats(): Promise<ApiResponse<WorkflowStats>> {
    const response = await fetch(`${API_URL}/_api/workflows/stats`);
    return response.json();
}

/**
 * Poll a job until it completes or fails.
 * Returns the final job state.
 *
 * @example
 * const job = await pollJobUntilComplete('job-uuid', {
 *   onProgress: (job) => console.log(`${job.progress_percent}%`),
 *   pollInterval: 1000
 * });
 */
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

/**
 * Poll a workflow until it completes or fails.
 * Returns the final workflow state.
 */
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

        // Call onStepChange whenever steps array changes
        const stepsJson = JSON.stringify(workflow.steps?.map(s => ({ name: s.name, status: s.status })) || []);
        if (stepsJson !== lastStepsJson) {
            lastStepsJson = stepsJson;
            onStepChange?.(workflow);
        }

        if (workflow.status === 'completed' || workflow.status === 'failed' || workflow.status === 'compensated') {
            // Final callback with completed state
            onStepChange?.(workflow);
            return workflow;
        }

        await new Promise(resolve => setTimeout(resolve, pollInterval));
    }

    return null;
}

// ============================================================================
// JOB & WORKFLOW DISPATCH FUNCTIONS
// ============================================================================

/**
 * Dispatch a background job by type.
 * Returns the job ID that can be used with getJobStatus or pollJobUntilComplete.
 *
 * @example
 * const { data } = await dispatchJob('export_users', { format: 'csv' });
 * if (data) {
 *   const job = await pollJobUntilComplete(data.job_id, {
 *     onProgress: (j) => console.log(`${j.progress_percent}%`)
 *   });
 * }
 */
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

/**
 * Start a workflow by name.
 * Returns the workflow ID that can be used with getWorkflowStatus or pollWorkflowUntilComplete.
 *
 * @example
 * const { data } = await startWorkflow('account_verification', { user_id: '...' });
 * if (data) {
 *   const workflow = await pollWorkflowUntilComplete(data.workflow_id, {
 *     onStepChange: (w) => console.log(`Step: ${w.current_step}`)
 *   });
 * }
 */
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
"#;
    fs::write(frontend_dir.join("src/lib/forge/api.ts"), api_ts)?;

    // Create $lib/forge/index.ts - Re-export everything
    let index_ts = r#"// Auto-generated by FORGE - DO NOT EDIT
export * from './types';
export * from './api';
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

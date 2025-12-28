use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use std::fs;
use std::path::Path;

/// Add a new component.
#[derive(Parser)]
pub struct AddCommand {
    #[command(subcommand)]
    pub component: AddType,
}

/// Component types that can be added.
#[derive(Subcommand)]
pub enum AddType {
    /// Add a new model.
    Model {
        /// Model name (PascalCase).
        name: String,
    },
    /// Add a new query function.
    Query {
        /// Function name (snake_case).
        name: String,
    },
    /// Add a new mutation function.
    Mutation {
        /// Function name (snake_case).
        name: String,
    },
    /// Add a new action function.
    Action {
        /// Function name (snake_case).
        name: String,
    },
    /// Add a new background job.
    Job {
        /// Job name (snake_case).
        name: String,
    },
    /// Add a new cron task.
    Cron {
        /// Cron name (snake_case).
        name: String,
    },
    /// Add a new workflow.
    Workflow {
        /// Workflow name (snake_case).
        name: String,
    },
}

impl AddCommand {
    /// Execute the add command.
    pub async fn execute(self) -> Result<()> {
        match self.component {
            AddType::Model { name } => add_model(&name),
            AddType::Query { name } => add_function(&name, FunctionType::Query),
            AddType::Mutation { name } => add_function(&name, FunctionType::Mutation),
            AddType::Action { name } => add_function(&name, FunctionType::Action),
            AddType::Job { name } => add_job(&name),
            AddType::Cron { name } => add_cron(&name),
            AddType::Workflow { name } => add_workflow(&name),
        }
    }
}

/// Function types.
enum FunctionType {
    Query,
    Mutation,
    Action,
}

/// Add a new model.
fn add_model(name: &str) -> Result<()> {
    let pascal_name = to_pascal_case(name);
    let snake_name = to_snake_case(&pascal_name);

    let schema_dir = Path::new("src/schema");
    if !schema_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/schema not found)");
    }

    let file_path = schema_dir.join(format!("{}.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Model already exists: {}", file_path.display());
    }

    let content = format!(
        r#"use forge::prelude::*;

/// {pascal_name} model.
#[forge::model]
pub struct {pascal_name} {{
    #[id]
    pub id: Uuid,

    // Add your fields here
    // pub name: String,

    #[default = "now()"]
    pub created_at: Timestamp,

    #[updated_at]
    pub updated_at: Timestamp,
}}
"#
    );

    fs::write(&file_path, content)?;
    update_schema_mod(&snake_name, &pascal_name)?;

    println!(
        "{} Created model: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );
    println!("   Don't forget to add your fields!");

    Ok(())
}

/// Add a new function.
fn add_function(name: &str, fn_type: FunctionType) -> Result<()> {
    let snake_name = to_snake_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Function file already exists: {}", file_path.display());
    }

    let content = match fn_type {
        FunctionType::Query => format!(
            r#"//! Query: {snake_name}
//!
//! Queries are read-only database operations. They support:
//! - Real-time subscriptions (auto-refresh on data changes)
//! - Caching and deduplication
//! - Pagination helpers

use forge::prelude::*;

/// {snake_name} query.
#[forge::query]
pub async fn {snake_name}(ctx: &QueryContext) -> Result<Vec<()>> {{
    // Example: Fetch data from database
    // let items = sqlx::query_as!(
    //     Item,
    //     "SELECT * FROM items WHERE deleted_at IS NULL ORDER BY created_at DESC"
    // )
    // .fetch_all(ctx.db())
    // .await?;

    Ok(vec![])
}}
"#
        ),
        FunctionType::Mutation => format!(
            r#"//! Mutation: {snake_name}
//!
//! Mutations are write operations that modify data. They:
//! - Automatically invalidate affected subscriptions
//! - Support optimistic updates on the frontend
//! - Are wrapped in database transactions

use forge::prelude::*;

/// {snake_name} mutation.
#[forge::mutation]
pub async fn {snake_name}(ctx: &MutationContext) -> Result<()> {{
    // Example: Insert or update data
    // let id = Uuid::new_v4();
    // sqlx::query!(
    //     "INSERT INTO items (id, name) VALUES ($1, $2)",
    //     id,
    //     input.name
    // )
    // .execute(ctx.db())
    // .await?;

    Ok(())
}}
"#
        ),
        FunctionType::Action => format!(
            r#"//! Action: {snake_name}
//!
//! Actions are for external API calls and side effects. They:
//! - Are NOT wrapped in database transactions
//! - Should be idempotent when possible
//! - Can call external services (Stripe, SendGrid, etc.)
//!
//! Common use cases:
//! - Payment processing
//! - Email/SMS sending
//! - Third-party API calls
//! - File uploads to cloud storage

use forge::prelude::*;

/// Result from {snake_name} action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Result {{
    pub success: bool,
    // Add your result fields here
}}

/// {snake_name} action.
#[forge::action]
pub async fn {snake_name}(ctx: &ActionContext) -> Result<{pascal_name}Result> {{
    tracing::info!("Executing {snake_name} action");

    // Example: Call external API
    // let response = ctx.http_client()
    //     .post("https://api.example.com/endpoint")
    //     .json(&payload)
    //     .send()
    //     .await?;

    Ok({pascal_name}Result {{ success: true }})
}}
"#,
            pascal_name = to_pascal_case(&snake_name)
        ),
    };

    fs::write(&file_path, content)?;
    update_functions_mod(&snake_name)?;

    let description = match fn_type {
        FunctionType::Query => "query",
        FunctionType::Mutation => "mutation",
        FunctionType::Action => "action",
    };

    println!(
        "{} Created {}: {}",
        style("✅").green(),
        description,
        style(&file_path.display()).cyan()
    );

    Ok(())
}

/// Add a new job.
fn add_job(name: &str) -> Result<()> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}_job.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Job file already exists: {}", file_path.display());
    }

    let content = format!(
        r#"//! Background job: {snake_name}
//!
//! Jobs are used for async processing with automatic retry logic.
//! They are ideal for tasks that:
//! - May take a long time to complete
//! - May fail and need retry
//! - Should run in the background
//!
//! ## Dispatching this job
//!
//! ```rust
//! ctx.dispatch_job({snake_name}, {pascal_name}Input {{
//!     // your arguments
//! }}).await?;
//! ```

use forge::prelude::*;

/// Input for the {snake_name} job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Input {{
    // Add your input fields here
    // pub user_id: Uuid,
    // pub data: String,
}}

/// Output from the {snake_name} job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Output {{
    // Add your output fields here
    pub success: bool,
}}

/// {pascal_name} background job.
///
/// Configuration options:
/// - `timeout`: Maximum execution time (default: "5m")
/// - `max_attempts`: Number of retry attempts (default: 3)
/// - `backoff`: Retry backoff strategy: "exponential" or "linear" (default: "exponential")
#[forge::job(
    timeout = "5m",
    max_attempts = 3,
    backoff = "exponential"
)]
pub async fn {snake_name}(ctx: &JobContext, input: {pascal_name}Input) -> Result<{pascal_name}Output> {{
    tracing::info!(job_id = %ctx.job_id(), "Starting {snake_name} job");

    // Add your job logic here
    // Example: Process data, call external APIs, etc.

    // Report progress (visible in dashboard)
    ctx.set_progress(50, "Processing...").await?;

    // Simulate work
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    ctx.set_progress(100, "Complete").await?;
    tracing::info!(job_id = %ctx.job_id(), "Completed {snake_name} job");

    Ok({pascal_name}Output {{ success: true }})
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&format!("{}_job", snake_name))?;

    println!(
        "{} Created job: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );
    println!("   Job features: timeout, retry, progress tracking");

    Ok(())
}

/// Add a new cron task.
fn add_cron(name: &str) -> Result<()> {
    let snake_name = to_snake_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}_cron.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Cron file already exists: {}", file_path.display());
    }

    let content = format!(
        r#"//! Scheduled task: {snake_name}
//!
//! Cron tasks run on a schedule defined by a cron expression.
//!
//! ## Common cron schedules
//!
//! - `* * * * *`     - Every minute
//! - `0 * * * *`     - Every hour
//! - `0 0 * * *`     - Daily at midnight
//! - `0 9 * * *`     - Daily at 9 AM
//! - `0 0 * * 0`     - Weekly on Sunday
//! - `0 0 1 * *`     - Monthly on the 1st
//! - `0 0 * * 1-5`   - Weekdays at midnight
//!
//! Format: `second minute hour day-of-month month day-of-week`

use forge::prelude::*;

/// {snake_name} scheduled task.
///
/// Configuration options:
/// - `schedule`: Cron expression (required)
/// - `timezone`: Timezone for schedule (default: "UTC")
/// - `overlap`: Allow overlapping runs (default: false)
#[forge::cron(
    schedule = "0 0 * * *",  // Daily at midnight UTC
    timezone = "UTC",
    overlap = false
)]
pub async fn {snake_name}(ctx: &CronContext) -> Result<()> {{
    tracing::info!(run_id = %ctx.run_id(), "Running {snake_name}");

    // Get database pool for queries
    let pool = ctx.db();

    // Example: Query data and dispatch jobs
    // let items = sqlx::query!("SELECT * FROM items WHERE status = 'pending'")
    //     .fetch_all(pool)
    //     .await?;
    //
    // for item in items {{
    //     ctx.dispatch_job(process_item, ProcessItemInput {{ id: item.id }}).await?;
    // }}

    tracing::info!(run_id = %ctx.run_id(), "Completed {snake_name}");

    Ok(())
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&format!("{}_cron", snake_name))?;

    println!(
        "{} Created cron: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );
    println!("   Schedule: 0 0 * * * (daily at midnight)");
    println!("   Edit the schedule in the #[forge::cron] attribute");

    Ok(())
}

/// Add a new workflow.
fn add_workflow(name: &str) -> Result<()> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}_workflow.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Workflow file already exists: {}", file_path.display());
    }

    let content = format!(
        r#"//! Workflow: {snake_name}
//!
//! Workflows are multi-step processes with automatic state persistence.
//! Each step is durable - if the workflow fails, it resumes from the last
//! completed step. Steps can also define compensation (rollback) logic.
//!
//! ## Starting this workflow
//!
//! ```rust
//! let result = ctx.start_workflow({snake_name}, {pascal_name}Input {{
//!     // your input
//! }}).await?;
//! ```
//!
//! ## Key concepts
//!
//! - Steps are idempotent and re-executable
//! - Compensation runs in reverse order on failure
//! - Workflow state persists across restarts

use forge::prelude::*;

/// Input for the {snake_name} workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Input {{
    // Add your input fields here
    // pub user_id: Uuid,
    // pub order_id: Uuid,
}}

/// Output from the {snake_name} workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Output {{
    pub success: bool,
    // Add your output fields here
    // pub confirmation_id: String,
}}

/// {pascal_name} workflow.
///
/// Configuration options:
/// - `version`: Workflow version for migrations (default: 1)
/// - `timeout`: Maximum workflow duration (default: "1h")
#[forge::workflow(version = 1, timeout = "1h")]
pub async fn {snake_name}(ctx: &WorkflowContext, input: {pascal_name}Input) -> Result<{pascal_name}Output> {{
    tracing::info!(workflow_id = %ctx.workflow_id(), "Starting {snake_name} workflow");

    // Step 1: First step (with compensation)
    let step1_result = ctx.step("validate")
        .run(|| async {{
            tracing::info!("Step 1: Validating input");
            // Validation logic here
            Ok("validated")
        }})
        .compensate(|_result| async {{
            tracing::info!("Compensating step 1: Cleanup validation");
            // Rollback validation side effects if any
            Ok(())
        }})
        .await?;

    // Step 2: Second step (depends on step 1)
    let step2_result = ctx.step("process")
        .run(|| async {{
            tracing::info!("Step 2: Processing");
            // Main processing logic
            // This step has access to step1_result
            Ok("processed")
        }})
        .compensate(|_result| async {{
            tracing::info!("Compensating step 2: Undo processing");
            // Rollback processing
            Ok(())
        }})
        .await?;

    // Step 3: Final step (no compensation needed)
    ctx.step("notify")
        .run(|| async {{
            tracing::info!("Step 3: Sending notification");
            // Send confirmation email, webhook, etc.
            Ok(())
        }})
        .await?;

    tracing::info!(workflow_id = %ctx.workflow_id(), "Completed {snake_name} workflow");

    Ok({pascal_name}Output {{ success: true }})
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&format!("{}_workflow", snake_name))?;

    println!(
        "{} Created workflow: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );
    println!("   Features: durable steps, compensation, automatic retry");

    Ok(())
}

/// Update src/schema/mod.rs to include the new model.
fn update_schema_mod(snake_name: &str, pascal_name: &str) -> Result<()> {
    let mod_path = Path::new("src/schema/mod.rs");
    let content = fs::read_to_string(mod_path).unwrap_or_default();

    let mod_decl = format!("pub mod {};", snake_name);

    // Check if already declared
    if content.contains(&mod_decl) {
        println!(
            "  {} {} already declared in mod.rs",
            style("ℹ").blue(),
            snake_name
        );
        return Ok(());
    }

    // Build new content without extra blank lines
    let mut new_content = content.trim_end().to_string();
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(&mod_decl);
    new_content.push('\n');
    new_content.push_str(&format!("pub use {}::{};\n", snake_name, pascal_name));

    fs::write(mod_path, new_content)?;
    Ok(())
}

/// Update src/functions/mod.rs to include the new function.
fn update_functions_mod(snake_name: &str) -> Result<()> {
    let mod_path = Path::new("src/functions/mod.rs");
    let content = fs::read_to_string(mod_path).unwrap_or_default();

    let mod_decl = format!("pub mod {};", snake_name);

    // Check if already declared
    if content.contains(&mod_decl) {
        println!(
            "  {} {} already declared in mod.rs",
            style("ℹ").blue(),
            snake_name
        );
        return Ok(());
    }

    // Build new content without extra blank lines
    let mut new_content = content.trim_end().to_string();
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(&mod_decl);
    new_content.push('\n');
    new_content.push_str(&format!("pub use {}::*;\n", snake_name));

    fs::write(mod_path, new_content)?;
    Ok(())
}

/// Convert to PascalCase.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();

    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else if c == '-' {
            result.push('_');
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("user"), "User");
        assert_eq!(to_pascal_case("order_item"), "OrderItem");
        assert_eq!(to_pascal_case("my-component"), "MyComponent");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("User"), "user");
        assert_eq!(to_snake_case("OrderItem"), "order_item");
        assert_eq!(to_snake_case("MyComponent"), "my_component");
    }
}

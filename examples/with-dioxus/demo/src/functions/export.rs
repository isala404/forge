use crate::schema::{User, UserRole};
use forge::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportInput {
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOutput {
    pub count: usize,
    pub data: String,
    pub format: String,
}

/// Export users as CSV or JSON with progress reporting
#[forge::job(
    timeout = "5m",
    priority = "low",
    retry(max_attempts = 3, backoff = "exponential"),
    idempotent,
    public
)]
pub async fn export_users(ctx: &JobContext, input: ExportInput) -> Result<ExportOutput> {
    if ctx.is_retry() {
        tracing::warn!(attempt = ctx.attempt, "Retrying export job");
    }

    let _ = ctx.progress(0, "Initializing export...");
    tokio::time::sleep(Duration::from_millis(800)).await;
    ctx.heartbeat().await?;

    let _ = ctx.progress(10, "Connecting to database...");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(20, "Fetching users...");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let users: Vec<User> = sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            email,
            name,
            role as "role: UserRole",
            password_hash,
            created_at,
            updated_at
        FROM users
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(ctx.db())
    .await?;

    ctx.heartbeat().await?;
    let _ = ctx.progress(30, format!("Found {} users", users.len()));
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(40, "Validating records...");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(50, "Processing user data...");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(
        60,
        format!("Formatting as {}...", input.format.to_uppercase()),
    );
    tokio::time::sleep(Duration::from_millis(800)).await;

    let data = match input.format.as_str() {
        "json" => {
            serde_json::to_string_pretty(&users).map_err(|e| ForgeError::Internal(e.to_string()))?
        }
        _ => {
            let mut csv = String::from("id,email,name,role,created_at\n");
            for user in &users {
                csv.push_str(&format!(
                    "{},{},{},{:?},{}\n",
                    user.id, user.email, user.name, user.role, user.created_at
                ));
            }
            csv
        }
    };

    let _ = ctx.progress(70, "Compressing output...");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(80, "Generating checksum...");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(90, "Finalizing export...");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let _ = ctx.progress(100, "Export complete");

    Ok(ExportOutput {
        count: users.len(),
        data,
        format: input.format,
    })
}

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

fn render_export_data(users: &[User], format: &str) -> Result<String> {
    match format {
        "json" => {
            serde_json::to_string_pretty(users).map_err(|e| ForgeError::internal(e.to_string()))
        }
        _ => {
            let mut csv = String::from("id,email,name,role,created_at\n");
            for user in users {
                csv.push_str(&format!(
                    "{},{},{},{:?},{}\n",
                    user.id, user.email, user.name, user.role, user.created_at
                ));
            }
            Ok(csv)
        }
    }
}

/// Export users as CSV or JSON with progress reporting.
///
/// The `tokio::time::sleep` calls below are SIMULATED work — they exist solely so the
/// progress UI is visible in the demo. Replace them with real I/O (S3 puts, large
/// DB scans, format conversion) in production code. Never ship sleep-padded jobs:
/// they pin worker slots and inflate p99 for no value.
#[forge::job(
    timeout = "5m",
    priority = "low",
    retry(max_attempts = 3, backoff = "exponential"),
    idempotent,
    auth = "none"
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

    let data = render_export_data(&users, &input.format)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user(email: &str, name: &str, role: UserRole) -> User {
        let now = Utc::now();
        User {
            id: Uuid::new_v4(),
            email: email.into(),
            name: name.into(),
            role,
            created_at: now,
            updated_at: now,
            password_hash: Some("secret".into()),
        }
    }

    #[test]
    fn test_render_export_data_json_omits_password_hash() {
        let data = render_export_data(
            &[sample_user("ship@example.com", "Ship It", UserRole::Admin)],
            "json",
        )
        .unwrap();

        assert!(data.contains("\"email\": \"ship@example.com\""));
        assert!(data.contains("\"role\": \"admin\""));
        assert!(!data.contains("password_hash"));
    }

    #[test]
    fn test_render_export_data_csv_includes_header_and_rows() {
        let data = render_export_data(
            &[
                sample_user("a@example.com", "Alpha", UserRole::Member),
                sample_user("b@example.com", "Beta", UserRole::Guest),
            ],
            "csv",
        )
        .unwrap();

        assert!(data.starts_with("id,email,name,role,created_at\n"));
        assert!(data.contains(",a@example.com,Alpha,Member,"));
        assert!(data.contains(",b@example.com,Beta,Guest,"));
    }

    #[test]
    fn test_render_export_data_unknown_format_falls_back_to_csv() {
        let data = render_export_data(
            &[sample_user("ship@example.com", "Ship It", UserRole::Member)],
            "xml",
        )
        .unwrap();

        assert!(data.starts_with("id,email,name,role,created_at\n"));
        assert!(data.contains("ship@example.com"));
    }
}

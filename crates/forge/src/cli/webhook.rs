use clap::{Args, Subcommand};
use forge_core::error::Result;

/// Webhook management commands.
#[derive(Args)]
pub struct WebhookCommand {
    #[command(subcommand)]
    command: WebhookSubcommand,
}

#[derive(Subcommand)]
enum WebhookSubcommand {
    /// Replay a previously received webhook by its idempotency key.
    ///
    /// Fetches the stored raw body and headers from forge_webhook_events
    /// and re-submits them to the local webhook endpoint.
    Replay(ReplayArgs),

    /// List recent webhook events.
    List(ListArgs),
}

#[derive(Args)]
struct ReplayArgs {
    /// Webhook name (as registered in the handler).
    webhook_name: String,
    /// Idempotency key of the event to replay.
    idempotency_key: String,
    /// Base URL of the running forge server (default: http://localhost:9081).
    #[arg(long, default_value = "http://localhost:9081")]
    base_url: String,
}

#[derive(Args)]
struct ListArgs {
    /// Filter by webhook name.
    #[arg(long)]
    name: Option<String>,
    /// Filter by status (claimed, completed, failed).
    #[arg(long)]
    status: Option<String>,
    /// Maximum number of events to show.
    #[arg(long, default_value = "20")]
    limit: i64,
}

impl WebhookCommand {
    /// Execute the webhook subcommand.
    pub async fn execute(self) -> Result<()> {
        // Webhook subcommands rely on cwd-relative `forge.toml`; anchor at project root.
        if let Err(e) = super::project_root::enter_project_root() {
            return Err(forge_core::ForgeError::config(format!(
                "must be run from inside a forge project: {e}"
            )));
        }
        match self.command {
            WebhookSubcommand::Replay(args) => replay(args).await,
            WebhookSubcommand::List(args) => list(args).await,
        }
    }
}

/// Mirror the runtime's URL resolution: prefer `DATABASE_URL` env var, then
/// fall back to `[database].url` from forge.toml.
fn resolve_database_url(config: &forge_core::config::ForgeConfig) -> Result<String> {
    if let Ok(url) = std::env::var("DATABASE_URL")
        && !url.is_empty()
    {
        return Ok(url);
    }
    let url = config.database.url();
    if url.is_empty() {
        return Err(forge_core::ForgeError::config(
            "no database URL configured: set DATABASE_URL or [database].url in forge.toml",
        ));
    }
    Ok(url.to_string())
}

#[allow(clippy::disallowed_methods)]
async fn replay(args: ReplayArgs) -> Result<()> {
    let config = forge_core::config::ForgeConfig::from_file("forge.toml")?;
    let db_url = resolve_database_url(&config)?;
    let pool = sqlx::PgPool::connect(&db_url)
        .await
        .map_err(forge_core::ForgeError::Database)?;

    let row: Option<(Option<Vec<u8>>, Option<serde_json::Value>, String)> = sqlx::query_as(
        "SELECT raw_body, raw_headers, status \
         FROM forge_webhook_events \
         WHERE webhook_name = $1 AND idempotency_key = $2",
    )
    .bind(&args.webhook_name)
    .bind(&args.idempotency_key)
    .fetch_optional(&pool)
    .await
    .map_err(forge_core::ForgeError::Database)?;

    let (raw_body, raw_headers, status) = match row {
        Some(r) => r,
        None => {
            println!(
                "No webhook event found for {} / {}",
                args.webhook_name, args.idempotency_key
            );
            return Ok(());
        }
    };

    let body = match raw_body {
        Some(b) => b,
        None => {
            println!(
                "Webhook event found (status: {status}) but raw body was not stored. \
                 Replay data is only available for events captured by a runtime that \
                 stores raw_body/raw_headers."
            );
            return Ok(());
        }
    };

    println!(
        "Replaying {} / {} (status: {}, {} bytes)",
        args.webhook_name,
        args.idempotency_key,
        status,
        body.len()
    );

    let client = reqwest::Client::new();
    let mut request = client.post(format!(
        "{}/webhooks/{}",
        args.base_url.trim_end_matches('/'),
        args.webhook_name
    ));

    if let Some(headers) = raw_headers
        && let Some(obj) = headers.as_object()
    {
        for (key, value) in obj {
            if let Some(v) = value.as_str()
                && let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                && let Ok(val) = reqwest::header::HeaderValue::from_str(v)
            {
                request = request.header(name, val);
            }
        }
    }

    let response =
        request.body(body).send().await.map_err(|e| {
            forge_core::ForgeError::internal_with("Failed to send replay request", e)
        })?;

    let status_code = response.status();
    let reason = status_code.canonical_reason().unwrap_or_default();
    println!("Response: {} {}", status_code.as_u16(), reason);

    // Only clear the dedup record on 2xx so a failed replay doesn't allow
    // the original event to silently re-execute as a brand-new delivery.
    if status_code.is_success() {
        if let Err(e) = sqlx::query(
            "DELETE FROM forge_webhook_events \
             WHERE webhook_name = $1 AND idempotency_key = $2",
        )
        .bind(&args.webhook_name)
        .bind(&args.idempotency_key)
        .execute(&pool)
        .await
        {
            eprintln!("Warning: replay succeeded but failed to clear dedup record: {e}");
        }
    } else {
        eprintln!(
            "Replay did not succeed (HTTP {}); dedup record preserved.",
            status_code.as_u16()
        );
    }

    let body = response
        .text()
        .await
        .map_err(|e| forge_core::ForgeError::internal_with("Failed to read response body", e))?;
    if !body.is_empty() {
        println!("{body}");
    }

    Ok(())
}

#[allow(clippy::disallowed_methods)]
async fn list(args: ListArgs) -> Result<()> {
    let config = forge_core::config::ForgeConfig::from_file("forge.toml")?;
    let db_url = resolve_database_url(&config)?;
    let pool = sqlx::PgPool::connect(&db_url)
        .await
        .map_err(forge_core::ForgeError::Database)?;

    let rows: Vec<(String, String, String, chrono::DateTime<chrono::Utc>, bool)> =
        if let Some(ref name) = args.name {
            sqlx::query_as(
                "SELECT webhook_name, idempotency_key, status, processed_at, \
                    raw_body IS NOT NULL as has_body \
             FROM forge_webhook_events \
             WHERE webhook_name = $1 \
             ORDER BY processed_at DESC \
             LIMIT $2",
            )
            .bind(name)
            .bind(args.limit)
            .fetch_all(&pool)
            .await
            .map_err(forge_core::ForgeError::Database)?
        } else {
            sqlx::query_as(
                "SELECT webhook_name, idempotency_key, status, processed_at, \
                    raw_body IS NOT NULL as has_body \
             FROM forge_webhook_events \
             ORDER BY processed_at DESC \
             LIMIT $1",
            )
            .bind(args.limit)
            .fetch_all(&pool)
            .await
            .map_err(forge_core::ForgeError::Database)?
        };

    if rows.is_empty() {
        println!("No webhook events found.");
        return Ok(());
    }

    println!(
        "{:<20} {:<30} {:<10} {:<24} REPLAY",
        "WEBHOOK", "IDEMPOTENCY_KEY", "STATUS", "PROCESSED_AT"
    );
    for (webhook, key, status, processed_at, has_body) in &rows {
        let replay = if *has_body { "yes" } else { "no" };
        let key_display: String = if key.chars().count() > 28 {
            key.chars().take(28).collect()
        } else {
            key.clone()
        };
        println!(
            "{:<20} {:<30} {:<10} {:<24} {}",
            webhook,
            key_display,
            status,
            processed_at.format("%Y-%m-%d %H:%M:%S UTC"),
            replay
        );
    }

    Ok(())
}

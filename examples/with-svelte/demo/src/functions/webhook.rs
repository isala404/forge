use forge::prelude::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Webhook event record stored in database
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct WebhookEvent {
    pub id: Uuid,
    pub idempotency_key: String,
    pub webhook_name: String,
    pub processed_at: Timestamp,
}

/// Get the 4 most recent webhook events
#[forge::query(auth = "none")]
pub async fn get_webhook_events(ctx: &QueryContext) -> Result<Vec<WebhookEvent>> {
    sqlx::query_as!(
        WebhookEvent,
        r#"
        SELECT id, idempotency_key, webhook_name, processed_at
        FROM webhook_events
        ORDER BY processed_at DESC
        LIMIT 4
        "#
    )
    .fetch_all(ctx.db())
    .await
    .map_err(Into::into)
}

/// Generic webhook endpoint demonstrating:
/// - HMAC-SHA256 signature validation
/// - Configurable idempotency key extraction from header
/// - Deduplication of repeated requests
///
/// The idempotency key can be sent via X-Idempotency-Key header.
/// Requests with the same key will be deduplicated.
#[forge::webhook(
    path = "/webhooks/demo",
    signature = WebhookSignature::hmac_sha256("X-Webhook-Signature", "WEBHOOK_SECRET"),
    idempotency = "header:X-Idempotency-Key",
)]
pub async fn demo_webhook(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> {
    let idempotency_key = ctx.idempotency_key.clone().unwrap_or_default();

    tracing::info!(
        idempotency_key = %idempotency_key,
        "Webhook received"
    );

    // Store the webhook event in the database
    sqlx::query!(
        "INSERT INTO webhook_events (id, idempotency_key, webhook_name, payload, processed_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, NOW())",
        &idempotency_key,
        "demo",
        &payload
    )
    .execute(ctx.db())
    .await?;

    Ok(WebhookResult::Accepted)
}

/// Server-side trigger for the demo webhook. The HMAC signing secret lives only
/// on the backend; the browser never sees `WEBHOOK_SECRET`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TriggerDemoWebhookInput {
    pub idempotency_key: String,
}

#[forge::mutation(transactional = false)]
pub async fn trigger_demo_webhook(
    ctx: &MutationContext,
    input: TriggerDemoWebhookInput,
) -> Result<bool> {
    let secret = ctx.env_require("WEBHOOK_SECRET")?;
    let port: u16 = ctx.env_parse_or("PORT", 9081u16)?;
    let payload = serde_json::json!({
        "action": "test",
        "ts": Utc::now().timestamp_millis(),
    })
    .to_string();

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .map_err(|e| ForgeError::internal(format!("HMAC key init failed: {e}")))?;
    mac.update(payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    let timestamp = Utc::now().timestamp();

    // Deliberate loopback call to this server's own webhook endpoint. The
    // framework's `ctx.http()` client blocks private/loopback IPs (SSRF guard),
    // so use a plain reqwest client for this intentional self-call.
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/_api/webhooks/demo"))
        .header("Content-Type", "application/json")
        .header("X-Webhook-Signature", signature)
        .header("X-Webhook-Timestamp", timestamp.to_string())
        .header("X-Idempotency-Key", &input.idempotency_key)
        .body(payload)
        .send()
        .await
        .map_err(|e| ForgeError::internal(format!("Webhook self-call failed: {e}")))?;

    if !response.status().is_success() {
        return Err(ForgeError::internal(format!(
            "Webhook returned status {}",
            response.status().as_u16()
        )));
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use forge::testing::TestWebhookContext;

    #[test]
    fn test_webhook_context_creation() {
        let ctx = TestWebhookContext::builder("demo_webhook")
            .with_idempotency_key("unique-key-123")
            .build();

        assert_eq!(ctx.webhook_name, "demo_webhook");
        assert_eq!(ctx.idempotency_key, Some("unique-key-123".to_string()));
    }

    #[test]
    fn test_webhook_header_access() {
        let ctx = TestWebhookContext::builder("demo_webhook")
            .with_header("X-Custom-Header", "custom-value")
            .with_header("Authorization", "Bearer token123")
            .build();

        // Headers are case-insensitive
        assert_eq!(ctx.header("x-custom-header"), Some("custom-value"));
        assert_eq!(ctx.header("X-CUSTOM-HEADER"), Some("custom-value"));
        assert_eq!(ctx.header("authorization"), Some("Bearer token123"));
        assert_eq!(ctx.header("missing"), None);
    }

    #[tokio::test]
    async fn test_webhook_dispatches_job() {
        let ctx = TestWebhookContext::builder("demo_webhook").build();

        ctx.dispatch_job("process_event", serde_json::json!({"action": "test"}))
            .await
            .unwrap();

        ctx.job_dispatch().assert_dispatched("process_event");
    }

    #[test]
    fn test_webhook_request_metadata() {
        let ctx = TestWebhookContext::builder("demo_webhook")
            .with_request_id("req-abc-123")
            .with_idempotency_key("idem-xyz-789")
            .build();

        assert_eq!(ctx.request_id, "req-abc-123");
        assert_eq!(ctx.idempotency_key, Some("idem-xyz-789".to_string()));
    }
}

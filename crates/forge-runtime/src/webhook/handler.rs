//! Axum handler for webhook requests with signature validation.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use forge_core::CircuitBreakerClient;
use forge_core::function::JobDispatch;
use forge_core::webhook::{
    IdempotencySource, REPLAY_TIMESTAMP_HEADER, SignatureAlgorithm, WebhookContext,
};
use hmac::{Hmac, Mac};
use ring::signature::{self, UnparsedPublicKey};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::registry::WebhookRegistry;
use crate::gateway::RpcError;

/// State for webhook handler.
#[derive(Clone)]
pub struct WebhookState {
    registry: Arc<WebhookRegistry>,
    pool: PgPool,
    http_client: CircuitBreakerClient,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
}

impl WebhookState {
    /// Create new webhook state.
    pub fn new(registry: Arc<WebhookRegistry>, pool: PgPool) -> Self {
        Self {
            registry,
            pool,
            http_client: CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            job_dispatcher: None,
        }
    }

    /// Set job dispatcher.
    pub fn with_job_dispatcher(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        self.job_dispatcher = Some(dispatcher);
        self
    }
}

/// Handle webhook requests.
///
/// This handler:
/// 1. Looks up webhook by path
/// 2. Validates signature if configured
/// 3. Checks idempotency
/// 4. Executes handler
/// 5. Records idempotency key
pub async fn webhook_handler(
    State(state): State<Arc<WebhookState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let full_path = format!("/webhooks/{}", path);
    let request_id = Uuid::new_v4().to_string();

    // Look up webhook by path
    let entry = match state.registry.get_by_path(&full_path) {
        Some(e) => e,
        None => {
            warn!(path = %full_path, "Webhook not found");
            return (
                StatusCode::NOT_FOUND,
                Json(RpcError::not_found("Webhook not found")),
            )
                .into_response();
        }
    };

    let info = &entry.info;
    info!(
        webhook = info.name,
        path = %full_path,
        request_id = %request_id,
        "Webhook request received"
    );

    if info.signature.is_none() && !info.allow_unsigned {
        warn!(
            webhook = info.name,
            "Unsigned webhook rejected (set allow_unsigned to opt in)"
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(RpcError::unauthorized("Webhook signature is required")),
        )
            .into_response();
    }

    // Validate signature if configured
    if let Some(ref sig_config) = info.signature {
        // Get signature from header
        let signature = match headers
            .get(sig_config.header_name)
            .and_then(|v| v.to_str().ok())
        {
            Some(s) => s,
            None => {
                warn!(webhook = info.name, "Missing signature header");
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(RpcError::unauthorized("Missing signature")),
                )
                    .into_response();
            }
        };

        // Get secret(s) from environment. Comma-separated values support
        // rotation: set to "new-secret,old-secret" during rollover.
        let secrets_raw = match std::env::var(sig_config.secret_env) {
            Ok(s) => s,
            Err(_) => {
                error!(
                    webhook = info.name,
                    env = sig_config.secret_env,
                    "Webhook secret not configured"
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(RpcError::internal("Webhook configuration error")),
                )
                    .into_response();
            }
        };

        // Try each secret, first match wins
        let secrets: Vec<&str> = secrets_raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let signature_valid = secrets.iter().any(|secret| {
            validate_signature(
                sig_config.algorithm,
                &body,
                secret,
                signature,
                &headers,
                sig_config.replay_window_secs,
            )
        });
        if !signature_valid {
            warn!(webhook = info.name, "Invalid signature");
            return (
                StatusCode::UNAUTHORIZED,
                Json(RpcError::unauthorized("Invalid signature")),
            )
                .into_response();
        }
    }

    // Extract idempotency key if configured
    let idempotency_key = if let Some(ref idem_config) = info.idempotency {
        match &idem_config.source {
            IdempotencySource::Header(header_name) => headers
                .get(*header_name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            IdempotencySource::Body(json_path) => {
                // Parse body and extract value using JSON path
                if let Ok(payload) = serde_json::from_slice::<Value>(&body) {
                    extract_json_path(&payload, json_path)
                } else {
                    None
                }
            }
            // Future IdempotencySource variants: skip key extraction.
            _ => None,
        }
    } else {
        None
    };

    // Atomically claim idempotency key before execution.
    let mut idempotency_claimed = false;
    if let Some(ref key) = idempotency_key
        && let Some(ref idem_config) = info.idempotency
    {
        match claim_idempotency(
            &state.pool,
            info.name,
            key,
            idem_config.ttl,
            idem_config.processing_timeout,
        )
        .await
        {
            Ok(true) => {
                idempotency_claimed = true;
            }
            Ok(false) => {
                info!(
                    webhook = info.name,
                    idempotency_key = %key,
                    "Request already processed (idempotent)"
                );
                return (StatusCode::OK, Json(json!({"status": "already_processed"})))
                    .into_response();
            }
            Err(e) => {
                // Fail closed: if idempotency is configured but the DB is unavailable,
                // reject the request rather than processing without replay protection
                error!(webhook = info.name, error = %e, "Failed to claim idempotency key -- rejecting request");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(RpcError::new(
                        "SERVICE_UNAVAILABLE",
                        "Service temporarily unavailable",
                    )),
                )
                    .into_response();
            }
        }
    }

    // Parse payload
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            if idempotency_claimed
                && let Some(ref key) = idempotency_key
                && let Err(release_err) = release_idempotency(&state.pool, info.name, key).await
            {
                warn!(
                    webhook = info.name,
                    error = %release_err,
                    "Failed to release idempotency key after JSON parse failure"
                );
            }
            warn!(webhook = info.name, error = %e, "Invalid JSON payload");
            return (
                StatusCode::BAD_REQUEST,
                Json(RpcError::validation("Invalid JSON")),
            )
                .into_response();
        }
    };

    // Build headers map (lowercase keys)
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_lowercase(), v.to_string()))
        })
        .collect();

    // TODO(pre-1.0): Replace WebhookContext with MutationContext for dispatch atomicity
    let mut ctx = WebhookContext::new(
        info.name.to_string(),
        request_id.clone(),
        header_map,
        state.pool.clone(),
        state.http_client.clone(),
    )
    .with_idempotency_key(idempotency_key.clone());
    ctx.set_http_timeout(info.http_timeout);

    if let Some(ref dispatcher) = state.job_dispatcher {
        ctx = ctx.with_job_dispatch(dispatcher.clone());
    }

    // Execute handler with timeout
    let exec_start = std::time::Instant::now();
    let result = tokio::time::timeout(info.timeout, (entry.handler)(&ctx, payload)).await;
    let exec_duration_ms = exec_start.elapsed().as_millis().min(i32::MAX as u128) as i32;

    match result {
        Ok(Ok(webhook_result)) => {
            if idempotency_claimed
                && let Some(ref key) = idempotency_key
                && let Err(complete_err) = complete_idempotency(&state.pool, info.name, key).await
            {
                warn!(
                    webhook = info.name,
                    error = %complete_err,
                    "Failed to mark idempotency key as completed"
                );
            }
            let status =
                StatusCode::from_u16(webhook_result.status_code()).unwrap_or(StatusCode::OK);
            crate::signals::emit_server_execution(
                info.name,
                "webhook",
                exec_duration_ms,
                status.is_success(),
                None,
            );
            (status, Json(webhook_result.body())).into_response()
        }
        Ok(Err(e)) => {
            if idempotency_claimed
                && let Some(ref key) = idempotency_key
                && let Err(release_err) = release_idempotency(&state.pool, info.name, key).await
            {
                warn!(
                    webhook = info.name,
                    error = %release_err,
                    "Failed to release idempotency key after handler error"
                );
            }
            let err_str = e.to_string();
            error!(webhook = info.name, error = %e, "Webhook handler error");
            crate::signals::emit_server_execution(
                info.name,
                "webhook",
                exec_duration_ms,
                false,
                Some(err_str),
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RpcError::with_details(
                    "INTERNAL_ERROR",
                    "Internal server error",
                    json!({ "request_id": request_id }),
                )),
            )
                .into_response()
        }
        Err(_) => {
            if idempotency_claimed
                && let Some(ref key) = idempotency_key
                && let Err(release_err) = release_idempotency(&state.pool, info.name, key).await
            {
                warn!(
                    webhook = info.name,
                    error = %release_err,
                    "Failed to release idempotency key after timeout"
                );
            }
            error!(
                webhook = info.name,
                timeout = ?info.timeout,
                "Webhook handler timed out"
            );
            crate::signals::emit_server_execution(
                info.name,
                "webhook",
                exec_duration_ms,
                false,
                Some(format!("Webhook timed out after {:?}", info.timeout)),
            );
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(RpcError::new("TIMEOUT", "Request timeout")),
            )
                .into_response()
        }
    }
}

/// Validate webhook signature, dispatching to the appropriate algorithm.
///
/// Stripe handles its own timestamp via the `t=` field. All other schemes
/// require an `x-webhook-timestamp` header carrying unix seconds; the request
/// is rejected as a replay when the difference from `now` falls outside
/// `replay_window_secs`. A `replay_window_secs` of 0 disables enforcement.
fn validate_signature(
    algorithm: SignatureAlgorithm,
    body: &[u8],
    secret: &str,
    signature: &str,
    headers: &HeaderMap,
    replay_window_secs: u64,
) -> bool {
    if !matches!(algorithm, SignatureAlgorithm::StripeWebhooks)
        && replay_window_secs > 0
        && !timestamp_within_replay_window(headers, replay_window_secs)
    {
        return false;
    }
    match algorithm {
        SignatureAlgorithm::StripeWebhooks => validate_stripe_webhooks(body, secret, signature),
        SignatureAlgorithm::HmacSha256Base64 => {
            validate_hmac_sha256_base64(body, secret, signature)
        }
        SignatureAlgorithm::Ed25519 => validate_ed25519(body, secret, signature),
        SignatureAlgorithm::HmacSha256 => {
            let sig_hex = signature
                .strip_prefix(SignatureAlgorithm::HmacSha256.prefix())
                .unwrap_or(signature);
            let expected = match decode_hex(sig_hex) {
                Some(b) => b,
                None => return false,
            };
            let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
                Ok(m) => m,
                Err(_) => return false,
            };
            mac.update(body);
            mac.verify_slice(&expected).is_ok()
        }
        // Future variants added to SignatureAlgorithm are caught here until handler support lands.
        _ => false,
    }
}

/// Reject requests whose `x-webhook-timestamp` header is missing, malformed,
/// dated in the future, or older than `window_secs`. Returns `true` only when
/// the request falls inside the window.
fn timestamp_within_replay_window(headers: &HeaderMap, window_secs: u64) -> bool {
    let Some(ts_str) = headers
        .get(REPLAY_TIMESTAMP_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Ok(ts) = ts_str.parse::<i64>() else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    let window = i64::try_from(window_secs).unwrap_or(i64::MAX);
    let age = now.saturating_sub(ts);
    age >= 0 && age <= window
}

/// Validate a Stripe webhook signature.
///
/// - Header format: `t=1234567890,v1=<hex>,v1=<hex>`
/// - Signed content: `{timestamp}.{body}`
/// - Rejects requests where the timestamp is more than 5 minutes old.
fn validate_stripe_webhooks(body: &[u8], secret: &str, signature_header: &str) -> bool {
    let mut timestamp: Option<&str> = None;
    let mut signatures: Vec<&str> = Vec::new();

    for part in signature_header.split(',') {
        if let Some(t) = part.strip_prefix("t=") {
            timestamp = Some(t);
        } else if let Some(sig) = part.strip_prefix("v1=") {
            signatures.push(sig);
        }
    }

    let timestamp = match timestamp {
        Some(t) => t,
        None => return false,
    };

    // Replay protection: reject if timestamp is more than 5 minutes off
    let ts: i64 = match timestamp.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    if (chrono::Utc::now().timestamp() - ts).abs() > 300 {
        return false;
    }

    let mut signed = Vec::with_capacity(timestamp.len() + 1 + body.len());
    signed.extend_from_slice(timestamp.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);

    // Constant-time comparison: decode each candidate hex signature to raw
    // bytes and use HMAC's `verify_slice`. Comparing hex strings directly
    // short-circuits on the first mismatch and leaks per-byte timing.
    for sig in signatures {
        let Some(decoded) = decode_hex(sig) else {
            continue;
        };
        let mut verifier = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
            Ok(v) => v,
            Err(_) => return false,
        };
        verifier.update(&signed);
        if verifier.verify_slice(&decoded).is_ok() {
            return true;
        }
    }
    false
}

/// Validate a Shopify (HMAC-SHA256, base64-encoded) webhook signature.
fn validate_hmac_sha256_base64(body: &[u8], secret: &str, signature: &str) -> bool {
    // Decode the client-supplied base64 signature back to raw HMAC bytes and
    // verify with a constant-time comparator. Comparing the base64 strings
    // directly leaks per-byte timing on string equality.
    let Ok(provided) = general_purpose::STANDARD.decode(signature) else {
        return false;
    };
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(&provided).is_ok()
}

/// Validate an Ed25519 asymmetric webhook signature.
///
/// `public_key_b64` is a base64-encoded 32-byte Ed25519 public key.
/// `signature_b64` is a base64-encoded 64-byte Ed25519 signature over the body.
fn validate_ed25519(body: &[u8], public_key_b64: &str, signature_b64: &str) -> bool {
    let pub_key_bytes = match general_purpose::STANDARD.decode(public_key_b64) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let sig_bytes = match general_purpose::STANDARD.decode(signature_b64) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let peer_public_key = UnparsedPublicKey::new(&signature::ED25519, &pub_key_bytes);
    peer_public_key.verify(body, &sig_bytes).is_ok()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Extract value from JSON using a simple path (e.g., "$.id" or "$.data.id").
fn extract_json_path(value: &Value, path: &str) -> Option<String> {
    let path = path.strip_prefix("$.").unwrap_or(path);
    let parts: Vec<&str> = path.split('.').collect();

    let mut current = value;
    for part in parts {
        current = current.get(part)?;
    }

    match current {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => Some(current.to_string()),
    }
}

/// Atomically claim idempotency key before processing.
///
/// Returns:
/// - `Ok(true)` if this request acquired the claim
/// - `Ok(false)` if key is already active (completed or being processed)
///
/// A key with `status = 'claimed'` is eligible for reclaim once
/// `processing_timeout` has elapsed (crash recovery).
async fn claim_idempotency(
    pool: &PgPool,
    webhook_name: &str,
    key: &str,
    ttl: std::time::Duration,
    processing_timeout: std::time::Duration,
) -> Result<bool, sqlx::Error> {
    let expires_at =
        chrono::Utc::now() + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(24));
    let processing_timeout_secs = processing_timeout.as_secs_f64();

    let result = sqlx::query!(
        r#"
        INSERT INTO forge_webhook_events (idempotency_key, webhook_name, status, processed_at, expires_at)
        VALUES ($1, $2, 'claimed', NOW(), $3)
        ON CONFLICT (webhook_name, idempotency_key) DO UPDATE
            SET status = 'claimed',
                processed_at = NOW(),
                expires_at = EXCLUDED.expires_at
        WHERE forge_webhook_events.expires_at < NOW()
           OR (forge_webhook_events.status = 'claimed'
               AND forge_webhook_events.processed_at + make_interval(secs => $4) < NOW())
        "#,
        key,
        webhook_name,
        expires_at,
        processing_timeout_secs,
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Mark idempotency key as completed after successful processing.
async fn complete_idempotency(
    pool: &PgPool,
    webhook_name: &str,
    key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE forge_webhook_events
        SET status = 'completed'
        WHERE webhook_name = $1 AND idempotency_key = $2
        "#,
        webhook_name,
        key,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Release idempotency key after failure so retries can proceed.
async fn release_idempotency(
    pool: &PgPool,
    webhook_name: &str,
    key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        DELETE FROM forge_webhook_events
        WHERE webhook_name = $1 AND idempotency_key = $2
        "#,
        webhook_name,
        key,
    )
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn encode_hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            })
    }

    #[test]
    fn test_extract_json_path_simple() {
        let value = json!({"id": "test-123"});
        assert_eq!(
            extract_json_path(&value, "$.id"),
            Some("test-123".to_string())
        );
    }

    #[test]
    fn test_extract_json_path_nested() {
        let value = json!({"data": {"id": "nested-456"}});
        assert_eq!(
            extract_json_path(&value, "$.data.id"),
            Some("nested-456".to_string())
        );
    }

    #[test]
    fn test_extract_json_path_number() {
        let value = json!({"count": 42});
        assert_eq!(extract_json_path(&value, "$.count"), Some("42".to_string()));
    }

    #[test]
    fn test_extract_json_path_missing() {
        let value = json!({"other": "value"});
        assert_eq!(extract_json_path(&value, "$.id"), None);
    }

    fn fresh_timestamp_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        let now = chrono::Utc::now().timestamp().to_string();
        h.insert(REPLAY_TIMESTAMP_HEADER, now.parse().unwrap());
        h
    }

    #[test]
    fn test_validate_signature_sha256() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"test payload";
        let secret = "test_secret";
        let headers = fresh_timestamp_headers();

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = encode_hex(&mac.finalize().into_bytes());

        assert!(validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &signature,
            &headers,
            300,
        ));

        // With prefix
        let sig_with_prefix = format!("sha256={}", signature);
        assert!(validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &sig_with_prefix,
            &headers,
            300,
        ));

        // Replay window disabled (0) — header presence no longer matters
        let empty_headers = HeaderMap::new();
        assert!(validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &signature,
            &empty_headers,
            0,
        ));
    }

    #[test]
    fn test_validate_signature_invalid() {
        let headers = fresh_timestamp_headers();

        assert!(!validate_signature(
            SignatureAlgorithm::HmacSha256,
            b"test",
            "secret",
            "invalid_hex",
            &headers,
            300,
        ));

        assert!(!validate_signature(
            SignatureAlgorithm::HmacSha256,
            b"test",
            "secret",
            "0000000000000000000000000000000000000000000000000000000000000000",
            &headers,
            300,
        ));
    }

    #[test]
    fn test_replay_window_rejects_when_header_missing() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"test payload";
        let secret = "test_secret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = encode_hex(&mac.finalize().into_bytes());

        let headers = HeaderMap::new();
        assert!(!validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &signature,
            &headers,
            300,
        ));
    }

    #[test]
    fn test_replay_window_rejects_when_header_malformed() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"test payload";
        let secret = "test_secret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = encode_hex(&mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(REPLAY_TIMESTAMP_HEADER, "not-a-timestamp".parse().unwrap());
        assert!(!validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &signature,
            &headers,
            300,
        ));
    }

    #[test]
    fn test_replay_window_rejects_stale_timestamp() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"test payload";
        let secret = "test_secret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = encode_hex(&mac.finalize().into_bytes());

        let stale = (chrono::Utc::now().timestamp() - 600).to_string();
        let mut headers = HeaderMap::new();
        headers.insert(REPLAY_TIMESTAMP_HEADER, stale.parse().unwrap());
        assert!(!validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &signature,
            &headers,
            300,
        ));
    }

    #[test]
    fn test_replay_window_rejects_future_timestamp() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"test payload";
        let secret = "test_secret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = encode_hex(&mac.finalize().into_bytes());

        let future = (chrono::Utc::now().timestamp() + 3600).to_string();
        let mut headers = HeaderMap::new();
        headers.insert(REPLAY_TIMESTAMP_HEADER, future.parse().unwrap());
        assert!(!validate_signature(
            SignatureAlgorithm::HmacSha256,
            body,
            secret,
            &signature,
            &headers,
            300,
        ));
    }

    #[test]
    fn test_replay_window_does_not_apply_to_stripe() {
        // Stripe carries its own timestamp inside the header and ignores
        // x-webhook-timestamp, so the window does not gate the dispatch.
        // This test exercises that the dispatch reaches the Stripe validator
        // regardless of the auxiliary header state.
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"{\"type\":\"event\"}";
        let secret = "whsec_x";
        let ts = chrono::Utc::now().timestamp().to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        let mut signed = Vec::new();
        signed.extend_from_slice(ts.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body);
        mac.update(&signed);
        let sig = encode_hex(&mac.finalize().into_bytes());
        let header = format!("t={ts},v1={sig}");

        // No x-webhook-timestamp at all — Stripe still validates
        let empty_headers = HeaderMap::new();
        assert!(validate_signature(
            SignatureAlgorithm::StripeWebhooks,
            body,
            secret,
            &header,
            &empty_headers,
            300,
        ));
    }

    #[test]
    fn test_validate_stripe_webhooks() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"{\"type\":\"payment_intent.succeeded\"}";
        let secret = "whsec_test_stripe_secret";
        let timestamp = chrono::Utc::now().timestamp().to_string();

        let mut signed = Vec::new();
        signed.extend_from_slice(timestamp.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body);

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(&signed);
        let sig_hex = encode_hex(&mac.finalize().into_bytes());

        let header = format!("t={timestamp},v1={sig_hex}");
        assert!(validate_stripe_webhooks(body, secret, &header));

        // Multiple signatures (Stripe can include both v1 and a legacy v0)
        let header_multi = format!("t={timestamp},v0=ignored,v1={sig_hex}");
        assert!(validate_stripe_webhooks(body, secret, &header_multi));

        // Wrong signature
        assert!(!validate_stripe_webhooks(
            body,
            secret,
            &format!("t={timestamp},v1=deadbeef")
        ));

        // Missing timestamp
        assert!(!validate_stripe_webhooks(
            body,
            secret,
            &format!("v1={sig_hex}")
        ));

        // Stale timestamp (replay attack)
        let old_ts = (chrono::Utc::now().timestamp() - 600).to_string();
        let mut mac2 = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        let mut signed2 = Vec::new();
        signed2.extend_from_slice(old_ts.as_bytes());
        signed2.push(b'.');
        signed2.extend_from_slice(body);
        mac2.update(&signed2);
        let old_sig = encode_hex(&mac2.finalize().into_bytes());
        assert!(!validate_stripe_webhooks(
            body,
            secret,
            &format!("t={old_ts},v1={old_sig}")
        ));
    }

    #[test]
    fn test_validate_hmac_sha256_base64() {
        use base64::{Engine as _, engine::general_purpose};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let body = b"{\"topic\":\"orders/create\"}";
        let secret = "shopify_secret";

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig_b64 = general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(validate_hmac_sha256_base64(body, secret, &sig_b64));

        // Hex-encoded (wrong format) should fail
        let sig_hex = encode_hex(&{
            let mut mac2 = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac2.update(body);
            mac2.finalize().into_bytes().to_vec()
        });
        assert!(!validate_hmac_sha256_base64(body, secret, &sig_hex));
    }

    #[test]
    fn test_validate_ed25519() {
        use base64::{Engine as _, engine::general_purpose};
        use ring::signature::{Ed25519KeyPair, KeyPair};

        let body = b"{\"event\":\"user.created\"}";
        let seed = [42u8; 32];
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("valid seed");
        let public_key_b64 = general_purpose::STANDARD.encode(key_pair.public_key().as_ref());
        let sig = key_pair.sign(body);
        let signature_b64 = general_purpose::STANDARD.encode(sig.as_ref());

        assert!(validate_ed25519(body, &public_key_b64, &signature_b64));

        // Wrong body
        assert!(!validate_ed25519(
            b"tampered",
            &public_key_b64,
            &signature_b64
        ));

        // Garbage signature
        assert!(!validate_ed25519(body, &public_key_b64, "notbase64!!"));

        // Wrong public key
        let other_seed = [99u8; 32];
        let other_pair = Ed25519KeyPair::from_seed_unchecked(&other_seed).expect("valid seed");
        let other_pub_b64 = general_purpose::STANDARD.encode(other_pair.public_key().as_ref());
        assert!(!validate_ed25519(body, &other_pub_b64, &signature_b64));
    }
}

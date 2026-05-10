use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::function::{FunctionInfo, FunctionKind};
use crate::metadata::HandlerMetadata;

use super::context::WebhookContext;
use super::signature::{IdempotencyConfig, SignatureConfig};

/// Trait for FORGE webhook handlers.
///
/// Webhooks are HTTP endpoints that receive external events (e.g., from Stripe, GitHub).
/// They support signature validation, idempotency, and bypass authentication.
pub trait ForgeWebhook: crate::__sealed::Sealed + Send + Sync + 'static {
    /// Deserialized payload type. Use `serde_json::Value` for raw access.
    type Payload: serde::de::DeserializeOwned + Send + Sync + 'static;

    /// Get webhook metadata.
    fn info() -> WebhookInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    /// Execute the webhook handler.
    ///
    /// # Arguments
    /// * `ctx` - Webhook context with db, http, and dispatch capabilities
    /// * `payload` - The deserialized request body
    fn execute(
        ctx: &WebhookContext,
        payload: Self::Payload,
    ) -> Pin<Box<dyn Future<Output = Result<WebhookResult>> + Send + '_>>;
}

/// Webhook metadata.
///
/// Constructed by the `#[webhook]` macro. Adding a field is a breaking change
/// for hand-written `ForgeWebhook` impls; stage extensions through a builder
/// or major bump.
#[derive(Debug, Clone)]
pub struct WebhookInfo {
    /// Webhook name (used for identification).
    pub name: &'static str,
    /// Human-readable description of the webhook's purpose.
    pub description: Option<&'static str>,
    /// URL path for the webhook (e.g., "/webhooks/stripe").
    pub path: &'static str,
    /// Signature validation configuration.
    pub signature: Option<SignatureConfig>,
    /// Allow unsigned requests for this webhook.
    ///
    /// Defaults to `false` for security. Only enable for trusted internal callers.
    pub allow_unsigned: bool,
    /// Idempotency configuration.
    pub idempotency: Option<IdempotencyConfig>,
    /// Request timeout.
    pub timeout: Duration,
    /// Default timeout for outbound HTTP requests made by the webhook.
    pub http_timeout: Option<Duration>,
}

impl Default for WebhookInfo {
    fn default() -> Self {
        Self {
            name: "",
            description: None,
            path: "",
            signature: None,
            allow_unsigned: false,
            idempotency: None,
            timeout: Duration::from_secs(30),
            http_timeout: None,
        }
    }
}

impl From<&WebhookInfo> for FunctionInfo {
    /// Convert webhook metadata to a `FunctionInfo` for registration in the
    /// `FunctionRegistry`. Webhooks are always public (they bypass JWT auth
    /// and rely on signature validation instead). Fields that are not
    /// applicable to webhooks are set to their zero/false defaults.
    fn from(webhook: &WebhookInfo) -> Self {
        Self {
            name: webhook.name,
            description: webhook.description,
            kind: FunctionKind::Webhook,
            required_role: None,
            is_public: true,
            cache_ttl: None,
            timeout: Some(webhook.timeout),
            http_timeout: webhook.http_timeout,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: &[],
            selected_columns: &[],
            changed_columns: &[],
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
        }
    }
}

/// Result returned by webhook handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
#[non_exhaustive]
pub enum WebhookResult {
    /// Request processed successfully (HTTP 200).
    #[serde(rename = "ok")]
    Ok,
    /// Request accepted for async processing (HTTP 202).
    #[serde(rename = "accepted")]
    Accepted,
    /// Custom response with specific status and body.
    #[serde(rename = "custom")]
    Custom {
        /// HTTP status code.
        status_code: u16,
        /// Response body.
        body: Value,
    },
}

impl WebhookResult {
    /// Get the HTTP status code for this result.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::Accepted => 202,
            Self::Custom { status_code, .. } => *status_code,
        }
    }

    /// Get the response body.
    pub fn body(&self) -> Value {
        match self {
            Self::Ok => serde_json::json!({"status": "ok"}),
            Self::Accepted => serde_json::json!({"status": "accepted"}),
            Self::Custom { body, .. } => body.clone(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_default_webhook_info() {
        let info = WebhookInfo::default();
        assert!(info.signature.is_none());
        assert!(!info.allow_unsigned);
        assert!(info.idempotency.is_none());
        assert_eq!(info.timeout, Duration::from_secs(30));
        assert_eq!(info.http_timeout, None);
    }

    #[test]
    fn test_webhook_result_status_codes() {
        assert_eq!(WebhookResult::Ok.status_code(), 200);
        assert_eq!(WebhookResult::Accepted.status_code(), 202);
        assert_eq!(
            WebhookResult::Custom {
                status_code: 400,
                body: serde_json::json!({"error": "bad request"})
            }
            .status_code(),
            400
        );
    }

    #[test]
    fn test_webhook_result_body() {
        assert_eq!(
            WebhookResult::Ok.body(),
            serde_json::json!({"status": "ok"})
        );
        assert_eq!(
            WebhookResult::Accepted.body(),
            serde_json::json!({"status": "accepted"})
        );
    }
}

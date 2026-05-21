use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::function::{FunctionInfo, FunctionKind};

use super::context::WebhookContext;
use super::signature::{IdempotencyConfig, SignatureConfig};

/// Trait for inbound webhook handlers.
pub trait ForgeWebhook: crate::__sealed::Sealed + Send + Sync + 'static {
    type Payload: serde::de::DeserializeOwned + Send + Sync + 'static;

    fn info() -> WebhookInfo;

    fn execute(
        ctx: &WebhookContext,
        payload: Self::Payload,
    ) -> Pin<Box<dyn Future<Output = Result<WebhookResult>> + Send + '_>>;
}

/// Metadata for a registered webhook handler.
#[derive(Debug, Clone)]
pub struct WebhookInfo {
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub path: &'static str,
    pub signature: Option<SignatureConfig>,
    pub allow_unsigned: bool,
    pub idempotency: Option<IdempotencyConfig>,
    pub timeout: Duration,
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
            requires_tenant_scope: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
#[non_exhaustive]
pub enum WebhookResult {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "accepted")]
    Accepted,
    #[serde(rename = "custom")]
    Custom { status_code: u16, body: Value },
}

impl WebhookResult {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::Accepted => 202,
            Self::Custom { status_code, .. } => *status_code,
        }
    }

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

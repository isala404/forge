use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::JobDispatch;

/// Context available to webhook handlers.
pub struct WebhookContext {
    /// Webhook name.
    pub webhook_name: String,
    /// Unique request ID for this webhook invocation.
    pub request_id: String,
    /// Idempotency key if extracted from request.
    pub idempotency_key: Option<String>,
    /// Request headers (lowercase keys).
    headers: HashMap<String, String>,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client for external calls.
    http_client: reqwest::Client,
    /// Job dispatcher for async processing.
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
}

impl WebhookContext {
    /// Create a new webhook context.
    pub fn new(
        webhook_name: String,
        request_id: String,
        headers: HashMap<String, String>,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            webhook_name,
            request_id,
            idempotency_key: None,
            headers,
            db_pool,
            http_client,
            job_dispatch: None,
            env_provider: Arc::new(RealEnvProvider::new()),
        }
    }

    /// Set idempotency key.
    pub fn with_idempotency_key(mut self, key: Option<String>) -> Self {
        self.idempotency_key = key;
        self
    }

    /// Set job dispatcher.
    pub fn with_job_dispatch(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        self.job_dispatch = Some(dispatcher);
        self
    }

    /// Set environment provider.
    pub fn with_env_provider(mut self, provider: Arc<dyn EnvProvider>) -> Self {
        self.env_provider = provider;
        self
    }

    /// Get database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get a request header value.
    ///
    /// Header names are case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(|s| s.as_str())
    }

    /// Get all headers.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.headers
    }

    /// Dispatch a background job for async processing.
    ///
    /// This is the recommended way to handle webhook events:
    /// 1. Validate the webhook signature
    /// 2. Dispatch a job to process the event
    /// 3. Return 202 Accepted immediately
    ///
    /// # Arguments
    /// * `job_type` - The registered name of the job type
    /// * `args` - The arguments for the job (will be serialized to JSON)
    ///
    /// # Returns
    /// The UUID of the dispatched job, or an error if dispatch is not available.
    pub async fn dispatch_job<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> crate::error::Result<Uuid> {
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".into())
        })?;
        let args_json = serde_json::to_value(args)?;
        dispatcher.dispatch_by_name(job_type, args_json).await
    }

    /// Request cancellation for a job.
    pub async fn cancel_job(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> crate::error::Result<bool> {
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".into())
        })?;
        dispatcher.cancel(job_id, reason).await
    }
}

impl EnvAccess for WebhookContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_webhook_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let mut headers = HashMap::new();
        headers.insert("x-github-event".to_string(), "push".to_string());
        headers.insert("x-github-delivery".to_string(), "abc-123".to_string());

        let ctx = WebhookContext::new(
            "github_webhook".to_string(),
            "req-123".to_string(),
            headers,
            pool,
            reqwest::Client::new(),
        )
        .with_idempotency_key(Some("abc-123".to_string()));

        assert_eq!(ctx.webhook_name, "github_webhook");
        assert_eq!(ctx.request_id, "req-123");
        assert_eq!(ctx.idempotency_key, Some("abc-123".to_string()));
        assert_eq!(ctx.header("X-GitHub-Event"), Some("push"));
        assert_eq!(ctx.header("x-github-event"), Some("push")); // case-insensitive
        assert!(ctx.header("nonexistent").is_none());
    }
}

//! Test context for webhook functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_dispatch::MockJobDispatch;
use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};

/// Test context for webhook functions.
///
/// Provides an isolated testing environment for webhooks with configurable
/// headers, idempotency key, and mock job dispatch.
///
/// # Example
///
/// ```ignore
/// let ctx = TestWebhookContext::builder("github_webhook")
///     .with_header("X-GitHub-Event", "push")
///     .with_idempotency_key("delivery-123")
///     .build();
///
/// // Dispatch a job
/// ctx.dispatch_job("process_event", payload).await?;
///
/// // Verify job was dispatched
/// ctx.job_dispatch().assert_dispatched("process_event");
/// ```
pub struct TestWebhookContext {
    /// Webhook name.
    pub webhook_name: String,
    /// Request ID.
    pub request_id: String,
    /// Idempotency key (if extracted from request).
    pub idempotency_key: Option<String>,
    /// Request headers (lowercase keys).
    headers: HashMap<String, String>,
    /// Optional database pool.
    pool: Option<PgPool>,
    /// Mock HTTP client.
    http: Arc<MockHttp>,
    /// Mock job dispatch for verification.
    job_dispatch: Arc<MockJobDispatch>,
    /// Mock environment provider.
    env_provider: Arc<MockEnvProvider>,
}

impl TestWebhookContext {
    /// Create a new builder.
    pub fn builder(webhook_name: impl Into<String>) -> TestWebhookContextBuilder {
        TestWebhookContextBuilder::new(webhook_name)
    }

    /// Get the database pool (if available).
    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Get the mock HTTP client.
    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    /// Get a request header value (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(|s| s.as_str())
    }

    /// Get all headers.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.headers
    }

    /// Get the mock job dispatch for verification.
    pub fn job_dispatch(&self) -> &MockJobDispatch {
        &self.job_dispatch
    }

    /// Dispatch a job (records for later verification).
    pub async fn dispatch_job<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        self.job_dispatch.dispatch(job_type, args).await
    }

    /// Get the mock env provider for verification.
    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }
}

impl EnvAccess for TestWebhookContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Builder for TestWebhookContext.
pub struct TestWebhookContextBuilder {
    webhook_name: String,
    request_id: Option<String>,
    idempotency_key: Option<String>,
    headers: HashMap<String, String>,
    pool: Option<PgPool>,
    http: MockHttp,
    job_dispatch: Arc<MockJobDispatch>,
    env_vars: HashMap<String, String>,
}

impl TestWebhookContextBuilder {
    /// Create a new builder with webhook name.
    pub fn new(webhook_name: impl Into<String>) -> Self {
        Self {
            webhook_name: webhook_name.into(),
            request_id: None,
            idempotency_key: None,
            headers: HashMap::new(),
            pool: None,
            http: MockHttp::new(),
            job_dispatch: Arc::new(MockJobDispatch::new()),
            env_vars: HashMap::new(),
        }
    }

    /// Set a specific request ID.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Set the idempotency key.
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Add a request header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .insert(name.into().to_lowercase(), value.into());
        self
    }

    /// Add multiple headers.
    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        for (k, v) in headers {
            self.headers.insert(k.to_lowercase(), v);
        }
        self
    }

    /// Set the database pool.
    pub fn with_pool(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Add an HTTP mock with a custom handler.
    pub fn mock_http<F>(self, pattern: &str, handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.http.add_mock_sync(pattern, handler);
        self
    }

    /// Add an HTTP mock that returns a JSON response.
    pub fn mock_http_json<T: serde::Serialize>(self, pattern: &str, response: T) -> Self {
        let json = serde_json::to_value(response).unwrap_or(serde_json::Value::Null);
        self.mock_http(pattern, move |_| MockResponse::json(json.clone()))
    }

    /// Use a specific mock job dispatch.
    pub fn with_job_dispatch(mut self, dispatch: Arc<MockJobDispatch>) -> Self {
        self.job_dispatch = dispatch;
        self
    }

    /// Set a single environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Set multiple environment variables.
    pub fn with_envs(mut self, vars: HashMap<String, String>) -> Self {
        self.env_vars.extend(vars);
        self
    }

    /// Build the test context.
    pub fn build(self) -> TestWebhookContext {
        TestWebhookContext {
            webhook_name: self.webhook_name,
            request_id: self
                .request_id
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            idempotency_key: self.idempotency_key,
            headers: self.headers,
            pool: self.pool,
            http: Arc::new(self.http),
            job_dispatch: self.job_dispatch,
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_context_creation() {
        let ctx = TestWebhookContext::builder("github_webhook")
            .with_header("X-GitHub-Event", "push")
            .with_idempotency_key("delivery-123")
            .build();

        assert_eq!(ctx.webhook_name, "github_webhook");
        assert_eq!(ctx.idempotency_key, Some("delivery-123".to_string()));
        assert_eq!(ctx.header("X-GitHub-Event"), Some("push"));
        assert_eq!(ctx.header("x-github-event"), Some("push")); // case-insensitive
    }

    #[tokio::test]
    async fn test_dispatch_job() {
        let ctx = TestWebhookContext::builder("test").build();

        let job_id = ctx
            .dispatch_job("process_event", serde_json::json!({"action": "push"}))
            .await
            .unwrap();

        assert!(!job_id.is_nil());
        ctx.job_dispatch().assert_dispatched("process_event");
    }

    #[test]
    fn test_headers_case_insensitive() {
        let ctx = TestWebhookContext::builder("test")
            .with_header("Content-Type", "application/json")
            .build();

        assert_eq!(ctx.header("content-type"), Some("application/json"));
        assert_eq!(ctx.header("CONTENT-TYPE"), Some("application/json"));
    }

    #[test]
    fn default_builder_auto_generates_request_id_and_no_idempotency_key() {
        let ctx = TestWebhookContext::builder("anon").build();
        // request_id must be a parseable UUID when not overridden.
        assert!(Uuid::parse_str(&ctx.request_id).is_ok());
        assert!(ctx.idempotency_key.is_none());
        assert!(ctx.headers().is_empty());
        assert!(ctx.db().is_none());
    }

    #[test]
    fn with_request_id_overrides_generated_value() {
        let ctx = TestWebhookContext::builder("a")
            .with_request_id("req-42")
            .build();
        assert_eq!(ctx.request_id, "req-42");
    }

    #[test]
    fn with_headers_bulk_lowercases_all_keys() {
        let mut input = HashMap::new();
        input.insert("X-One".to_string(), "1".to_string());
        input.insert("X-TWO".to_string(), "2".to_string());
        let ctx = TestWebhookContext::builder("a").with_headers(input).build();
        assert_eq!(ctx.header("x-one"), Some("1"));
        assert_eq!(ctx.header("x-two"), Some("2"));
        // Stored map should only contain lowercased keys.
        for k in ctx.headers().keys() {
            assert_eq!(k, &k.to_lowercase(), "header key not lowercased: {k}");
        }
    }

    #[test]
    fn header_returns_none_for_missing() {
        let ctx = TestWebhookContext::builder("a").build();
        assert!(ctx.header("absent").is_none());
    }

    #[tokio::test]
    async fn dispatch_job_records_each_call_in_order() {
        let ctx = TestWebhookContext::builder("h").build();
        ctx.dispatch_job("a", serde_json::json!({"n": 1}))
            .await
            .unwrap();
        ctx.dispatch_job("a", serde_json::json!({"n": 2}))
            .await
            .unwrap();
        ctx.dispatch_job("b", serde_json::json!({})).await.unwrap();
        ctx.job_dispatch().assert_dispatch_count("a", 2);
        ctx.job_dispatch().assert_dispatch_count("b", 1);
        ctx.job_dispatch().assert_not_dispatched("never");
    }

    #[tokio::test]
    async fn with_job_dispatch_shares_state_across_clones() {
        let shared = Arc::new(MockJobDispatch::new());
        let ctx = TestWebhookContext::builder("h")
            .with_job_dispatch(shared.clone())
            .build();
        ctx.dispatch_job("shared", serde_json::json!({}))
            .await
            .unwrap();
        // Caller's handle sees the dispatch even though the call went through ctx.
        shared.assert_dispatched("shared");
    }

    #[test]
    fn with_env_and_with_envs_compose() {
        let mut bulk = HashMap::new();
        bulk.insert("B1".to_string(), "vb1".to_string());
        let ctx = TestWebhookContext::builder("h")
            .with_env("A", "va")
            .with_envs(bulk)
            .build();
        // EnvAccess wires the MockEnvProvider correctly.
        assert_eq!(ctx.env("A"), Some("va".to_string()));
        assert_eq!(ctx.env("B1"), Some("vb1".to_string()));
        assert!(ctx.env("UNSET").is_none());
        // The mock records accesses, so we can verify reads landed on the same provider.
        assert!(ctx.env_mock().was_accessed("A"));
    }

    #[tokio::test]
    async fn mock_http_json_returns_serialized_body_when_executed() {
        let ctx = TestWebhookContext::builder("h")
            .mock_http_json("https://example.test/echo", serde_json::json!({"ok": true}))
            .build();

        let req = MockRequest {
            method: "GET".to_string(),
            path: "/echo".to_string(),
            url: "https://example.test/echo".to_string(),
            headers: HashMap::new(),
            body: serde_json::Value::Null,
        };
        let resp = ctx.http().execute(req).await;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, serde_json::json!({"ok": true}));
        ctx.http().assert_called("https://example.test/echo");
    }
}

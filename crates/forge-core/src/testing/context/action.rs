//! Test context for action functions.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_dispatch::{MockJobDispatch, MockWorkflowDispatch};
use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use crate::function::{AuthContext, RequestMetadata};
use crate::Result;

/// Test context for action functions.
///
/// Provides an isolated testing environment for actions with HTTP mocking,
/// configurable authentication, and optional database access.
///
/// # Example
///
/// ```ignore
/// let ctx = TestActionContext::builder()
///     .as_user(Uuid::new_v4())
///     .mock_http_json("https://api.example.com/*", json!({"status": "ok"}))
///     .build();
///
/// // Make HTTP call (will return mocked response)
/// let response = ctx.http().execute(...).await;
///
/// // Verify HTTP call was made
/// ctx.http().assert_called("https://api.example.com/*");
/// ```
pub struct TestActionContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Optional database pool.
    pool: Option<PgPool>,
    /// Mock HTTP client.
    http: Arc<MockHttp>,
    /// Mock job dispatch.
    job_dispatch: Arc<MockJobDispatch>,
    /// Mock workflow dispatch.
    workflow_dispatch: Arc<MockWorkflowDispatch>,
}

impl TestActionContext {
    /// Create a new builder.
    pub fn builder() -> TestActionContextBuilder {
        TestActionContextBuilder::default()
    }

    /// Create a minimal unauthenticated context.
    pub fn minimal() -> Self {
        Self::builder().build()
    }

    /// Create an authenticated context.
    pub fn authenticated(user_id: Uuid) -> Self {
        Self::builder().as_user(user_id).build()
    }

    /// Get the database pool (if available).
    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Get the mock HTTP client.
    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    /// Get the mock job dispatch for verification.
    pub fn job_dispatch(&self) -> &MockJobDispatch {
        &self.job_dispatch
    }

    /// Get the mock workflow dispatch for verification.
    pub fn workflow_dispatch(&self) -> &MockWorkflowDispatch {
        &self.workflow_dispatch
    }

    /// Get the authenticated user ID or return an error.
    pub fn require_user_id(&self) -> Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Dispatch a job (records for later verification).
    pub async fn dispatch_job<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        self.job_dispatch.dispatch(job_type, args).await
    }

    /// Start a workflow (records for later verification).
    pub async fn start_workflow<T: serde::Serialize>(&self, name: &str, input: T) -> Result<Uuid> {
        self.workflow_dispatch.start(name, input).await
    }
}

/// Builder for TestActionContext.
pub struct TestActionContextBuilder {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
    http: MockHttp,
    job_dispatch: Arc<MockJobDispatch>,
    workflow_dispatch: Arc<MockWorkflowDispatch>,
}

impl Default for TestActionContextBuilder {
    fn default() -> Self {
        Self {
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            pool: None,
            http: MockHttp::new(),
            job_dispatch: Arc::new(MockJobDispatch::new()),
            workflow_dispatch: Arc::new(MockWorkflowDispatch::new()),
        }
    }
}

impl TestActionContextBuilder {
    /// Set the authenticated user.
    pub fn as_user(mut self, id: Uuid) -> Self {
        self.user_id = Some(id);
        self
    }

    /// Add a role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Add multiple roles.
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles.extend(roles);
        self
    }

    /// Add a custom claim.
    pub fn with_claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(key.into(), value);
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

    /// Build the test context.
    pub fn build(self) -> TestActionContext {
        let auth = if let Some(user_id) = self.user_id {
            AuthContext::authenticated(user_id, self.roles, self.claims)
        } else {
            AuthContext::unauthenticated()
        };

        TestActionContext {
            auth,
            request: RequestMetadata::default(),
            pool: self.pool,
            http: Arc::new(self.http),
            job_dispatch: self.job_dispatch,
            workflow_dispatch: self.workflow_dispatch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_context() {
        let ctx = TestActionContext::minimal();
        assert!(!ctx.auth.is_authenticated());
    }

    #[test]
    fn test_authenticated_context() {
        let user_id = Uuid::new_v4();
        let ctx = TestActionContext::authenticated(user_id);
        assert!(ctx.auth.is_authenticated());
        assert_eq!(ctx.require_user_id().unwrap(), user_id);
    }
}

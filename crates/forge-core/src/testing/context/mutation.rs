//! Test context for mutation functions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_dispatch::{MockJobDispatch, MockWorkflowDispatch};
use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use super::build_test_auth;
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::{AuthContext, OutboxBuffer, PendingJob, PendingWorkflow, RequestMetadata};

/// Test context for mutation functions.
///
/// Provides an isolated testing environment for mutations with configurable
/// authentication, optional database access, and mock job/workflow dispatch.
///
/// # Example
///
/// ```ignore
/// let ctx = TestMutationContext::builder()
///     .as_user(Uuid::new_v4())
///     .build();
///
/// // Dispatch a job
/// ctx.dispatch_job("send_email", json!({"to": "test@example.com"})).await?;
///
/// // Verify job was dispatched
/// ctx.job_dispatch().assert_dispatched("send_email");
/// ```
pub struct TestMutationContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Optional database pool.
    pool: Option<PgPool>,
    /// Mock HTTP client.
    http: Arc<MockHttp>,
    /// Mock job dispatch for verification.
    job_dispatch: Arc<MockJobDispatch>,
    /// Mock workflow dispatch for verification.
    workflow_dispatch: Arc<MockWorkflowDispatch>,
    /// Mock environment provider.
    env_provider: Arc<MockEnvProvider>,
    /// Outbox buffer for transactional mode simulation.
    outbox: Arc<Mutex<OutboxBuffer>>,
}

impl TestMutationContext {
    /// Create a new builder.
    pub fn builder() -> TestMutationContextBuilder {
        TestMutationContextBuilder::default()
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

    /// Like `require_user_id()` but for non-UUID auth providers.
    pub fn require_subject(&self) -> Result<&str> {
        self.auth.require_subject()
    }

    /// Dispatch a job (records for later verification).
    pub async fn dispatch_job<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        self.job_dispatch.dispatch(job_type, args).await
    }

    /// Cancel a job (records for later verification).
    pub async fn cancel_job(&self, job_id: Uuid, reason: Option<String>) -> Result<bool> {
        self.job_dispatch.cancel_job(job_id, reason);
        Ok(true)
    }

    /// Start a workflow (records for later verification).
    pub async fn start_workflow<T: serde::Serialize>(&self, name: &str, input: T) -> Result<Uuid> {
        self.workflow_dispatch.start(name, input).await
    }

    /// Get the mock env provider for verification.
    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }

    /// Get pending jobs from the outbox buffer.
    pub fn pending_jobs(&self) -> Vec<PendingJob> {
        self.outbox.lock().unwrap().jobs.clone()
    }

    /// Get pending workflows from the outbox buffer.
    pub fn pending_workflows(&self) -> Vec<PendingWorkflow> {
        self.outbox.lock().unwrap().workflows.clone()
    }

    /// Assert that a job was buffered in the outbox.
    pub fn assert_job_buffered(&self, job_type: &str) {
        let jobs = self.pending_jobs();
        assert!(
            jobs.iter().any(|j| j.job_type == job_type),
            "Expected job '{}' to be buffered, but it was not. Buffered jobs: {:?}",
            job_type,
            jobs.iter().map(|j| &j.job_type).collect::<Vec<_>>()
        );
    }

    /// Assert that a workflow was buffered in the outbox.
    pub fn assert_workflow_buffered(&self, workflow_name: &str) {
        let workflows = self.pending_workflows();
        assert!(
            workflows.iter().any(|w| w.workflow_name == workflow_name),
            "Expected workflow '{}' to be buffered, but it was not. Buffered workflows: {:?}",
            workflow_name,
            workflows.iter().map(|w| &w.workflow_name).collect::<Vec<_>>()
        );
    }
}

impl EnvAccess for TestMutationContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Builder for TestMutationContext.
pub struct TestMutationContextBuilder {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
    http: MockHttp,
    job_dispatch: Arc<MockJobDispatch>,
    workflow_dispatch: Arc<MockWorkflowDispatch>,
    env_vars: HashMap<String, String>,
}

impl Default for TestMutationContextBuilder {
    fn default() -> Self {
        Self {
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            pool: None,
            http: MockHttp::new(),
            job_dispatch: Arc::new(MockJobDispatch::new()),
            workflow_dispatch: Arc::new(MockWorkflowDispatch::new()),
            env_vars: HashMap::new(),
        }
    }
}

impl TestMutationContextBuilder {
    /// Set the authenticated user with a UUID.
    pub fn as_user(mut self, id: Uuid) -> Self {
        self.user_id = Some(id);
        self
    }

    /// For non-UUID auth providers (Firebase, Clerk, etc.).
    pub fn as_subject(mut self, subject: impl Into<String>) -> Self {
        self.claims
            .insert("sub".to_string(), serde_json::json!(subject.into()));
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

    /// Use a specific mock job dispatch.
    pub fn with_job_dispatch(mut self, dispatch: Arc<MockJobDispatch>) -> Self {
        self.job_dispatch = dispatch;
        self
    }

    /// Use a specific mock workflow dispatch.
    pub fn with_workflow_dispatch(mut self, dispatch: Arc<MockWorkflowDispatch>) -> Self {
        self.workflow_dispatch = dispatch;
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
    pub fn build(self) -> TestMutationContext {
        TestMutationContext {
            auth: build_test_auth(self.user_id, self.roles, self.claims),
            request: RequestMetadata::default(),
            pool: self.pool,
            http: Arc::new(self.http),
            job_dispatch: self.job_dispatch,
            workflow_dispatch: self.workflow_dispatch,
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
            outbox: Arc::new(Mutex::new(OutboxBuffer::default())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatch_job() {
        let ctx = TestMutationContext::authenticated(Uuid::new_v4());

        let job_id = ctx
            .dispatch_job("send_email", serde_json::json!({"to": "test@example.com"}))
            .await
            .unwrap();

        assert!(!job_id.is_nil());
        ctx.job_dispatch().assert_dispatched("send_email");
    }

    #[tokio::test]
    async fn test_start_workflow() {
        let ctx = TestMutationContext::authenticated(Uuid::new_v4());

        let run_id = ctx
            .start_workflow("onboarding", serde_json::json!({"user_id": "123"}))
            .await
            .unwrap();

        assert!(!run_id.is_nil());
        ctx.workflow_dispatch().assert_started("onboarding");
    }

    #[tokio::test]
    async fn test_cancel_job() {
        let ctx = TestMutationContext::authenticated(Uuid::new_v4());

        let job_id = ctx
            .dispatch_job("send_email", serde_json::json!({"to": "test@example.com"}))
            .await
            .unwrap();

        let cancelled = ctx
            .cancel_job(job_id, Some("test cancel".to_string()))
            .await
            .unwrap();

        assert!(cancelled);
        let jobs = ctx.job_dispatch().dispatched_jobs();
        assert_eq!(jobs[0].status, crate::job::JobStatus::Cancelled);
        assert_eq!(jobs[0].cancel_reason.as_deref(), Some("test cancel"));
    }

    #[tokio::test]
    async fn test_job_not_dispatched() {
        let ctx = TestMutationContext::authenticated(Uuid::new_v4());

        ctx.job_dispatch().assert_not_dispatched("send_email");
    }
}

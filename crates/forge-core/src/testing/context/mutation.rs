//! Test context for mutation functions.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_dispatch::{MockJobDispatch, MockWorkflowDispatch};
use crate::function::{AuthContext, RequestMetadata};
use crate::Result;

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
    /// Mock job dispatch for verification.
    job_dispatch: Arc<MockJobDispatch>,
    /// Mock workflow dispatch for verification.
    workflow_dispatch: Arc<MockWorkflowDispatch>,
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

/// Builder for TestMutationContext.
pub struct TestMutationContextBuilder {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
    job_dispatch: Arc<MockJobDispatch>,
    workflow_dispatch: Arc<MockWorkflowDispatch>,
}

impl Default for TestMutationContextBuilder {
    fn default() -> Self {
        Self {
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            pool: None,
            job_dispatch: Arc::new(MockJobDispatch::new()),
            workflow_dispatch: Arc::new(MockWorkflowDispatch::new()),
        }
    }
}

impl TestMutationContextBuilder {
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

    /// Build the test context.
    pub fn build(self) -> TestMutationContext {
        let auth = if let Some(user_id) = self.user_id {
            AuthContext::authenticated(user_id, self.roles, self.claims)
        } else {
            AuthContext::unauthenticated()
        };

        TestMutationContext {
            auth,
            request: RequestMetadata::default(),
            pool: self.pool,
            job_dispatch: self.job_dispatch,
            workflow_dispatch: self.workflow_dispatch,
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
    async fn test_job_not_dispatched() {
        let ctx = TestMutationContext::authenticated(Uuid::new_v4());

        ctx.job_dispatch().assert_not_dispatched("send_email");
    }
}

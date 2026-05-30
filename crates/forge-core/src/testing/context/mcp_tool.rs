//! Test context for MCP tool functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_dispatch::{MockJobDispatch, MockWorkflowDispatch};
use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use super::build_test_auth;
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::{AuthContext, RequestMetadata};

/// Test context for MCP tool functions.
pub struct TestMcpToolContext {
    pub auth: AuthContext,
    pub request: RequestMetadata,
    pool: Option<PgPool>,
    tenant_id: Option<Uuid>,
    http: Arc<MockHttp>,
    job_dispatch: Arc<MockJobDispatch>,
    workflow_dispatch: Arc<MockWorkflowDispatch>,
    env_provider: Arc<MockEnvProvider>,
}

impl TestMcpToolContext {
    pub fn builder() -> TestMcpToolContextBuilder {
        TestMcpToolContextBuilder::default()
    }

    pub fn minimal() -> Self {
        Self::builder().build()
    }

    pub fn authenticated(user_id: Uuid) -> Self {
        Self::builder().as_user(user_id).build()
    }

    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    /// Returns `Unauthorized` if not authenticated.
    pub fn user_id(&self) -> Result<Uuid> {
        self.auth.require_user_id()
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.auth.has_role(role)
    }

    pub fn claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.auth.claim(key)
    }

    pub fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id
    }

    pub fn job_dispatch(&self) -> &MockJobDispatch {
        &self.job_dispatch
    }

    pub fn workflow_dispatch(&self) -> &MockWorkflowDispatch {
        &self.workflow_dispatch
    }

    pub async fn dispatch_job<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        self.job_dispatch.dispatch(job_type, args).await
    }

    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> Result<Uuid> {
        self.workflow_dispatch.start(workflow_name, input).await
    }

    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }
}

impl EnvAccess for TestMcpToolContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[derive(Default)]
pub struct TestMcpToolContextBuilder {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    tenant_id: Option<Uuid>,
    pool: Option<PgPool>,
    http: MockHttp,
    job_dispatch: Option<Arc<MockJobDispatch>>,
    workflow_dispatch: Option<Arc<MockWorkflowDispatch>>,
    env_vars: HashMap<String, String>,
}

impl TestMcpToolContextBuilder {
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

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles.extend(roles);
        self
    }

    pub fn with_claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(key.into(), value);
        self
    }

    /// Set the tenant id for multi-tenant testing.
    ///
    /// Production code reads the tenant from `auth.claims["tenant_id"]`, so
    /// this writes the same value into the claims map. Tests calling
    /// `ctx.auth.tenant_id()` then behave identically to production.
    pub fn with_tenant(mut self, tenant_id: Uuid) -> Self {
        self.tenant_id = Some(tenant_id);
        self.claims.insert(
            "tenant_id".to_string(),
            serde_json::Value::String(tenant_id.to_string()),
        );
        self
    }

    pub fn with_pool(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn mock_http<F>(self, pattern: &str, handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.http.add_mock_sync(pattern, handler);
        self
    }

    pub fn mock_http_json<T: serde::Serialize>(self, pattern: &str, response: T) -> Self {
        let json = serde_json::to_value(response).unwrap_or(serde_json::Value::Null);
        self.mock_http(pattern, move |_| MockResponse::json(json.clone()))
    }

    pub fn with_job_dispatch(mut self, dispatch: Arc<MockJobDispatch>) -> Self {
        self.job_dispatch = Some(dispatch);
        self
    }

    pub fn with_workflow_dispatch(mut self, dispatch: Arc<MockWorkflowDispatch>) -> Self {
        self.workflow_dispatch = Some(dispatch);
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    pub fn with_envs(mut self, vars: HashMap<String, String>) -> Self {
        self.env_vars.extend(vars);
        self
    }

    pub fn build(self) -> TestMcpToolContext {
        TestMcpToolContext {
            auth: build_test_auth(self.user_id, self.roles, self.claims),
            request: RequestMetadata::default(),
            pool: self.pool,
            tenant_id: self.tenant_id,
            http: Arc::new(self.http),
            job_dispatch: self
                .job_dispatch
                .unwrap_or_else(|| Arc::new(MockJobDispatch::new())),
            workflow_dispatch: self
                .workflow_dispatch
                .unwrap_or_else(|| Arc::new(MockWorkflowDispatch::new())),
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_context() {
        let ctx = TestMcpToolContext::minimal();
        assert!(!ctx.auth.is_authenticated());
        assert!(ctx.db().is_none());
    }

    #[test]
    fn test_authenticated_context() {
        let user_id = Uuid::new_v4();
        let ctx = TestMcpToolContext::authenticated(user_id);
        assert!(ctx.auth.is_authenticated());
        assert_eq!(ctx.user_id().unwrap(), user_id);
    }

    #[test]
    fn test_context_with_roles() {
        let ctx = TestMcpToolContext::builder()
            .as_user(Uuid::new_v4())
            .with_role("admin")
            .with_role("user")
            .build();

        assert!(ctx.has_role("admin"));
        assert!(ctx.has_role("user"));
        assert!(!ctx.has_role("superuser"));
    }

    #[tokio::test]
    async fn test_dispatch_job() {
        let ctx = TestMcpToolContext::builder()
            .as_user(Uuid::new_v4())
            .build();

        let job_id = ctx
            .dispatch_job("process_event", serde_json::json!({"action": "push"}))
            .await
            .unwrap();

        assert!(!job_id.is_nil());
        ctx.job_dispatch().assert_dispatched("process_event");
    }

    #[tokio::test]
    async fn test_start_workflow() {
        let ctx = TestMcpToolContext::builder()
            .as_user(Uuid::new_v4())
            .build();

        let run_id = ctx
            .start_workflow("onboarding", serde_json::json!({"user_id": "u123"}))
            .await
            .unwrap();

        assert!(!run_id.is_nil());
        ctx.workflow_dispatch().assert_started("onboarding");
    }

    #[test]
    fn test_context_with_env() {
        let ctx = TestMcpToolContext::builder()
            .with_env("API_KEY", "test_key_123")
            .with_env("TIMEOUT", "30")
            .build();

        assert_eq!(ctx.env("API_KEY"), Some("test_key_123".to_string()));
        assert_eq!(ctx.env_or("TIMEOUT", "10"), "30");
        assert_eq!(ctx.env_or("MISSING", "default"), "default");

        ctx.env_mock().assert_accessed("API_KEY");
        ctx.env_mock().assert_accessed("TIMEOUT");
    }
}

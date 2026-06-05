//! Test context for mutation functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_dispatch::{MockJobDispatch, MockWorkflowDispatch};
use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use super::build_test_auth;
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::{
    AuthContext, JobDispatch, MutationContext, RequestMetadata, WorkflowDispatch,
};
use crate::http::CircuitBreakerClient;

/// Test context for mutation functions with mock dispatch and optional DB access.
pub struct TestMutationContext {
    pub auth: AuthContext,
    pub request: RequestMetadata,
    pool: Option<PgPool>,
    http: Arc<MockHttp>,
    job_dispatch: Arc<MockJobDispatch>,
    workflow_dispatch: Arc<MockWorkflowDispatch>,
    env_provider: Arc<MockEnvProvider>,
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

    /// Get the authenticated user's UUID. Returns 401 if not authenticated.
    pub fn user_id(&self) -> Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Dispatch a job (records for later verification).
    pub async fn dispatch_job<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        self.job_dispatch.dispatch(job_type, args).await
    }

    /// Dispatch a job at a specific time (records for later verification).
    pub async fn dispatch_job_at<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        scheduled_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        self.job_dispatch
            .dispatch_at(job_type, args, scheduled_at)
            .await
    }

    /// Dispatch a job after a delay (records for later verification).
    pub async fn dispatch_job_after<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        delay: Duration,
    ) -> Result<Uuid> {
        let scheduled_at = Utc::now()
            + chrono::Duration::from_std(delay)
                .map_err(|_| crate::error::ForgeError::InvalidArgument("delay too large".into()))?;
        self.job_dispatch
            .dispatch_at(job_type, args, scheduled_at)
            .await
    }

    /// Type-safe dispatch: resolves the job name from the type's `ForgeJob`
    /// impl and serializes the args at the call site.
    pub async fn dispatch<J: crate::ForgeJob>(&self, args: J::Args) -> Result<Uuid> {
        self.dispatch_job(J::info().name, args).await
    }

    /// Type-safe dispatch at a specific time.
    pub async fn dispatch_at<J: crate::ForgeJob>(
        &self,
        args: J::Args,
        scheduled_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        self.dispatch_job_at(J::info().name, args, scheduled_at)
            .await
    }

    /// Type-safe dispatch after a delay.
    pub async fn dispatch_after<J: crate::ForgeJob>(
        &self,
        args: J::Args,
        delay: Duration,
    ) -> Result<Uuid> {
        self.dispatch_job_after(J::info().name, args, delay).await
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

    /// Type-safe workflow start.
    pub async fn start<W: crate::ForgeWorkflow>(&self, input: W::Input) -> Result<Uuid> {
        self.start_workflow(W::info().name, input).await
    }

    /// Get the mock env provider for verification.
    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }

    /// Bridge to a real [`MutationContext`] wired with the test mocks.
    ///
    /// Handlers are written against `&MutationContext`. Pass the bridged
    /// context to invoke real handler bodies from tests. The mock job and
    /// workflow dispatchers remain accessible on the [`TestMutationContext`]
    /// for assertions (the same `Arc<Mock*>` is shared with the bridge).
    ///
    /// A `sqlx::PgPool` is required because [`MutationContext`] performs
    /// pool-backed operations (`tx()`, `conn()`); a real pool is the only
    /// safe way to support those paths. Tests that don't touch the database
    /// can pass a pool created in a test fixture and never call those
    /// methods.
    pub fn into_mutation_context(self, pool: sqlx::PgPool) -> MutationContext {
        let job_dispatch: Option<Arc<dyn JobDispatch>> = Some(self.job_dispatch);
        let workflow_dispatch: Option<Arc<dyn WorkflowDispatch>> = Some(self.workflow_dispatch);
        MutationContext::with_env(
            pool,
            self.auth,
            self.request,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            job_dispatch,
            workflow_dispatch,
            self.env_provider,
        )
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

impl_test_auth_builder!(TestMutationContextBuilder);
impl_test_env_builder!(TestMutationContextBuilder);

impl TestMutationContextBuilder {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ForgeError;

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

    #[tokio::test]
    async fn minimal_is_unauthenticated_and_user_id_errors() {
        let ctx = TestMutationContext::minimal();
        assert!(!ctx.auth.is_authenticated());
        let err = ctx.user_id().unwrap_err();
        assert!(
            matches!(err, ForgeError::Unauthorized(_)),
            "expected Unauthorized, got {err:?}"
        );
        assert!(ctx.db().is_none());
    }

    #[tokio::test]
    async fn authenticated_helper_exposes_user_id() {
        let uid = Uuid::new_v4();
        let ctx = TestMutationContext::authenticated(uid);
        assert_eq!(ctx.user_id().unwrap(), uid);
        assert!(ctx.auth.is_authenticated());
    }

    #[tokio::test]
    async fn as_subject_authenticates_without_uuid() {
        let ctx = TestMutationContext::builder()
            .as_subject("firebase|abc123")
            .build();
        // Subject-only auth: authenticated but no UUID.
        assert!(ctx.auth.is_authenticated());
        assert!(ctx.user_id().is_err(), "no UUID -> require_user_id fails");
        assert_eq!(
            ctx.auth.claim("sub"),
            Some(&serde_json::json!("firebase|abc123"))
        );
    }

    #[tokio::test]
    async fn builder_with_role_and_with_roles_compose() {
        let ctx = TestMutationContext::builder()
            .as_user(Uuid::new_v4())
            .with_role("editor")
            .with_roles(vec!["admin".to_string(), "billing".to_string()])
            .build();
        assert!(ctx.auth.has_role("editor"));
        assert!(ctx.auth.has_role("admin"));
        assert!(ctx.auth.has_role("billing"));
        assert!(!ctx.auth.has_role("ghost"));
    }

    #[tokio::test]
    async fn with_claim_round_trips_through_auth_context() {
        let ctx = TestMutationContext::builder()
            .as_user(Uuid::new_v4())
            .with_claim("tenant", serde_json::json!("acme"))
            .build();
        assert_eq!(ctx.auth.claim("tenant"), Some(&serde_json::json!("acme")));
    }

    #[tokio::test]
    async fn with_envs_bulk_loads_provider() {
        let mut vars = HashMap::new();
        vars.insert("K1".to_string(), "v1".to_string());
        vars.insert("K2".to_string(), "v2".to_string());
        let ctx = TestMutationContext::builder().with_envs(vars).build();
        assert_eq!(ctx.env("K1"), Some("v1".to_string()));
        assert_eq!(ctx.env("K2"), Some("v2".to_string()));
        assert!(ctx.env_mock().was_accessed("K1"));
    }

    #[tokio::test]
    async fn with_env_single_and_with_envs_compose() {
        let mut bulk = HashMap::new();
        bulk.insert("BULK".to_string(), "b".to_string());
        let ctx = TestMutationContext::builder()
            .with_env("ONE", "1")
            .with_envs(bulk)
            .build();
        assert_eq!(ctx.env("ONE"), Some("1".to_string()));
        assert_eq!(ctx.env("BULK"), Some("b".to_string()));
    }

    #[tokio::test]
    async fn with_job_dispatch_shares_state() {
        let shared = Arc::new(MockJobDispatch::new());
        let ctx = TestMutationContext::builder()
            .with_job_dispatch(shared.clone())
            .build();
        ctx.dispatch_job("ext", serde_json::json!({}))
            .await
            .unwrap();
        // Outside handle sees the dispatch.
        shared.assert_dispatched("ext");
    }

    #[tokio::test]
    async fn with_workflow_dispatch_shares_state() {
        let shared = Arc::new(MockWorkflowDispatch::new());
        let ctx = TestMutationContext::builder()
            .with_workflow_dispatch(shared.clone())
            .build();
        ctx.start_workflow("ext_wf", serde_json::json!({}))
            .await
            .unwrap();
        shared.assert_started("ext_wf");
    }

    #[cfg(feature = "testcontainers")]
    #[tokio::test]
    async fn bridge_to_mutation_context_preserves_mocks() {
        // A handler written against the production `MutationContext` should
        // run unchanged when invoked through the bridged test context, with
        // dispatches landing on the same `MockJobDispatch` exposed by the
        // builder.
        use crate::function::MutationContext;
        use crate::testing::db::TestDatabase;

        let db = TestDatabase::from_env().await.expect("test DB");
        let shared_jobs = Arc::new(super::super::super::mock_dispatch::MockJobDispatch::new());
        let uid = Uuid::new_v4();

        let test_ctx = TestMutationContext::builder()
            .as_user(uid)
            .with_job_dispatch(shared_jobs.clone())
            .build();
        let ctx: MutationContext = test_ctx.into_mutation_context(db.pool().clone());

        // Simulate what a handler would do.
        ctx.dispatch_job("welcome_email", serde_json::json!({"to": "a@b"}))
            .await
            .expect("dispatch through bridged context");

        shared_jobs.assert_dispatched("welcome_email");
    }

    #[tokio::test]
    async fn mock_http_json_executes_via_pattern() {
        let ctx = TestMutationContext::builder()
            .mock_http_json("https://api.test/echo", serde_json::json!({"ok": true}))
            .build();
        let req = MockRequest {
            method: "GET".to_string(),
            path: "/echo".to_string(),
            url: "https://api.test/echo".to_string(),
            headers: HashMap::new(),
            body: serde_json::Value::Null,
        };
        let resp = ctx.http().execute(req).await;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, serde_json::json!({"ok": true}));
        ctx.http().assert_called("https://api.test/echo");
    }
}

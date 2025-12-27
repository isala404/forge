//! Test context for integration tests.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Serialize};
use uuid::Uuid;

use forge_core::error::{ForgeError, Result};
use forge_core::function::{AuthContext, MutationContext, QueryContext};
use forge_core::job::JobStatus;
use forge_core::workflow::WorkflowStatus;

use super::mock::{MockHttp, MockRequest, MockResponse};
use super::TestConfig;

/// Test context for integration tests.
///
/// Provides an isolated testing environment with transaction-based
/// isolation, mock HTTP support, and test utilities.
pub struct TestContext {
    /// Database pool (if connected).
    pool: Option<sqlx::PgPool>,
    /// Transaction for isolation.
    #[allow(dead_code)]
    tx: Option<sqlx::Transaction<'static, sqlx::Postgres>>,
    /// HTTP mock.
    mock_http: MockHttp,
    /// Auth context.
    auth: AuthContext,
    /// Test configuration.
    #[allow(dead_code)]
    config: TestConfig,
    /// Dispatched jobs for verification.
    dispatched_jobs: Vec<DispatchedJob>,
    /// Started workflows for verification.
    started_workflows: Vec<StartedWorkflow>,
}

/// Record of a dispatched job.
#[derive(Debug, Clone)]
pub struct DispatchedJob {
    /// Job ID.
    pub id: Uuid,
    /// Job type name.
    pub job_type: String,
    /// Job input.
    pub input: serde_json::Value,
    /// Dispatch time.
    pub dispatched_at: DateTime<Utc>,
    /// Current status (for test verification).
    pub status: JobStatus,
}

/// Record of a started workflow.
#[derive(Debug, Clone)]
pub struct StartedWorkflow {
    /// Run ID.
    pub run_id: Uuid,
    /// Workflow name.
    pub workflow_name: String,
    /// Input.
    pub input: serde_json::Value,
    /// Started time.
    pub started_at: DateTime<Utc>,
    /// Current status.
    pub status: WorkflowStatus,
    /// Completed steps.
    pub completed_steps: Vec<String>,
}

impl TestContext {
    /// Create a new test context (without database).
    pub fn new_without_db() -> Self {
        Self {
            pool: None,
            tx: None,
            mock_http: MockHttp::new(),
            auth: AuthContext::unauthenticated(),
            config: TestConfig::default(),
            dispatched_jobs: Vec::new(),
            started_workflows: Vec::new(),
        }
    }

    /// Create a new test context with database connection.
    pub async fn new() -> Result<Self> {
        let config = TestConfig::default();
        Self::with_config(config).await
    }

    /// Create a test context with custom configuration.
    pub async fn with_config(config: TestConfig) -> Result<Self> {
        let pool = if let Some(ref url) = config.database_url {
            Some(
                sqlx::postgres::PgPoolOptions::new()
                    .max_connections(config.max_connections)
                    .acquire_timeout(Duration::from_secs(30))
                    .connect(url)
                    .await
                    .map_err(|e| ForgeError::Database(e.to_string()))?,
            )
        } else {
            None
        };

        Ok(Self {
            pool,
            tx: None,
            mock_http: MockHttp::new(),
            auth: AuthContext::unauthenticated(),
            config,
            dispatched_jobs: Vec::new(),
            started_workflows: Vec::new(),
        })
    }

    /// Create a builder for more complex setup.
    pub fn builder() -> TestContextBuilder {
        TestContextBuilder::new()
    }

    /// Get the database pool.
    pub fn pool(&self) -> Option<&sqlx::PgPool> {
        self.pool.as_ref()
    }

    /// Get the auth context.
    pub fn auth(&self) -> &AuthContext {
        &self.auth
    }

    /// Get the user ID if authenticated.
    pub fn user_id(&self) -> Option<Uuid> {
        if self.auth.is_authenticated() {
            self.auth.user_id()
        } else {
            None
        }
    }

    /// Set the authenticated user.
    pub fn set_user(&mut self, user_id: Uuid) {
        self.auth = AuthContext::authenticated(user_id, vec![], HashMap::new());
    }

    /// Get the mock HTTP.
    pub fn mock_http(&self) -> &MockHttp {
        &self.mock_http
    }

    /// Get mutable mock HTTP.
    pub fn mock_http_mut(&mut self) -> &mut MockHttp {
        &mut self.mock_http
    }

    /// Execute a query function.
    pub async fn query<F, I, O>(&self, _func: F, _input: I) -> Result<O>
    where
        F: Fn(
            QueryContext,
            I,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<O>> + Send>>,
        I: Serialize,
        O: DeserializeOwned,
    {
        // In a real implementation, this would call the function with a proper context
        Err(ForgeError::Internal(
            "Query execution requires database connection".to_string(),
        ))
    }

    /// Execute a mutation function.
    pub async fn mutate<F, I, O>(&self, _func: F, _input: I) -> Result<O>
    where
        F: Fn(
            MutationContext,
            I,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<O>> + Send>>,
        I: Serialize,
        O: DeserializeOwned,
    {
        // In a real implementation, this would call the function with a proper context
        Err(ForgeError::Internal(
            "Mutation execution requires database connection".to_string(),
        ))
    }

    /// Dispatch a job for testing.
    pub fn dispatch_job(&mut self, job_type: &str, input: serde_json::Value) -> Uuid {
        let job_id = Uuid::new_v4();
        self.dispatched_jobs.push(DispatchedJob {
            id: job_id,
            job_type: job_type.to_string(),
            input,
            dispatched_at: Utc::now(),
            status: JobStatus::Pending,
        });
        job_id
    }

    /// Get dispatched jobs.
    pub fn dispatched_jobs(&self) -> &[DispatchedJob] {
        &self.dispatched_jobs
    }

    /// Check if a job was dispatched.
    pub fn job_dispatched(&self, job_type: &str) -> bool {
        self.dispatched_jobs.iter().any(|j| j.job_type == job_type)
    }

    /// Get job status.
    pub fn job_status(&self, job_id: Uuid) -> Option<JobStatus> {
        self.dispatched_jobs
            .iter()
            .find(|j| j.id == job_id)
            .map(|j| j.status)
    }

    /// Mark a job as completed (for testing).
    pub fn complete_job(&mut self, job_id: Uuid) {
        if let Some(job) = self.dispatched_jobs.iter_mut().find(|j| j.id == job_id) {
            job.status = JobStatus::Completed;
        }
    }

    /// Run all pending jobs synchronously.
    pub fn run_jobs(&mut self) {
        for job in &mut self.dispatched_jobs {
            if job.status == JobStatus::Pending {
                job.status = JobStatus::Completed;
            }
        }
    }

    /// Start a workflow for testing.
    pub fn start_workflow(&mut self, workflow_name: &str, input: serde_json::Value) -> Uuid {
        let run_id = Uuid::new_v4();
        self.started_workflows.push(StartedWorkflow {
            run_id,
            workflow_name: workflow_name.to_string(),
            input,
            started_at: Utc::now(),
            status: WorkflowStatus::Created,
            completed_steps: Vec::new(),
        });
        run_id
    }

    /// Get started workflows.
    pub fn started_workflows(&self) -> &[StartedWorkflow] {
        &self.started_workflows
    }

    /// Get workflow status.
    pub fn workflow_status(&self, run_id: Uuid) -> Option<WorkflowStatus> {
        self.started_workflows
            .iter()
            .find(|w| w.run_id == run_id)
            .map(|w| w.status)
    }

    /// Mark a workflow step as completed.
    pub fn complete_workflow_step(&mut self, run_id: Uuid, step_name: &str) {
        if let Some(workflow) = self
            .started_workflows
            .iter_mut()
            .find(|w| w.run_id == run_id)
        {
            workflow.completed_steps.push(step_name.to_string());
        }
    }

    /// Complete a workflow.
    pub fn complete_workflow(&mut self, run_id: Uuid) {
        if let Some(workflow) = self
            .started_workflows
            .iter_mut()
            .find(|w| w.run_id == run_id)
        {
            workflow.status = WorkflowStatus::Completed;
        }
    }

    /// Check if a workflow step was completed.
    pub fn workflow_step_completed(&self, run_id: Uuid, step_name: &str) -> bool {
        self.started_workflows
            .iter()
            .find(|w| w.run_id == run_id)
            .map(|w| w.completed_steps.contains(&step_name.to_string()))
            .unwrap_or(false)
    }
}

/// Builder for TestContext.
pub struct TestContextBuilder {
    config: TestConfig,
    user_id: Option<Uuid>,
    roles: Vec<String>,
    custom_claims: HashMap<String, serde_json::Value>,
    mock_http: MockHttp,
}

impl TestContextBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            config: TestConfig::default(),
            user_id: None,
            roles: Vec::new(),
            custom_claims: HashMap::new(),
            mock_http: MockHttp::new(),
        }
    }

    /// Set the database URL.
    pub fn database_url(mut self, url: impl Into<String>) -> Self {
        self.config.database_url = Some(url.into());
        self
    }

    /// Set the authenticated user.
    pub fn as_user(mut self, user_id: Uuid) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Add roles.
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }

    /// Add custom claims.
    pub fn with_claims(mut self, claims: HashMap<String, serde_json::Value>) -> Self {
        self.custom_claims = claims;
        self
    }

    /// Enable logging.
    pub fn with_logging(mut self, enabled: bool) -> Self {
        self.config.logging = enabled;
        self
    }

    /// Add HTTP mock.
    pub fn mock_http(
        mut self,
        pattern: &str,
        handler: impl Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    ) -> Self {
        self.mock_http.add_mock(pattern, handler);
        self
    }

    /// Build the test context.
    pub async fn build(self) -> Result<TestContext> {
        let mut ctx = TestContext::with_config(self.config).await?;

        if let Some(user_id) = self.user_id {
            ctx.auth = AuthContext::authenticated(user_id, self.roles, self.custom_claims);
        }

        ctx.mock_http = self.mock_http;

        Ok(ctx)
    }
}

impl Default for TestContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_builder() {
        let builder = TestContextBuilder::new()
            .as_user(Uuid::new_v4())
            .with_logging(true);

        assert!(builder.user_id.is_some());
        assert!(builder.config.logging);
    }

    #[test]
    fn test_context_without_db() {
        let ctx = TestContext::new_without_db();
        assert!(ctx.pool().is_none());
        assert!(!ctx.auth().is_authenticated());
    }

    #[test]
    fn test_job_dispatch() {
        let mut ctx = TestContext::new_without_db();
        let job_id = ctx.dispatch_job("send_email", serde_json::json!({"to": "test@example.com"}));

        assert!(ctx.job_dispatched("send_email"));
        assert_eq!(ctx.job_status(job_id), Some(JobStatus::Pending));

        ctx.complete_job(job_id);
        assert_eq!(ctx.job_status(job_id), Some(JobStatus::Completed));
    }

    #[test]
    fn test_workflow_tracking() {
        let mut ctx = TestContext::new_without_db();
        let run_id = ctx.start_workflow(
            "onboarding",
            serde_json::json!({"email": "test@example.com"}),
        );

        assert_eq!(ctx.workflow_status(run_id), Some(WorkflowStatus::Created));

        ctx.complete_workflow_step(run_id, "create_user");
        assert!(ctx.workflow_step_completed(run_id, "create_user"));
        assert!(!ctx.workflow_step_completed(run_id, "send_email"));

        ctx.complete_workflow(run_id);
        assert_eq!(ctx.workflow_status(run_id), Some(WorkflowStatus::Completed));
    }

    #[test]
    fn test_run_jobs() {
        let mut ctx = TestContext::new_without_db();
        let job1 = ctx.dispatch_job("job1", serde_json::json!({}));
        let job2 = ctx.dispatch_job("job2", serde_json::json!({}));

        ctx.run_jobs();

        assert_eq!(ctx.job_status(job1), Some(JobStatus::Completed));
        assert_eq!(ctx.job_status(job2), Some(JobStatus::Completed));
    }
}

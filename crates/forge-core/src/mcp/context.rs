use std::sync::Arc;
use std::time::Duration;

use crate::Result;
use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::{AuthContext, JobDispatch, KvHandle, RequestMetadata, WorkflowDispatch};
use crate::http::CircuitBreakerClient;
use uuid::Uuid;

/// Context for MCP tool execution.
#[non_exhaustive]
pub struct McpToolContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    db_pool: sqlx::PgPool,
    /// HTTP client for external calls.
    http_client: CircuitBreakerClient,
    /// Default timeout for outbound HTTP requests made through the
    /// circuit-breaker client. `None` means unlimited.
    http_timeout: Option<Duration>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    env_provider: Arc<dyn EnvProvider>,
    /// KV store handle. `None` until threaded in by the runtime.
    kv: Option<Arc<dyn KvHandle>>,
}

impl McpToolContext {
    /// Create a new MCP tool context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self::with_dispatch(db_pool, auth, request, None, None)
    }

    /// Create a context with dispatch capabilities.
    pub fn with_dispatch(
        db_pool: sqlx::PgPool,
        auth: AuthContext,
        request: RequestMetadata,
        job_dispatch: Option<Arc<dyn JobDispatch>>,
        workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    ) -> Self {
        Self::with_env(
            db_pool,
            auth,
            request,
            job_dispatch,
            workflow_dispatch,
            Arc::new(RealEnvProvider::new()),
        )
    }

    /// Create a context with a custom environment provider.
    pub fn with_env(
        db_pool: sqlx::PgPool,
        auth: AuthContext,
        request: RequestMetadata,
        job_dispatch: Option<Arc<dyn JobDispatch>>,
        workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
        env_provider: Arc<dyn EnvProvider>,
    ) -> Self {
        Self {
            auth,
            request,
            db_pool,
            http_client: CircuitBreakerClient::with_ssrf_protection(),
            http_timeout: None,
            job_dispatch,
            workflow_dispatch,
            env_provider,
            kv: None,
        }
    }

    /// Set the HTTP client. Called by the runtime to inject the shared client.
    pub fn with_http_client(mut self, client: CircuitBreakerClient) -> Self {
        self.http_client = client;
        self
    }

    /// Attach a KV store handle. Called by the runtime before handing the
    /// context to the handler.
    pub fn set_kv(&mut self, kv: Arc<dyn KvHandle>) {
        self.kv = Some(kv);
    }

    /// Access the KV store.
    pub fn kv(&self) -> crate::error::Result<&dyn KvHandle> {
        self.kv
            .as_deref()
            .ok_or_else(|| crate::error::ForgeError::internal("KV store not available"))
    }

    pub fn db(&self) -> crate::function::ForgeDb {
        crate::function::ForgeDb::from_pool(&self.db_pool)
    }

    /// Get a `DbConn` for use in shared helper functions.
    pub fn db_conn(&self) -> crate::function::DbConn<'_> {
        crate::function::DbConn::Pool(self.db_pool.clone())
    }

    /// Acquire a connection compatible with sqlx compile-time checked macros.
    pub async fn conn(&self) -> sqlx::Result<crate::function::ForgeConn<'static>> {
        Ok(crate::function::ForgeConn::Pool(
            self.db_pool.acquire().await?,
        ))
    }

    /// Get the HTTP client for external requests.
    pub fn http(&self) -> crate::http::HttpClient {
        self.http_client.with_timeout(self.http_timeout)
    }

    /// Get the raw reqwest client, bypassing circuit breaker execution.
    pub fn raw_http(&self) -> &reqwest::Client {
        self.http_client.inner()
    }

    /// Set the default timeout for outbound HTTP requests.
    pub fn set_http_timeout(&mut self, timeout: Option<Duration>) {
        self.http_timeout = timeout;
    }

    /// Get the authenticated user's UUID. Returns 401 if not authenticated.
    pub fn user_id(&self) -> Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Get the tenant ID from JWT claims, if present.
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.auth.tenant_id()
    }

    /// Dispatch a background job.
    pub async fn dispatch_job<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::internal("Job dispatch not available")
        })?;

        let args_json = serde_json::to_value(args)?;
        dispatcher
            .dispatch_by_name(
                job_type,
                args_json,
                self.auth.principal_id(),
                self.auth.tenant_id(),
            )
            .await
    }

    /// Type-safe dispatch: resolves the job name from the type's `ForgeJob`
    /// impl and serializes the args at the call site.
    pub async fn dispatch<J: crate::ForgeJob>(&self, args: J::Args) -> Result<Uuid> {
        self.dispatch_job(J::info().name, args).await
    }

    /// Request cancellation for a job.
    pub async fn cancel_job(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> Result<bool> {
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::internal("Job dispatch not available")
        })?;
        dispatcher.cancel(job_id, reason).await
    }

    /// Start a workflow.
    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> Result<Uuid> {
        let dispatcher = self.workflow_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::internal("Workflow dispatch not available")
        })?;

        let input_json = serde_json::to_value(input)?;
        dispatcher
            .start_by_name(
                workflow_name,
                input_json,
                self.auth.principal_id(),
                Some(self.request.trace_id().to_string()),
            )
            .await
    }

    /// Type-safe workflow start.
    pub async fn start<W: crate::ForgeWorkflow>(&self, input: W::Input) -> Result<Uuid> {
        self.start_workflow(W::info().name, input).await
    }
}

impl EnvAccess for McpToolContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

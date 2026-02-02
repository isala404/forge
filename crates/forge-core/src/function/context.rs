use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::postgres::{PgArguments, PgQueryResult, PgRow};
use sqlx::{FromRow, Postgres, Transaction};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use super::dispatch::{JobDispatch, WorkflowDispatch};
use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::http::CircuitBreakerClient;
use crate::job::JobInfo;

/// Abstracts over pool and transaction connections so handlers can work with either.
pub enum DbConn<'a> {
    Pool(&'a sqlx::PgPool),
    Transaction(Arc<AsyncMutex<Transaction<'static, Postgres>>>),
}

impl DbConn<'_> {
    pub async fn fetch_one<'q, O>(
        &self,
        query: sqlx::query::QueryAs<'q, Postgres, O, PgArguments>,
    ) -> sqlx::Result<O>
    where
        O: Send + Unpin + for<'r> FromRow<'r, PgRow>,
    {
        match self {
            DbConn::Pool(pool) => query.fetch_one(*pool).await,
            DbConn::Transaction(tx) => query.fetch_one(&mut **tx.lock().await).await,
        }
    }

    pub async fn fetch_optional<'q, O>(
        &self,
        query: sqlx::query::QueryAs<'q, Postgres, O, PgArguments>,
    ) -> sqlx::Result<Option<O>>
    where
        O: Send + Unpin + for<'r> FromRow<'r, PgRow>,
    {
        match self {
            DbConn::Pool(pool) => query.fetch_optional(*pool).await,
            DbConn::Transaction(tx) => query.fetch_optional(&mut **tx.lock().await).await,
        }
    }

    pub async fn fetch_all<'q, O>(
        &self,
        query: sqlx::query::QueryAs<'q, Postgres, O, PgArguments>,
    ) -> sqlx::Result<Vec<O>>
    where
        O: Send + Unpin + for<'r> FromRow<'r, PgRow>,
    {
        match self {
            DbConn::Pool(pool) => query.fetch_all(*pool).await,
            DbConn::Transaction(tx) => query.fetch_all(&mut **tx.lock().await).await,
        }
    }

    pub async fn execute<'q>(
        &self,
        query: sqlx::query::Query<'q, Postgres, PgArguments>,
    ) -> sqlx::Result<PgQueryResult> {
        match self {
            DbConn::Pool(pool) => query.execute(*pool).await,
            DbConn::Transaction(tx) => query.execute(&mut **tx.lock().await).await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingJob {
    pub id: Uuid,
    pub job_type: String,
    pub args: serde_json::Value,
    pub context: serde_json::Value,
    pub priority: i32,
    pub max_attempts: i32,
    pub worker_capability: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingWorkflow {
    pub id: Uuid,
    pub workflow_name: String,
    pub input: serde_json::Value,
}

#[derive(Default)]
pub struct OutboxBuffer {
    pub jobs: Vec<PendingJob>,
    pub workflows: Vec<PendingWorkflow>,
}

/// Authentication context available to all functions.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// The authenticated user ID (if any).
    user_id: Option<Uuid>,
    /// User roles.
    roles: Vec<String>,
    /// Custom claims from JWT.
    claims: HashMap<String, serde_json::Value>,
    /// Whether the request is authenticated.
    authenticated: bool,
}

impl AuthContext {
    /// Create an unauthenticated context.
    pub fn unauthenticated() -> Self {
        Self {
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            authenticated: false,
        }
    }

    /// Create an authenticated context with a UUID user ID.
    pub fn authenticated(
        user_id: Uuid,
        roles: Vec<String>,
        claims: HashMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            user_id: Some(user_id),
            roles,
            claims,
            authenticated: true,
        }
    }

    /// Create an authenticated context without requiring a UUID user ID.
    ///
    /// Use this for auth providers that don't use UUID subjects (e.g., Firebase,
    /// Clerk). The raw subject string is available via `subject()` method
    /// from the "sub" claim.
    pub fn authenticated_without_uuid(
        roles: Vec<String>,
        claims: HashMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            user_id: None,
            roles,
            claims,
            authenticated: true,
        }
    }

    /// Check if the user is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Get the user ID if authenticated.
    pub fn user_id(&self) -> Option<Uuid> {
        self.user_id
    }

    /// Get the user ID, returning an error if not authenticated.
    pub fn require_user_id(&self) -> crate::error::Result<Uuid> {
        self.user_id
            .ok_or_else(|| crate::error::ForgeError::Unauthorized("Authentication required".into()))
    }

    /// Check if the user has a specific role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Require a specific role, returning an error if not present.
    pub fn require_role(&self, role: &str) -> crate::error::Result<()> {
        if self.has_role(role) {
            Ok(())
        } else {
            Err(crate::error::ForgeError::Forbidden(format!(
                "Required role '{}' not present",
                role
            )))
        }
    }

    /// Get a custom claim value.
    pub fn claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.claims.get(key)
    }

    /// Get all roles.
    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    /// Get the raw subject claim.
    ///
    /// This works with any provider's subject format (UUID, email, custom ID).
    /// For providers like Firebase or Clerk that don't use UUIDs, use this
    /// instead of `user_id()`.
    pub fn subject(&self) -> Option<&str> {
        self.claims.get("sub").and_then(|v| v.as_str())
    }

    /// Like `require_user_id()` but returns the raw subject string for non-UUID providers.
    pub fn require_subject(&self) -> crate::error::Result<&str> {
        if !self.authenticated {
            return Err(crate::error::ForgeError::Unauthorized(
                "Authentication required".to_string(),
            ));
        }
        self.subject().ok_or_else(|| {
            crate::error::ForgeError::Unauthorized("No subject claim in token".to_string())
        })
    }
}

/// Request metadata available to all functions.
#[derive(Debug, Clone)]
pub struct RequestMetadata {
    /// Unique request ID for tracing.
    pub request_id: Uuid,
    /// Trace ID for distributed tracing.
    pub trace_id: String,
    /// Client IP address.
    pub client_ip: Option<String>,
    /// User agent string.
    pub user_agent: Option<String>,
    /// Request timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl RequestMetadata {
    /// Create new request metadata.
    pub fn new() -> Self {
        Self {
            request_id: Uuid::new_v4(),
            trace_id: Uuid::new_v4().to_string(),
            client_ip: None,
            user_agent: None,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Create with a specific trace ID.
    pub fn with_trace_id(trace_id: String) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            trace_id,
            client_ip: None,
            user_agent: None,
            timestamp: chrono::Utc::now(),
        }
    }
}

impl Default for RequestMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Context for query functions (read-only database access).
pub struct QueryContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for read operations.
    db_pool: sqlx::PgPool,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
}

impl QueryContext {
    /// Create a new query context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self {
            auth,
            request,
            db_pool,
            env_provider: Arc::new(RealEnvProvider::new()),
        }
    }

    /// Create a query context with a custom environment provider.
    pub fn with_env(
        db_pool: sqlx::PgPool,
        auth: AuthContext,
        request: RequestMetadata,
        env_provider: Arc<dyn EnvProvider>,
    ) -> Self {
        Self {
            auth,
            request,
            db_pool,
            env_provider,
        }
    }

    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the authenticated user ID or return an error.
    pub fn require_user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Like `require_user_id()` but for non-UUID auth providers.
    pub fn require_subject(&self) -> crate::error::Result<&str> {
        self.auth.require_subject()
    }
}

impl EnvAccess for QueryContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Callback type for looking up job info by name.
pub type JobInfoLookup = Arc<dyn Fn(&str) -> Option<JobInfo> + Send + Sync>;

/// Context for mutation functions (transactional database access).
pub struct MutationContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for transactional operations.
    db_pool: sqlx::PgPool,
    /// HTTP client with circuit breaker for external requests.
    http_client: CircuitBreakerClient,
    /// Optional job dispatcher for dispatching background jobs.
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    /// Optional workflow dispatcher for starting workflows.
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
    /// Transaction handle for transactional mutations.
    tx: Option<Arc<AsyncMutex<Transaction<'static, Postgres>>>>,
    /// Outbox buffer for jobs/workflows dispatched during transaction.
    outbox: Option<Arc<Mutex<OutboxBuffer>>>,
    /// Job info lookup for transactional dispatch.
    job_info_lookup: Option<JobInfoLookup>,
}

impl MutationContext {
    /// Create a new mutation context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self {
            auth,
            request,
            db_pool,
            http_client: CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            job_dispatch: None,
            workflow_dispatch: None,
            env_provider: Arc::new(RealEnvProvider::new()),
            tx: None,
            outbox: None,
            job_info_lookup: None,
        }
    }

    /// Create a mutation context with dispatch capabilities.
    pub fn with_dispatch(
        db_pool: sqlx::PgPool,
        auth: AuthContext,
        request: RequestMetadata,
        http_client: CircuitBreakerClient,
        job_dispatch: Option<Arc<dyn JobDispatch>>,
        workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    ) -> Self {
        Self {
            auth,
            request,
            db_pool,
            http_client,
            job_dispatch,
            workflow_dispatch,
            env_provider: Arc::new(RealEnvProvider::new()),
            tx: None,
            outbox: None,
            job_info_lookup: None,
        }
    }

    /// Create a mutation context with a custom environment provider.
    pub fn with_env(
        db_pool: sqlx::PgPool,
        auth: AuthContext,
        request: RequestMetadata,
        http_client: CircuitBreakerClient,
        job_dispatch: Option<Arc<dyn JobDispatch>>,
        workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
        env_provider: Arc<dyn EnvProvider>,
    ) -> Self {
        Self {
            auth,
            request,
            db_pool,
            http_client,
            job_dispatch,
            workflow_dispatch,
            env_provider,
            tx: None,
            outbox: None,
            job_info_lookup: None,
        }
    }

    /// Returns handles to transaction and outbox for the caller to commit/flush.
    pub fn with_transaction(
        db_pool: sqlx::PgPool,
        tx: Transaction<'static, Postgres>,
        auth: AuthContext,
        request: RequestMetadata,
        http_client: CircuitBreakerClient,
        job_info_lookup: JobInfoLookup,
    ) -> (
        Self,
        Arc<AsyncMutex<Transaction<'static, Postgres>>>,
        Arc<Mutex<OutboxBuffer>>,
    ) {
        let tx_handle = Arc::new(AsyncMutex::new(tx));
        let outbox = Arc::new(Mutex::new(OutboxBuffer::default()));

        let ctx = Self {
            auth,
            request,
            db_pool,
            http_client,
            job_dispatch: None,
            workflow_dispatch: None,
            env_provider: Arc::new(RealEnvProvider::new()),
            tx: Some(tx_handle.clone()),
            outbox: Some(outbox.clone()),
            job_info_lookup: Some(job_info_lookup),
        };

        (ctx, tx_handle, outbox)
    }

    pub fn is_transactional(&self) -> bool {
        self.tx.is_some()
    }

    pub fn db(&self) -> DbConn<'_> {
        match &self.tx {
            Some(tx) => DbConn::Transaction(tx.clone()),
            None => DbConn::Pool(&self.db_pool),
        }
    }

    /// Direct pool access for operations that cannot run inside a transaction.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the HTTP client for external requests.
    ///
    /// The client includes circuit breaker protection that tracks failure rates
    /// per host. After repeated failures, requests fail fast to prevent cascade
    /// failures when downstream services are unhealthy.
    pub fn http(&self) -> &reqwest::Client {
        self.http_client.inner()
    }

    /// Get the circuit breaker client directly for advanced usage.
    pub fn http_with_circuit_breaker(&self) -> &CircuitBreakerClient {
        &self.http_client
    }

    pub fn require_user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }

    pub fn require_subject(&self) -> crate::error::Result<&str> {
        self.auth.require_subject()
    }

    /// In transactional mode, buffers for atomic commit; otherwise dispatches immediately.
    pub async fn dispatch_job<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> crate::error::Result<Uuid> {
        let args_json = serde_json::to_value(args)?;

        // Transactional mode: buffer the job for atomic commit
        if let (Some(outbox), Some(job_info_lookup)) = (&self.outbox, &self.job_info_lookup) {
            let job_info = job_info_lookup(job_type).ok_or_else(|| {
                crate::error::ForgeError::NotFound(format!("Job type '{}' not found", job_type))
            })?;

            let pending = PendingJob {
                id: Uuid::new_v4(),
                job_type: job_type.to_string(),
                args: args_json,
                context: serde_json::json!({}),
                priority: job_info.priority.as_i32(),
                max_attempts: job_info.retry.max_attempts as i32,
                worker_capability: job_info.worker_capability.map(|s| s.to_string()),
            };

            let job_id = pending.id;
            outbox.lock().unwrap().jobs.push(pending);
            return Ok(job_id);
        }

        // Non-transactional mode: dispatch immediately
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".into())
        })?;
        dispatcher.dispatch_by_name(job_type, args_json).await
    }

    /// Dispatch a job with initial context.
    pub async fn dispatch_job_with_context<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        context: serde_json::Value,
    ) -> crate::error::Result<Uuid> {
        let args_json = serde_json::to_value(args)?;

        if let (Some(outbox), Some(job_info_lookup)) = (&self.outbox, &self.job_info_lookup) {
            let job_info = job_info_lookup(job_type).ok_or_else(|| {
                crate::error::ForgeError::NotFound(format!("Job type '{}' not found", job_type))
            })?;

            let pending = PendingJob {
                id: Uuid::new_v4(),
                job_type: job_type.to_string(),
                args: args_json,
                context,
                priority: job_info.priority.as_i32(),
                max_attempts: job_info.retry.max_attempts as i32,
                worker_capability: job_info.worker_capability.map(|s| s.to_string()),
            };

            let job_id = pending.id;
            outbox.lock().unwrap().jobs.push(pending);
            return Ok(job_id);
        }

        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".into())
        })?;
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

    /// In transactional mode, buffers for atomic commit; otherwise starts immediately.
    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> crate::error::Result<Uuid> {
        let input_json = serde_json::to_value(input)?;

        // Transactional mode: buffer the workflow for atomic commit
        if let Some(outbox) = &self.outbox {
            let pending = PendingWorkflow {
                id: Uuid::new_v4(),
                workflow_name: workflow_name.to_string(),
                input: input_json,
            };

            let workflow_id = pending.id;
            outbox.lock().unwrap().workflows.push(pending);
            return Ok(workflow_id);
        }

        // Non-transactional mode: start immediately
        let dispatcher = self.workflow_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Workflow dispatch not available".into())
        })?;
        dispatcher.start_by_name(workflow_name, input_json).await
    }
}

impl EnvAccess for MutationContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_context_unauthenticated() {
        let ctx = AuthContext::unauthenticated();
        assert!(!ctx.is_authenticated());
        assert!(ctx.user_id().is_none());
        assert!(ctx.require_user_id().is_err());
    }

    #[test]
    fn test_auth_context_authenticated() {
        let user_id = Uuid::new_v4();
        let ctx = AuthContext::authenticated(
            user_id,
            vec!["admin".to_string(), "user".to_string()],
            HashMap::new(),
        );

        assert!(ctx.is_authenticated());
        assert_eq!(ctx.user_id(), Some(user_id));
        assert!(ctx.require_user_id().is_ok());
        assert!(ctx.has_role("admin"));
        assert!(ctx.has_role("user"));
        assert!(!ctx.has_role("superadmin"));
        assert!(ctx.require_role("admin").is_ok());
        assert!(ctx.require_role("superadmin").is_err());
    }

    #[test]
    fn test_auth_context_with_claims() {
        let mut claims = HashMap::new();
        claims.insert("org_id".to_string(), serde_json::json!("org-123"));

        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], claims);

        assert_eq!(ctx.claim("org_id"), Some(&serde_json::json!("org-123")));
        assert!(ctx.claim("nonexistent").is_none());
    }

    #[test]
    fn test_request_metadata() {
        let meta = RequestMetadata::new();
        assert!(!meta.trace_id.is_empty());
        assert!(meta.client_ip.is_none());

        let meta2 = RequestMetadata::with_trace_id("trace-123".to_string());
        assert_eq!(meta2.trace_id, "trace-123");
    }
}

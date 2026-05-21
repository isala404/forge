use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sqlx::{Postgres, Transaction};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::{JobDispatch, KvHandle, WorkflowDispatch};
use crate::http::CircuitBreakerClient;

/// Shared handle to the webhook's active transaction.
///
/// The runtime keeps ownership and commits/rolls back; the handler dispatches
/// jobs and workflows on the same connection so they roll back with the
/// webhook on error.
pub type WebhookTxHandle = Arc<AsyncMutex<Option<Transaction<'static, Postgres>>>>;

/// Context available to webhook handlers.
#[non_exhaustive]
pub struct WebhookContext {
    pub webhook_name: String,
    pub request_id: String,
    pub idempotency_key: Option<String>,
    /// Lowercase keys.
    headers: HashMap<String, String>,
    db_pool: sqlx::PgPool,
    /// Held by the runtime; handler dispatches piggy-back on this connection
    /// so they roll back with the webhook on error.
    tx: Option<WebhookTxHandle>,
    http_client: CircuitBreakerClient,
    /// `None` means unlimited.
    http_timeout: Option<Duration>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    env_provider: Arc<dyn EnvProvider>,
    kv: Option<Arc<dyn KvHandle>>,
}

impl WebhookContext {
    /// Create a new webhook context.
    pub fn new(
        webhook_name: String,
        request_id: String,
        headers: HashMap<String, String>,
        db_pool: sqlx::PgPool,
        http_client: CircuitBreakerClient,
    ) -> Self {
        Self {
            webhook_name,
            request_id,
            idempotency_key: None,
            headers,
            db_pool,
            tx: None,
            http_client,
            http_timeout: None,
            job_dispatch: None,
            workflow_dispatch: None,
            env_provider: Arc::new(RealEnvProvider::new()),
            kv: None,
        }
    }

    /// Build a webhook context that dispatches jobs and workflows on `tx`.
    ///
    /// The runtime opens the transaction, owns the handle, and commits or
    /// rolls back based on the handler's result. Anything the handler
    /// dispatches lands on the same connection, so failed webhooks don't
    /// leave orphaned jobs behind.
    pub fn with_transaction(
        webhook_name: String,
        request_id: String,
        headers: HashMap<String, String>,
        db_pool: sqlx::PgPool,
        tx: Transaction<'static, Postgres>,
        http_client: CircuitBreakerClient,
    ) -> (Self, WebhookTxHandle) {
        let handle: WebhookTxHandle = Arc::new(AsyncMutex::new(Some(tx)));
        let ctx = Self {
            webhook_name,
            request_id,
            idempotency_key: None,
            headers,
            db_pool,
            tx: Some(handle.clone()),
            http_client,
            http_timeout: None,
            job_dispatch: None,
            workflow_dispatch: None,
            env_provider: Arc::new(RealEnvProvider::new()),
            kv: None,
        };
        (ctx, handle)
    }

    /// Whether dispatches will participate in an enclosing transaction.
    pub fn is_transactional(&self) -> bool {
        self.tx.is_some()
    }

    /// Attach a KV store handle. Called by the runtime before handing the
    /// context to the handler.
    pub fn with_kv(mut self, kv: Arc<dyn KvHandle>) -> Self {
        self.kv = Some(kv);
        self
    }

    /// Access the KV store.
    pub fn kv(&self) -> crate::error::Result<&dyn KvHandle> {
        self.kv
            .as_deref()
            .ok_or_else(|| crate::error::ForgeError::internal("KV store not available"))
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

    /// Set workflow dispatcher.
    pub fn with_workflow_dispatch(mut self, dispatcher: Arc<dyn WorkflowDispatch>) -> Self {
        self.workflow_dispatch = Some(dispatcher);
        self
    }

    /// Set environment provider.
    pub fn with_env_provider(mut self, provider: Arc<dyn EnvProvider>) -> Self {
        self.env_provider = provider;
        self
    }

    /// Get database pool.
    pub fn db(&self) -> crate::function::ForgeDb {
        crate::function::ForgeDb::from_pool(&self.db_pool)
    }

    /// Get a `DbConn` for use in shared helper functions.
    ///
    /// In transactional mode, returns a transaction-backed handle. Outside
    /// a transaction, returns a pool-backed handle.
    pub fn db_conn(&self) -> crate::function::DbConn<'_> {
        match &self.tx {
            Some(tx) => crate::function::DbConn::Transaction(tx.clone(), &self.db_pool),
            None => crate::function::DbConn::Pool(self.db_pool.clone()),
        }
    }

    /// Acquire a connection compatible with sqlx compile-time checked macros.
    ///
    /// In transactional mode, returns a guard over the active transaction so
    /// the handler's queries participate in the same transaction as its
    /// dispatches. Outside a transaction, acquires a fresh pool connection.
    pub async fn conn(&self) -> sqlx::Result<crate::function::ForgeConn<'_>> {
        match &self.tx {
            Some(tx) => Ok(crate::function::ForgeConn::Tx(tx.lock().await)),
            None => Ok(crate::function::ForgeConn::Pool(
                self.db_pool.acquire().await?,
            )),
        }
    }

    /// Get the HTTP client for external requests.
    pub fn http(&self) -> crate::http::HttpClient {
        self.http_client.with_timeout(self.http_timeout)
    }

    /// Get the raw reqwest client, bypassing circuit breaker execution.
    pub fn raw_http(&self) -> &reqwest::Client {
        self.http_client.inner()
    }

    pub fn set_http_timeout(&mut self, timeout: Option<Duration>) {
        self.http_timeout = timeout;
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
        let dispatcher = self
            .job_dispatch
            .as_ref()
            .ok_or_else(|| crate::error::ForgeError::internal("Job dispatch not available"))?;
        let args_json = serde_json::to_value(args)?;

        if let Some(tx) = &self.tx {
            let mut guard = tx.lock().await;
            let conn = guard.as_mut().ok_or_else(|| {
                crate::error::ForgeError::internal("Transaction already taken; cannot dispatch job")
            })?;
            return dispatcher
                .dispatch_in_conn(conn, job_type, args_json, None, None)
                .await;
        }

        dispatcher
            .dispatch_by_name(job_type, args_json, None, None)
            .await
    }

    /// Type-safe dispatch: resolves the job name from the type's `ForgeJob`
    /// impl and serializes the args at the call site.
    pub async fn dispatch<J: crate::ForgeJob>(&self, args: J::Args) -> crate::error::Result<Uuid> {
        self.dispatch_job(J::info().name, args).await
    }

    /// Start a workflow.
    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> crate::error::Result<Uuid> {
        let dispatcher = self
            .workflow_dispatch
            .as_ref()
            .ok_or_else(|| crate::error::ForgeError::internal("Workflow dispatch not available"))?;
        let input_json = serde_json::to_value(input)?;

        if let Some(tx) = &self.tx {
            let mut guard = tx.lock().await;
            let conn = guard.as_mut().ok_or_else(|| {
                crate::error::ForgeError::internal(
                    "Transaction already taken; cannot start workflow",
                )
            })?;
            return dispatcher
                .start_in_conn(conn, workflow_name, input_json, None, None)
                .await;
        }

        dispatcher
            .start_by_name(workflow_name, input_json, None, None)
            .await
    }

    /// Type-safe workflow start.
    pub async fn start<W: crate::ForgeWorkflow>(
        &self,
        input: W::Input,
    ) -> crate::error::Result<Uuid> {
        self.start_workflow(W::info().name, input).await
    }

    /// Request cancellation for a job.
    pub async fn cancel_job(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> crate::error::Result<bool> {
        let dispatcher = self
            .job_dispatch
            .as_ref()
            .ok_or_else(|| crate::error::ForgeError::internal("Job dispatch not available"))?;
        dispatcher.cancel(job_id, reason).await
    }
}

impl EnvAccess for WebhookContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
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

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tracing::Span;
use uuid::Uuid;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::{JobDispatch, WorkflowDispatch};
use crate::http::CircuitBreakerClient;

/// Context available to daemon handlers.
#[non_exhaustive]
pub struct DaemonContext {
    /// Daemon name.
    pub daemon_name: String,
    /// Unique instance ID for this daemon execution.
    pub instance_id: Uuid,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client for external calls.
    http_client: CircuitBreakerClient,
    /// Default timeout for outbound HTTP requests made through the
    /// circuit-breaker client. `None` means unlimited.
    http_timeout: Option<Duration>,
    /// Shutdown signal receiver (wrapped in Mutex for interior mutability).
    shutdown_rx: Mutex<watch::Receiver<bool>>,
    /// Job dispatcher for background jobs.
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    /// Workflow dispatcher for starting workflows.
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
    /// Parent span for trace propagation.
    span: Span,
}

impl DaemonContext {
    /// Create a new daemon context.
    pub fn new(
        daemon_name: String,
        instance_id: Uuid,
        db_pool: sqlx::PgPool,
        http_client: CircuitBreakerClient,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            daemon_name,
            instance_id,
            db_pool,
            http_client,
            http_timeout: None,
            shutdown_rx: Mutex::new(shutdown_rx),
            job_dispatch: None,
            workflow_dispatch: None,
            env_provider: Arc::new(RealEnvProvider::new()),
            span: Span::current(),
        }
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

    pub fn http(&self) -> crate::http::HttpClient {
        self.http_client.with_timeout(self.http_timeout)
    }

    pub fn raw_http(&self) -> &reqwest::Client {
        self.http_client.inner()
    }

    pub fn set_http_timeout(&mut self, timeout: Option<Duration>) {
        self.http_timeout = timeout;
    }

    /// Dispatch a background job.
    pub async fn dispatch_job<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> crate::Result<Uuid> {
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".to_string())
        })?;

        let args_json = serde_json::to_value(args)?;
        dispatcher.dispatch_by_name(job_type, args_json, None).await
    }

    /// Start a workflow.
    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> crate::Result<Uuid> {
        let dispatcher = self.workflow_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Workflow dispatch not available".to_string())
        })?;

        let input_json = serde_json::to_value(input)?;
        dispatcher
            .start_by_name(workflow_name, input_json, None, None)
            .await
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown_requested(&self) -> bool {
        // Use try_lock to avoid blocking; if can't lock, assume not shutdown
        self.shutdown_rx
            .try_lock()
            .map(|rx| *rx.borrow())
            .unwrap_or(false)
    }

    /// Wait for shutdown signal.
    ///
    /// Use this in a `tokio::select!` to handle graceful shutdown:
    ///
    /// ```ignore
    /// tokio::select! {
    ///     _ = tokio::time::sleep(Duration::from_secs(60)) => {}
    ///     _ = ctx.shutdown_signal() => break,
    /// }
    /// ```
    pub async fn shutdown_signal(&self) {
        let mut rx = self.shutdown_rx.lock().await;
        // Wait until the value becomes true
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                // Channel closed, treat as shutdown
                break;
            }
        }
    }

    /// Send heartbeat to indicate daemon is alive.
    pub async fn heartbeat(&self) -> crate::Result<()> {
        tracing::trace!(daemon.name = %self.daemon_name, "Sending heartbeat");

        sqlx::query!(
            r#"
            UPDATE forge_daemons
            SET last_heartbeat = NOW()
            WHERE name = $1 AND instance_id = $2
            "#,
            self.daemon_name,
            self.instance_id,
        )
        .execute(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        Ok(())
    }

    /// Get the trace ID for this daemon execution.
    ///
    /// Returns the instance_id as a correlation ID.
    pub fn trace_id(&self) -> String {
        self.instance_id.to_string()
    }

    /// Get the parent span for trace propagation.
    ///
    /// Use this to create child spans within daemon handlers.
    pub fn span(&self) -> &Span {
        &self.span
    }
}

impl EnvAccess for DaemonContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_daemon_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let instance_id = Uuid::new_v4();

        let ctx = DaemonContext::new(
            "test_daemon".to_string(),
            instance_id,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            shutdown_rx,
        );

        assert_eq!(ctx.daemon_name, "test_daemon");
        assert_eq!(ctx.instance_id, instance_id);
        assert!(!ctx.is_shutdown_requested());

        // Signal shutdown
        shutdown_tx.send(true).unwrap();
        assert!(ctx.is_shutdown_requested());
    }

    #[tokio::test]
    async fn test_shutdown_signal() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let ctx = DaemonContext::new(
            "test_daemon".to_string(),
            Uuid::new_v4(),
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            shutdown_rx,
        );

        // Spawn a task to signal shutdown after a delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            shutdown_tx.send(true).unwrap();
        });

        // Wait for shutdown signal
        tokio::time::timeout(std::time::Duration::from_millis(200), ctx.shutdown_signal())
            .await
            .expect("Shutdown signal should complete");

        assert!(ctx.is_shutdown_requested());
    }
}

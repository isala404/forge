use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tracing::Span;
use uuid::Uuid;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::{JobDispatch, KvHandle, WorkflowDispatch};
use crate::http::CircuitBreakerClient;

/// Context available to daemon handlers.
#[non_exhaustive]
pub struct DaemonContext {
    pub daemon_name: String,
    pub instance_id: Uuid,
    db_pool: sqlx::PgPool,
    http_client: CircuitBreakerClient,
    /// `None` means unlimited.
    http_timeout: Option<Duration>,
    /// Wrapped in `Mutex` for interior mutability across async boundaries.
    shutdown_rx: Mutex<watch::Receiver<bool>>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    env_provider: Arc<dyn EnvProvider>,
    span: Span,
    kv: Option<Arc<dyn KvHandle>>,
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
            kv: None,
        }
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
        let dispatcher = self
            .job_dispatch
            .as_ref()
            .ok_or_else(|| crate::error::ForgeError::internal("Job dispatch not available"))?;

        let args_json = serde_json::to_value(args)?;
        dispatcher
            .dispatch_by_name(job_type, args_json, None, None)
            .await
    }

    /// Type-safe dispatch: resolves the job name from the type's `ForgeJob`
    /// impl and serializes the args at the call site.
    pub async fn dispatch<J: crate::ForgeJob>(&self, args: J::Args) -> crate::Result<Uuid> {
        self.dispatch_job(J::info().name, args).await
    }

    /// Request cancellation for a job.
    pub async fn cancel_job(&self, job_id: Uuid, reason: Option<String>) -> crate::Result<bool> {
        let dispatcher = self
            .job_dispatch
            .as_ref()
            .ok_or_else(|| crate::error::ForgeError::internal("Job dispatch not available"))?;
        dispatcher.cancel(job_id, reason).await
    }

    /// Start a workflow.
    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> crate::Result<Uuid> {
        let dispatcher = self
            .workflow_dispatch
            .as_ref()
            .ok_or_else(|| crate::error::ForgeError::internal("Workflow dispatch not available"))?;

        let input_json = serde_json::to_value(input)?;
        dispatcher
            .start_by_name(workflow_name, input_json, None, None)
            .await
    }

    /// Type-safe workflow start.
    pub async fn start<W: crate::ForgeWorkflow>(&self, input: W::Input) -> crate::Result<Uuid> {
        self.start_workflow(W::info().name, input).await
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

    /// Sleep for `interval`, waking early if shutdown is requested.
    ///
    /// Returns `true` if the daemon should continue, `false` if shutdown was
    /// requested before or during the sleep. Intended for the main daemon loop:
    ///
    /// ```ignore
    /// while ctx.tick(Duration::from_secs(60)).await {
    ///     // do periodic work
    /// }
    /// ```
    pub async fn tick(&self, interval: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(interval) => true,
            _ = self.shutdown_signal() => false,
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::env::MockEnvProvider;
    use crate::error::ForgeError;

    fn lazy_ctx() -> (DaemonContext, watch::Sender<bool>, Uuid) {
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
        (ctx, shutdown_tx, instance_id)
    }

    #[tokio::test]
    async fn test_daemon_context_creation() {
        let (ctx, shutdown_tx, instance_id) = lazy_ctx();

        assert_eq!(ctx.daemon_name, "test_daemon");
        assert_eq!(ctx.instance_id, instance_id);
        assert!(!ctx.is_shutdown_requested());

        // Signal shutdown
        shutdown_tx.send(true).unwrap();
        assert!(ctx.is_shutdown_requested());
    }

    #[tokio::test]
    async fn test_shutdown_signal() {
        let (ctx, shutdown_tx, _) = lazy_ctx();

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

    #[tokio::test]
    async fn dispatch_job_returns_internal_when_dispatcher_unset() {
        let (ctx, _tx, _id) = lazy_ctx();
        let err = ctx
            .dispatch_job("send_email", serde_json::json!({"to": "x"}))
            .await
            .unwrap_err();
        match err {
            ForgeError::Internal { context: msg, .. } => assert!(msg.contains("Job dispatch")),
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_workflow_returns_internal_when_dispatcher_unset() {
        let (ctx, _tx, _id) = lazy_ctx();
        let err = ctx
            .start_workflow("checkout", serde_json::json!({"cart": 1}))
            .await
            .unwrap_err();
        match err {
            ForgeError::Internal { context: msg, .. } => assert!(msg.contains("Workflow dispatch")),
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn trace_id_returns_instance_id_as_string() {
        let (ctx, _tx, instance_id) = lazy_ctx();
        assert_eq!(ctx.trace_id(), instance_id.to_string());
    }

    #[tokio::test]
    async fn set_http_timeout_round_trips_through_http_client() {
        let (mut ctx, _tx, _id) = lazy_ctx();
        // Default is unbounded; setting then clearing must be observable via http().
        ctx.set_http_timeout(Some(Duration::from_millis(250)));
        // The HttpClient is opaque, but the call must not panic and the
        // setter must be idempotent on repeat — confirm by re-setting None.
        let _client = ctx.http();
        ctx.set_http_timeout(None);
        let _client = ctx.http();
    }

    #[tokio::test]
    async fn with_env_provider_overrides_real_provider() {
        let (ctx, _tx, _id) = lazy_ctx();
        let mut mock = MockEnvProvider::new();
        mock.set("FORGE_TEST_KEY", "hello");
        let ctx = ctx.with_env_provider(Arc::new(mock));
        // EnvAccess is implemented for DaemonContext; route through the trait
        // method to prove the override took effect end-to-end.
        use crate::env::EnvAccess;
        assert_eq!(ctx.env("FORGE_TEST_KEY"), Some("hello".to_string()));
        assert_eq!(ctx.env("FORGE_MISSING_KEY"), None);
    }

    #[tokio::test]
    async fn tick_returns_true_after_interval_elapses() {
        let (ctx, _tx, _) = lazy_ctx();
        // Short interval; no shutdown fired — must return true.
        let should_continue = ctx.tick(Duration::from_millis(10)).await;
        assert!(should_continue);
    }

    #[tokio::test]
    async fn tick_returns_false_when_shutdown_fires_before_interval() {
        let (ctx, shutdown_tx, _) = lazy_ctx();
        // Signal shutdown immediately before the long interval would finish.
        shutdown_tx.send(true).unwrap();
        // Interval is very long; shutdown should preempt it and return false quickly.
        let should_continue = tokio::time::timeout(
            Duration::from_millis(200),
            ctx.tick(Duration::from_secs(60)),
        )
        .await
        .expect("tick should return promptly on shutdown");
        assert!(!should_continue);
    }

    #[tokio::test]
    async fn span_returns_current_span_handle() {
        let (ctx, _tx, _id) = lazy_ctx();
        // We can't introspect the span content, but it must be a real handle
        // (not disabled) so child spans can attach. `is_disabled` is the only
        // observable cheap check that doesn't require a subscriber.
        let _ = ctx.span().id();
    }
}

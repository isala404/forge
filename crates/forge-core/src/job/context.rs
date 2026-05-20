use std::sync::{Arc, mpsc};
use std::time::Duration;

use uuid::Uuid;

use serde::Serialize;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::{AuthContext, KvHandle};
use crate::http::CircuitBreakerClient;

/// Returns an empty JSON object for initializing job saved data.
pub fn empty_saved_data() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Context available to job handlers.
#[non_exhaustive]
pub struct JobContext {
    /// Job ID.
    pub job_id: Uuid,
    /// Job type/name.
    pub job_type: String,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Maximum attempts allowed.
    pub max_attempts: u32,
    /// Authentication context (for queries/mutations).
    pub auth: AuthContext,
    /// Persisted job data (survives retries, accessible during compensation).
    saved_data: Arc<tokio::sync::RwLock<serde_json::Value>>,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client for external calls.
    http_client: CircuitBreakerClient,
    /// Default timeout for outbound HTTP requests made through the
    /// circuit-breaker client. `None` means unlimited.
    http_timeout: Option<Duration>,
    /// Progress reporter (sync channel for simplicity).
    progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
    /// KV store handle. `None` until threaded in by the runtime.
    kv: Option<Arc<dyn KvHandle>>,
}

/// Progress update message.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    /// Job ID.
    pub job_id: Uuid,
    /// Progress percentage (0-100).
    pub percentage: u8,
    /// Status message.
    pub message: String,
}

impl JobContext {
    /// Create a new job context.
    pub fn new(
        job_id: Uuid,
        job_type: String,
        attempt: u32,
        max_attempts: u32,
        db_pool: sqlx::PgPool,
        http_client: CircuitBreakerClient,
    ) -> Self {
        Self {
            job_id,
            job_type,
            attempt,
            max_attempts,
            auth: AuthContext::unauthenticated(),
            saved_data: Arc::new(tokio::sync::RwLock::new(empty_saved_data())),
            db_pool,
            http_client,
            http_timeout: None,
            progress_tx: None,
            env_provider: Arc::new(RealEnvProvider::new()),
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

    /// Create a new job context with persisted saved data.
    pub fn with_saved(mut self, data: serde_json::Value) -> Self {
        self.saved_data = Arc::new(tokio::sync::RwLock::new(data));
        self
    }

    /// Set authentication context.
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }

    /// Inject a tenant ID into the auth context claims.
    ///
    /// Merges the `tenant_id` claim into the existing auth context so that
    /// `ctx.auth.tenant_id()` returns the value for the duration of this job.
    /// Used by the executor when the job record carries a tenant ID.
    pub fn with_tenant_id(mut self, tenant_id: Uuid) -> Self {
        let mut claims = self.auth.claims().clone();
        claims.insert(
            "tenant_id".to_string(),
            serde_json::Value::String(tenant_id.to_string()),
        );
        self.auth = if self.auth.is_authenticated() {
            if let Some(user_id) = self.auth.user_id() {
                AuthContext::authenticated(user_id, self.auth.roles().to_vec(), claims)
            } else {
                AuthContext::authenticated_without_uuid(self.auth.roles().to_vec(), claims)
            }
        } else {
            // Unauthenticated job — still carry the tenant_id for scoping.
            AuthContext::authenticated_without_uuid(Vec::new(), claims)
        };
        self
    }

    /// Set progress channel.
    pub fn with_progress(mut self, tx: mpsc::Sender<ProgressUpdate>) -> Self {
        self.progress_tx = Some(tx);
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

    pub fn set_http_timeout(&mut self, timeout: Option<Duration>) {
        self.http_timeout = timeout;
    }

    /// Get the raw database pool for bridge handlers that need to construct
    /// other context types (e.g., CronContext from a cron bridge job).
    ///
    /// Not intended for use in application job handlers. Use `db()`, `db_conn()`,
    /// or `conn()` instead. This exists so bridge handlers (cron-to-job,
    /// workflow-to-job) can construct sub-contexts that require a pool reference.
    #[doc(hidden)]
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the circuit breaker HTTP client for bridge handlers that need to
    /// construct sub-contexts (e.g., CronContext from a cron bridge job).
    ///
    /// Not intended for use in application job handlers. Use `http()` or
    /// `raw_http()` instead.
    #[doc(hidden)]
    pub fn circuit_breaker_client(&self) -> &CircuitBreakerClient {
        &self.http_client
    }

    /// Get the KV handle for bridge handlers that need to propagate it to
    /// sub-contexts (e.g., CronContext from a cron bridge job).
    ///
    /// Not intended for use in application job handlers. Use `kv()` instead.
    #[doc(hidden)]
    pub fn kv_handle(&self) -> Option<Arc<dyn KvHandle>> {
        self.kv.clone()
    }

    /// Report job progress.
    pub fn progress(&self, percentage: u8, message: impl Into<String>) -> crate::Result<()> {
        let update = ProgressUpdate {
            job_id: self.job_id,
            percentage: percentage.min(100),
            message: message.into(),
        };

        if let Some(ref tx) = self.progress_tx {
            tx.send(update)
                .map_err(|e| crate::ForgeError::internal(format!("Failed to send progress: {e}")))?;
        }

        Ok(())
    }

    /// Get all saved job data.
    ///
    /// Returns data that was saved during job execution via `save()`.
    /// This data persists across retries and is accessible in compensation handlers.
    pub async fn saved(&self) -> serde_json::Value {
        self.saved_data.read().await.clone()
    }

    /// Save a key-value pair to persistent job data.
    ///
    /// Merges `key` into the saved data object and persists the result to the
    /// database. Saved data survives retries and is accessible in compensation
    /// handlers. Use this to store information needed for rollback (e.g.,
    /// transaction IDs, resource handles, progress markers).
    ///
    /// Read saved data back with [`saved()`](Self::saved).
    ///
    /// # Example
    ///
    /// ```ignore
    /// ctx.save("charge_id", json!(charge.id)).await?;
    /// ctx.save("refund_amount", json!(amount)).await?;
    /// ```
    pub async fn save(&self, key: &str, value: serde_json::Value) -> crate::Result<()> {
        let mut guard = self.saved_data.write().await;
        Self::apply_save(&mut guard, key, value);
        let persisted = Self::clone_and_drop(guard);
        if self.job_id.is_nil() {
            return Ok(());
        }
        self.persist_saved_data(persisted).await
    }

    /// Dispatch a sub-job directly.
    ///
    /// The job is inserted into the queue immediately. If the parent job
    /// subsequently fails, the sub-job remains enqueued (fire-and-forget
    /// semantics). Use transactional dispatch in MutationContext for
    /// commit-dependent dispatch.
    pub async fn dispatch_job<T: Serialize>(
        &self,
        job_type: &str,
        args: &T,
    ) -> crate::Result<Uuid> {
        let id = Uuid::new_v4();
        let args_json = serde_json::to_value(args)
            .map_err(|e| crate::ForgeError::Serialization(e.to_string()))?;

        // Runtime query: these INSERTs are new in the direct-dispatch refactor
        // and the .sqlx cache hasn't been regenerated yet. Convert to compile-time
        // query! after next `cargo sqlx prepare`.
        #[allow(clippy::disallowed_methods)]
        sqlx::query(
            r#"
            INSERT INTO forge_jobs (
                id, job_type, input, job_context, status, priority, attempts, max_attempts,
                owner_subject, scheduled_at, created_at
            ) VALUES ($1, $2, $3, '{}', 'pending', 50, 0, 3, $4, NOW(), NOW())
            "#,
        )
        .bind(id)
        .bind(job_type)
        .bind(&args_json)
        .bind(self.auth.subject())
        .execute(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        Ok(id)
    }

    /// Type-safe dispatch: resolves the job name from the type's `ForgeJob`
    /// impl and serializes the args at the call site.
    pub async fn dispatch<J: crate::ForgeJob>(&self, args: &J::Args) -> crate::Result<Uuid> {
        self.dispatch_job(J::info().name, args).await
    }

    /// Start a workflow directly.
    ///
    /// Inserts the workflow run row and a `$workflow_resume` job immediately.
    /// The workflow will be picked up by the worker pool.
    pub async fn start_workflow<T: Serialize>(
        &self,
        workflow_name: &str,
        args: &T,
    ) -> crate::Result<Uuid> {
        let id = Uuid::new_v4();
        let input_json = serde_json::to_value(args)
            .map_err(|e| crate::ForgeError::Serialization(e.to_string()))?;

        // Runtime query: convert to query! after next `cargo sqlx prepare`.
        #[allow(clippy::disallowed_methods)]
        sqlx::query(
            r#"
            INSERT INTO forge_workflow_runs (
                id, workflow_name, workflow_version, workflow_signature,
                input, status, owner_subject, started_at
            ) VALUES ($1, $2, '', '', $3, 'pending', $4, NOW())
            "#,
        )
        .bind(id)
        .bind(workflow_name)
        .bind(&input_json)
        .bind(self.auth.subject())
        .execute(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        // Enqueue resume job so the worker picks it up
        let resume_input = serde_json::json!({
            "run_id": id.to_string(),
            "from_sleep": false,
        });
        // Runtime query: convert to query! after next `cargo sqlx prepare`.
        #[allow(clippy::disallowed_methods)]
        sqlx::query(
            r#"
            INSERT INTO forge_jobs (
                id, job_type, input, job_context, status, priority, attempts, max_attempts,
                worker_capability, scheduled_at, created_at
            ) VALUES (gen_random_uuid(), '$workflow_resume', $1, '{}', 'pending', 75, 0, 3, 'workflows', NOW(), NOW())
            "#,
        )
        .bind(&resume_input)
        .execute(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        Ok(id)
    }

    /// Check if cancellation has been requested for this job.
    pub async fn is_cancel_requested(&self) -> crate::Result<bool> {
        let row = sqlx::query_scalar!(
            r#"
            SELECT status
            FROM forge_jobs
            WHERE id = $1
            "#,
            self.job_id
        )
        .fetch_optional(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        Ok(matches!(
            row.as_deref(),
            Some("cancel_requested") | Some("cancelled")
        ))
    }

    /// Return an error if cancellation has been requested.
    pub async fn check_cancelled(&self) -> crate::Result<()> {
        if self.is_cancel_requested().await? {
            Err(crate::ForgeError::JobCancelled(
                "Job cancellation requested".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    async fn persist_saved_data(&self, data: serde_json::Value) -> crate::Result<()> {
        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET job_context = $2
            WHERE id = $1
            "#,
            self.job_id,
            data,
        )
        .execute(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        Ok(())
    }

    fn apply_save(data: &mut serde_json::Value, key: &str, value: serde_json::Value) {
        if let Some(map) = data.as_object_mut() {
            map.insert(key.to_string(), value);
        } else {
            let mut map = serde_json::Map::new();
            map.insert(key.to_string(), value);
            *data = serde_json::Value::Object(map);
        }
    }

    fn clone_and_drop(
        guard: tokio::sync::RwLockWriteGuard<'_, serde_json::Value>,
    ) -> serde_json::Value {
        let cloned = guard.clone();
        drop(guard);
        cloned
    }

    /// Send heartbeat to keep job alive (async).
    pub async fn heartbeat(&self) -> crate::Result<()> {
        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET last_heartbeat = NOW()
            WHERE id = $1
            "#,
            self.job_id,
        )
        .execute(&self.db_pool)
        .await
        .map_err(crate::ForgeError::Database)?;

        Ok(())
    }

    /// Check if this is a retry attempt.
    pub fn is_retry(&self) -> bool {
        self.attempt > 1
    }

    /// Check if this is the last attempt.
    pub fn is_last_attempt(&self) -> bool {
        self.attempt >= self.max_attempts
    }
}

impl EnvAccess for JobContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_job_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let job_id = Uuid::new_v4();
        let ctx = JobContext::new(
            job_id,
            "test_job".to_string(),
            1,
            3,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        );

        assert_eq!(ctx.job_id, job_id);
        assert_eq!(ctx.job_type, "test_job");
        assert_eq!(ctx.attempt, 1);
        assert_eq!(ctx.max_attempts, 3);
        assert!(!ctx.is_retry());
        assert!(!ctx.is_last_attempt());
    }

    #[tokio::test]
    async fn test_is_retry() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::new_v4(),
            "test".to_string(),
            2,
            3,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        );

        assert!(ctx.is_retry());
    }

    #[tokio::test]
    async fn test_is_last_attempt() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::new_v4(),
            "test".to_string(),
            3,
            3,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        );

        assert!(ctx.is_last_attempt());
    }

    #[test]
    fn test_progress_update() {
        let update = ProgressUpdate {
            job_id: Uuid::new_v4(),
            percentage: 50,
            message: "Halfway there".to_string(),
        };

        assert_eq!(update.percentage, 50);
        assert_eq!(update.message, "Halfway there");
    }

    #[tokio::test]
    async fn test_saved_data_in_memory() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::nil(),
            "test_job".to_string(),
            1,
            3,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        )
        .with_saved(serde_json::json!({"foo": "bar"}));

        let saved = ctx.saved().await;
        assert_eq!(saved["foo"], "bar");
    }

    #[tokio::test]
    async fn test_save_key_value() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::nil(),
            "test_job".to_string(),
            1,
            3,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        );

        ctx.save("charge_id", serde_json::json!("ch_123"))
            .await
            .unwrap();
        ctx.save("amount", serde_json::json!(100)).await.unwrap();

        let saved = ctx.saved().await;
        assert_eq!(saved["charge_id"], "ch_123");
        assert_eq!(saved["amount"], 100);
    }

    fn mock_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool")
    }

    fn nil_ctx() -> JobContext {
        JobContext::new(
            Uuid::nil(),
            "test_job".to_string(),
            1,
            3,
            mock_pool(),
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        )
    }

    #[test]
    fn empty_saved_data_is_an_empty_object() {
        let data = empty_saved_data();
        let obj = data.as_object().expect("empty_saved_data is an object");
        assert!(obj.is_empty());
    }

    #[tokio::test]
    async fn progress_without_channel_is_a_noop() {
        let ctx = nil_ctx();
        // No `.with_progress(...)` was attached — progress should succeed silently.
        ctx.progress(42, "boot").expect("noop progress should not error");
    }

    #[tokio::test]
    async fn progress_clamps_percentage_to_100() {
        let (tx, rx) = mpsc::channel();
        let ctx = nil_ctx().with_progress(tx);
        ctx.progress(250, "over").expect("send should succeed");
        let update = rx.recv().expect("update available");
        assert_eq!(update.percentage, 100);
        assert_eq!(update.message, "over");
        assert_eq!(update.job_id, ctx.job_id);
    }

    #[tokio::test]
    async fn progress_returns_job_error_when_receiver_dropped() {
        let (tx, rx) = mpsc::channel::<ProgressUpdate>();
        drop(rx);
        let ctx = nil_ctx().with_progress(tx);
        let err = ctx
            .progress(10, "lost")
            .expect_err("dropped receiver should fail send");
        match err {
            crate::ForgeError::Internal { context: msg, .. } => {
                assert!(msg.contains("Failed to send progress"), "got: {msg}");
            }
            other => panic!("expected ForgeError::Internal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn with_auth_threads_authenticated_principal() {
        let user = Uuid::new_v4();
        let ctx = nil_ctx().with_auth(AuthContext::authenticated(
            user,
            vec!["admin".to_string()],
            Default::default(),
        ));
        assert_eq!(ctx.auth.user_id(), Some(user));
        assert!(ctx.auth.has_role("admin"));
    }

    #[tokio::test]
    async fn with_env_provider_reaches_through_env_access_trait() {
        use crate::env::MockEnvProvider;
        let mut mock = MockEnvProvider::new();
        mock.set("API_KEY", "sk_test");
        let ctx = nil_ctx().with_env_provider(Arc::new(mock));

        assert_eq!(ctx.env("API_KEY"), Some("sk_test".to_string()));
        assert!(ctx.env("MISSING").is_none());
    }

    #[tokio::test]
    async fn save_promotes_non_object_value_into_object() {
        // If saved data is somehow not an object (e.g., legacy nullable column),
        // save() must replace it with an object containing the new key rather
        // than silently dropping the write.
        let ctx = nil_ctx().with_saved(serde_json::Value::Null);
        ctx.save("charge", serde_json::json!("ch_1"))
            .await
            .expect("save coerces non-object data");

        let saved = ctx.saved().await;
        assert!(saved.is_object(), "saved should be an object after save()");
        assert_eq!(saved["charge"], "ch_1");
    }

    #[test]
    fn progress_update_carries_job_id_percentage_and_message() {
        let id = Uuid::new_v4();
        let update = ProgressUpdate {
            job_id: id,
            percentage: 75,
            message: "almost there".to_string(),
        };
        assert_eq!(update.job_id, id);
        assert_eq!(update.percentage, 75);
        assert_eq!(update.message, "almost there");
    }
}

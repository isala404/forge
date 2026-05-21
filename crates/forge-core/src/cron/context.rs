use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::Span;
use uuid::Uuid;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::{AuthContext, KvHandle};
use crate::http::CircuitBreakerClient;

/// Context available to cron handlers.
#[non_exhaustive]
pub struct CronContext {
    pub run_id: Uuid,
    pub cron_name: String,
    pub scheduled_time: DateTime<Utc>,
    pub execution_time: DateTime<Utc>,
    pub timezone: String,
    pub is_catch_up: bool,
    pub auth: AuthContext,
    db_pool: sqlx::PgPool,
    http_client: CircuitBreakerClient,
    /// `None` means unlimited.
    http_timeout: Option<Duration>,
    env_provider: Arc<dyn EnvProvider>,
    span: Span,
    kv: Option<Arc<dyn KvHandle>>,
}

impl CronContext {
    /// Create a new cron context.
    pub fn new(
        run_id: Uuid,
        cron_name: impl Into<String>,
        scheduled_time: DateTime<Utc>,
        timezone: String,
        is_catch_up: bool,
        db_pool: sqlx::PgPool,
        http_client: CircuitBreakerClient,
    ) -> Self {
        let cron_name = cron_name.into();
        Self {
            run_id,
            cron_name,
            scheduled_time,
            execution_time: Utc::now(),
            timezone,
            is_catch_up,
            auth: AuthContext::unauthenticated(),
            db_pool,
            http_client,
            http_timeout: None,
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

    /// Get the delay between scheduled and actual execution time.
    pub fn delay(&self) -> chrono::Duration {
        self.execution_time - self.scheduled_time
    }

    /// Check if the cron is running late (more than 1 minute delay).
    pub fn is_late(&self) -> bool {
        self.delay() > chrono::Duration::minutes(1)
    }

    /// Set authentication context.
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }

    /// Get the trace ID for this cron execution.
    ///
    /// Returns the trace ID if OpenTelemetry is configured, otherwise returns the run_id.
    pub fn trace_id(&self) -> String {
        // The span carries the trace context. When OTel is configured,
        // the trace ID can be extracted from the span context.
        // For now, return the run_id as a fallback correlation ID.
        self.run_id.to_string()
    }

    /// Get the parent span for trace propagation.
    ///
    /// Use this to create child spans within cron handlers.
    pub fn span(&self) -> &Span {
        &self.span
    }
}

impl EnvAccess for CronContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::env::MockEnvProvider;

    fn make_ctx(scheduled: DateTime<Utc>, is_catch_up: bool) -> CronContext {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");
        CronContext::new(
            Uuid::new_v4(),
            "test_cron".to_string(),
            scheduled,
            "UTC".to_string(),
            is_catch_up,
            pool,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        )
    }

    #[tokio::test]
    async fn test_cron_context_creation() {
        let scheduled = Utc::now() - chrono::Duration::seconds(30);
        let ctx = make_ctx(scheduled, false);

        assert_eq!(ctx.cron_name, "test_cron");
        assert!(!ctx.is_catch_up);
        // Default auth is unauthenticated.
        assert!(!ctx.auth.is_authenticated());
        // execution_time is set after scheduled, so delay must be non-negative.
        assert!(ctx.delay() >= chrono::Duration::zero());
    }

    #[tokio::test]
    async fn test_cron_delay() {
        let scheduled = Utc::now() - chrono::Duration::minutes(5);
        let ctx = make_ctx(scheduled, false);

        assert!(ctx.is_late());
        assert!(ctx.delay() >= chrono::Duration::minutes(5));
    }

    #[tokio::test]
    async fn cron_on_time_is_not_late() {
        // scheduled in the future or roughly now -> delay <= 1m, not late
        let ctx = make_ctx(Utc::now() + chrono::Duration::seconds(5), false);
        assert!(!ctx.is_late());
    }

    #[tokio::test]
    async fn cron_catch_up_flag_round_trips() {
        let ctx = make_ctx(Utc::now() - chrono::Duration::minutes(30), true);
        assert!(ctx.is_catch_up);
    }

    #[tokio::test]
    async fn cron_trace_id_returns_run_id_as_string() {
        let ctx = make_ctx(Utc::now(), false);
        assert_eq!(ctx.trace_id(), ctx.run_id.to_string());
    }

    #[tokio::test]
    async fn cron_with_auth_replaces_default() {
        use std::collections::HashMap;
        let uid = Uuid::new_v4();
        let auth = AuthContext::authenticated(uid, vec!["admin".to_string()], HashMap::new());
        let ctx = make_ctx(Utc::now(), false).with_auth(auth);
        assert!(ctx.auth.is_authenticated());
        assert!(ctx.auth.has_role("admin"));
    }

    #[tokio::test]
    async fn cron_with_env_provider_overrides_real() {
        let mut mock = MockEnvProvider::new();
        mock.set("FORGE_CRON_KEY", "v");
        let ctx = make_ctx(Utc::now(), false).with_env_provider(Arc::new(mock));
        use crate::env::EnvAccess;
        assert_eq!(ctx.env("FORGE_CRON_KEY"), Some("v".to_string()));
        assert_eq!(ctx.env("FORGE_MISSING"), None);
    }

    #[tokio::test]
    async fn cron_set_http_timeout_does_not_panic() {
        let mut ctx = make_ctx(Utc::now(), false);
        ctx.set_http_timeout(Some(Duration::from_millis(100)));
        let _ = ctx.http();
        ctx.set_http_timeout(None);
        let _ = ctx.http();
    }
}

//! Execution contexts for queries and mutations.
//!
//! Every function receives a context providing access to:
//!
//! - Database connection (pool or transaction)
//! - Authentication state (user ID, roles, claims)
//! - Request metadata (request ID, trace ID, client IP)
//! - Environment variables
//! - Job/workflow dispatch (mutations only)
//!
//! # QueryContext vs MutationContext
//!
//! | Feature | QueryContext | MutationContext |
//! |---------|--------------|-----------------|
//! | Database | Pool (read-only) | Transaction or pool |
//! | Dispatch jobs | No | Yes |
//! | Start workflows | No | Yes |
//! | HTTP client | No | Yes (circuit breaker) |
//!
//! # Transactional Mutations
//!
//! When `transactional = true` (default), mutations run in a transaction.
//! Jobs and workflows dispatched during the mutation insert their rows on
//! the same transaction, so they only become visible to workers once the
//! mutation commits and are rolled back if it fails.
//!
//! ```text
//! BEGIN
//!   ├── ctx.tx().execute(...)
//!   ├── ctx.dispatch_job("send_email", ...)  // INSERT into forge_jobs on this tx
//!   └── return Ok(result)
//! COMMIT
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use sqlx::postgres::{PgConnection, PgQueryResult, PgRow};
use sqlx::{Postgres, Transaction};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use tracing::Instrument;

use super::dispatch::{JobDispatch, KvHandle, WorkflowDispatch};
use crate::auth::Claims;
use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::http::CircuitBreakerClient;

/// Token issuer for signing JWTs.
///
/// Implemented by the runtime when HMAC auth is configured.
/// Available via `ctx.issue_token()` in mutation handlers.
pub trait TokenIssuer: Send + Sync {
    /// Sign the given claims into a JWT string.
    fn sign(&self, claims: &Claims) -> crate::error::Result<String>;
}

/// Connection wrapper that implements sqlx's `Executor` trait with automatic
/// `db.query` tracing spans.
///
/// Obtain via `ctx.conn().await?` in mutation handlers.
/// Works with compile-time checked macros via `&mut conn`.
///
/// ```ignore
/// let mut conn = ctx.conn().await?;
/// sqlx::query_as!(User, "SELECT * FROM users WHERE id = $1", id)
///     .fetch_one(&mut *conn)
///     .await?
/// ```
pub enum ForgeConn<'a> {
    Pool(sqlx::pool::PoolConnection<Postgres>),
    Tx(tokio::sync::MutexGuard<'a, Option<Transaction<'static, Postgres>>>),
}

impl std::ops::Deref for ForgeConn<'_> {
    type Target = PgConnection;
    fn deref(&self) -> &PgConnection {
        match self {
            ForgeConn::Pool(c) => c,
            ForgeConn::Tx(g) => g
                .as_ref()
                .expect("ForgeConn::Tx held while transaction was already taken"),
        }
    }
}

impl std::ops::DerefMut for ForgeConn<'_> {
    fn deref_mut(&mut self) -> &mut PgConnection {
        match self {
            ForgeConn::Pool(c) => c,
            ForgeConn::Tx(g) => g
                .as_mut()
                .expect("ForgeConn::Tx held while transaction was already taken"),
        }
    }
}

/// Pool wrapper that adds `db.query` tracing spans to every database operation.
///
/// Returned by [`QueryContext::db()`]. Implements sqlx's [`sqlx::Executor`] trait,
/// so it works as a drop-in replacement for `&PgPool` with compile-time
/// checked macros (`query!`, `query_as!`).
///
/// ```ignore
/// sqlx::query_as!(User, "SELECT * FROM users")
///     .fetch_all(ctx.db())
///     .await?
/// ```
#[derive(Clone)]
pub struct ForgeDb(sqlx::PgPool);

impl std::fmt::Debug for ForgeDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ForgeDb").finish()
    }
}

impl ForgeDb {
    /// Create a `ForgeDb` from a pool reference. Clones the Arc-backed pool handle.
    pub fn from_pool(pool: &sqlx::PgPool) -> Self {
        Self(pool.clone())
    }
}

fn sql_operation(sql: &str) -> &'static str {
    let bytes = sql.trim_start().as_bytes();
    match bytes.get(..6) {
        Some(prefix) if prefix.eq_ignore_ascii_case(b"select") => "SELECT",
        Some(prefix) if prefix.eq_ignore_ascii_case(b"insert") => "INSERT",
        Some(prefix) if prefix.eq_ignore_ascii_case(b"update") => "UPDATE",
        Some(prefix) if prefix.eq_ignore_ascii_case(b"delete") => "DELETE",
        _ => "OTHER",
    }
}

impl sqlx::Executor<'static> for ForgeDb {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<'e, Result<sqlx::Either<PgQueryResult, PgRow>, sqlx::Error>>
    where
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        (&self.0).fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<PgRow>, sqlx::Error>>
    where
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        Box::pin(
            async move { sqlx::Executor::fetch_optional(&self.0, query).await }.instrument(span),
        )
    }

    fn execute<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<PgQueryResult, sqlx::Error>>
    where
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        Box::pin(async move { sqlx::Executor::execute(&self.0, query).await }.instrument(span))
    }

    fn fetch_all<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<Vec<PgRow>, sqlx::Error>>
    where
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        Box::pin(async move { sqlx::Executor::fetch_all(&self.0, query).await }.instrument(span))
    }

    fn fetch_one<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<PgRow, sqlx::Error>>
    where
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        Box::pin(async move { sqlx::Executor::fetch_one(&self.0, query).await }.instrument(span))
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<Postgres as sqlx::Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<Postgres as sqlx::Database>::Statement<'q>, sqlx::Error>> {
        Box::pin(async move { sqlx::Executor::prepare_with(&self.0, sql, parameters).await })
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, Result<sqlx::Describe<Postgres>, sqlx::Error>> {
        Box::pin(async move { sqlx::Executor::describe(&self.0, sql).await })
    }
}

/// Abstraction over pool and transaction connections.
///
/// Allows shared helper functions to work with any context type.
/// Obtain via `ctx.db_conn()` on pool-based contexts (queries, jobs, crons,
/// daemons, webhooks, MCP tools) or via `ctx.tx()` on `MutationContext`.
///
/// # Example
///
/// ```ignore
/// pub async fn list_items(db: DbConn<'_>) -> Result<Vec<Item>> {
///     db.fetch_all(sqlx::query_as!(Item, "SELECT * FROM items ORDER BY created_at DESC"))
///         .await
///         .map_err(Into::into)
/// }
/// ```
#[non_exhaustive]
pub enum DbConn<'a> {
    /// Direct pool connection (queries, jobs, crons, daemons, webhooks, MCP).
    Pool(sqlx::PgPool),
    /// Transaction handle (transactional mutations).
    Transaction(
        Arc<AsyncMutex<Option<Transaction<'static, Postgres>>>>,
        &'a sqlx::PgPool,
    ),
}

impl DbConn<'_> {
    /// Fetch exactly one row.
    pub async fn fetch_one<'q, O>(
        &self,
        query: sqlx::query::QueryAs<'q, Postgres, O, sqlx::postgres::PgArguments>,
    ) -> sqlx::Result<O>
    where
        O: Send + Unpin + for<'r> sqlx::FromRow<'r, PgRow>,
    {
        match self {
            DbConn::Pool(pool) => query.fetch_one(pool).await,
            DbConn::Transaction(tx, _) => {
                let mut guard = tx.lock().await;
                let conn = guard.as_mut().ok_or(sqlx::Error::PoolClosed)?;
                query.fetch_one(&mut **conn).await
            }
        }
    }

    /// Fetch zero or one row.
    pub async fn fetch_optional<'q, O>(
        &self,
        query: sqlx::query::QueryAs<'q, Postgres, O, sqlx::postgres::PgArguments>,
    ) -> sqlx::Result<Option<O>>
    where
        O: Send + Unpin + for<'r> sqlx::FromRow<'r, PgRow>,
    {
        match self {
            DbConn::Pool(pool) => query.fetch_optional(pool).await,
            DbConn::Transaction(tx, _) => {
                let mut guard = tx.lock().await;
                let conn = guard.as_mut().ok_or(sqlx::Error::PoolClosed)?;
                query.fetch_optional(&mut **conn).await
            }
        }
    }

    /// Fetch all matching rows.
    pub async fn fetch_all<'q, O>(
        &self,
        query: sqlx::query::QueryAs<'q, Postgres, O, sqlx::postgres::PgArguments>,
    ) -> sqlx::Result<Vec<O>>
    where
        O: Send + Unpin + for<'r> sqlx::FromRow<'r, PgRow>,
    {
        match self {
            DbConn::Pool(pool) => query.fetch_all(pool).await,
            DbConn::Transaction(tx, _) => {
                let mut guard = tx.lock().await;
                let conn = guard.as_mut().ok_or(sqlx::Error::PoolClosed)?;
                query.fetch_all(&mut **conn).await
            }
        }
    }

    /// Execute a statement (INSERT, UPDATE, DELETE).
    pub async fn execute<'q>(
        &self,
        query: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::Result<PgQueryResult> {
        match self {
            DbConn::Pool(pool) => query.execute(pool).await,
            DbConn::Transaction(tx, _) => {
                let mut guard = tx.lock().await;
                let conn = guard.as_mut().ok_or(sqlx::Error::PoolClosed)?;
                query.execute(&mut **conn).await
            }
        }
    }
}

impl std::fmt::Debug for DbConn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbConn::Pool(_) => f.debug_tuple("DbConn::Pool").finish(),
            DbConn::Transaction(_, _) => f.debug_tuple("DbConn::Transaction").finish(),
        }
    }
}

impl std::fmt::Debug for ForgeConn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeConn::Pool(_) => f.debug_tuple("ForgeConn::Pool").finish(),
            ForgeConn::Tx(_) => f.debug_tuple("ForgeConn::Tx").finish(),
        }
    }
}

impl<'c> sqlx::Executor<'c> for &'c mut ForgeConn<'_> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<'e, Result<sqlx::Either<PgQueryResult, PgRow>, sqlx::Error>>
    where
        'c: 'e,
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let conn: &'e mut PgConnection = &mut *self;
        conn.fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<PgRow>, sqlx::Error>>
    where
        'c: 'e,
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        let conn: &'e mut PgConnection = &mut *self;
        Box::pin(conn.fetch_optional(query).instrument(span))
    }

    fn execute<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<PgQueryResult, sqlx::Error>>
    where
        'c: 'e,
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        let conn: &'e mut PgConnection = &mut *self;
        Box::pin(conn.execute(query).instrument(span))
    }

    fn fetch_all<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<Vec<PgRow>, sqlx::Error>>
    where
        'c: 'e,
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        let conn: &'e mut PgConnection = &mut *self;
        Box::pin(conn.fetch_all(query).instrument(span))
    }

    fn fetch_one<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Result<PgRow, sqlx::Error>>
    where
        'c: 'e,
        E: sqlx::Execute<'q, Postgres> + 'q,
    {
        let op = sql_operation(query.sql());
        let span =
            tracing::info_span!("db.query", db.system = "postgresql", db.operation.name = op,);
        let conn: &'e mut PgConnection = &mut *self;
        Box::pin(conn.fetch_one(query).instrument(span))
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<Postgres as sqlx::Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<Postgres as sqlx::Database>::Statement<'q>, sqlx::Error>>
    where
        'c: 'e,
    {
        let conn: &'e mut PgConnection = &mut *self;
        conn.prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, Result<sqlx::Describe<Postgres>, sqlx::Error>>
    where
        'c: 'e,
    {
        let conn: &'e mut PgConnection = &mut *self;
        conn.describe(sql)
    }
}

/// Authentication context available to all functions.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuthContext {
    /// The authenticated user ID (if any).
    user_id: Option<Uuid>,
    /// User roles.
    roles: Vec<String>,
    /// Custom claims from JWT.
    claims: HashMap<String, serde_json::Value>,
    /// Whether the request is authenticated.
    authenticated: bool,
    /// JWT expiry as Unix timestamp (`exp` claim). `None` for unauthenticated
    /// sessions or when no JWT was presented.
    token_exp: Option<i64>,
}

impl AuthContext {
    /// Create an unauthenticated context.
    pub fn unauthenticated() -> Self {
        Self {
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            authenticated: false,
            token_exp: None,
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
            token_exp: None,
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
            token_exp: None,
        }
    }

    /// Attach the JWT expiry timestamp to this context.
    ///
    /// Called by the auth middleware immediately after building the context so
    /// downstream SSE session tracking can evict sessions when their token expires.
    pub fn with_token_exp(mut self, exp: i64) -> Self {
        self.token_exp = Some(exp);
        self
    }

    /// Return the JWT expiry as a Unix timestamp, if available.
    pub fn token_exp(&self) -> Option<i64> {
        self.token_exp
    }

    /// Check whether the JWT this context was built from has expired.
    ///
    /// Returns `false` for unauthenticated sessions (no token → never expires).
    pub fn token_is_expired(&self) -> bool {
        self.token_exp
            .map(|exp| exp < chrono::Utc::now().timestamp())
            .unwrap_or(false)
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

    /// Get all custom claims.
    pub fn claims(&self) -> &HashMap<String, serde_json::Value> {
        &self.claims
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

    /// Get a stable principal identifier for access control and cache scoping.
    ///
    /// Prefers the raw JWT `sub` claim and falls back to UUID user_id.
    pub fn principal_id(&self) -> Option<String> {
        self.subject()
            .map(ToString::to_string)
            .or_else(|| self.user_id.map(|id| id.to_string()))
    }

    /// Check whether this principal should be treated as privileged admin.
    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }

    /// Get the tenant ID from the JWT claims, if present.
    ///
    /// Looks for a `tenant_id` claim in the token and attempts to parse it as
    /// a UUID. Returns `None` if the claim is absent or not a valid UUID.
    pub fn tenant_id(&self) -> Option<uuid::Uuid> {
        self.claims
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
    }
}

/// Request metadata available to all functions.
///
/// Fields are crate-private; use the accessor methods. Construct via
/// [`RequestMetadata::new`] / [`RequestMetadata::with_trace_id`] and
/// populate optional fields with the fluent setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RequestMetadata {
    /// Unique request ID for tracing.
    pub(crate) request_id: Uuid,
    /// Trace ID for distributed tracing.
    pub(crate) trace_id: String,
    /// Client IP address.
    pub(crate) client_ip: Option<String>,
    /// User agent string.
    pub(crate) user_agent: Option<String>,
    /// Correlation ID linking frontend events to this backend call.
    pub(crate) correlation_id: Option<String>,
    /// Request timestamp.
    pub(crate) timestamp: chrono::DateTime<chrono::Utc>,
}

impl RequestMetadata {
    /// Create new request metadata.
    pub fn new() -> Self {
        Self {
            request_id: Uuid::new_v4(),
            trace_id: Uuid::new_v4().to_string(),
            client_ip: None,
            user_agent: None,
            correlation_id: None,
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
            correlation_id: None,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Build request metadata from gateway-extracted parts.
    ///
    /// Hidden from docs because this is a framework-internal constructor used by
    /// `forge-runtime` to assemble metadata from raw HTTP parts. User code should
    /// use [`RequestMetadata::new`] or [`RequestMetadata::with_trace_id`] together
    /// with the fluent setters.
    #[doc(hidden)]
    pub fn __build_internal(
        request_id: Uuid,
        trace_id: String,
        client_ip: Option<String>,
        user_agent: Option<String>,
        correlation_id: Option<String>,
    ) -> Self {
        Self {
            request_id,
            trace_id,
            client_ip,
            user_agent,
            correlation_id,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Set the client IP.
    pub fn set_client_ip(&mut self, ip: Option<String>) {
        self.client_ip = ip;
    }

    /// Set the user-agent string.
    pub fn set_user_agent(&mut self, ua: Option<String>) {
        self.user_agent = ua;
    }

    /// Set the correlation ID.
    pub fn set_correlation_id(&mut self, id: Option<String>) {
        self.correlation_id = id;
    }

    /// Get the unique request ID.
    pub fn request_id(&self) -> Uuid {
        self.request_id
    }

    /// Get the distributed-tracing trace ID.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Get the client IP, if known.
    pub fn client_ip(&self) -> Option<&str> {
        self.client_ip.as_deref()
    }

    /// Get the user-agent string, if any.
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }

    /// Get the frontend-supplied correlation ID, if any.
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    /// Get the request timestamp.
    pub fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }
}

impl Default for RequestMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Context for query functions (read-only database access).
#[non_exhaustive]
pub struct QueryContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for read operations.
    db_pool: sqlx::PgPool,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
    /// KV store handle. `None` until threaded in by the runtime.
    kv: Option<Arc<dyn KvHandle>>,
}

impl QueryContext {
    /// Create a new query context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self {
            auth,
            request,
            db_pool,
            env_provider: RealEnvProvider::shared(),
            kv: None,
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
            kv: None,
        }
    }

    /// Attach a KV store handle. Called by the runtime before handing the
    /// context to the handler.
    pub fn set_kv(&mut self, kv: Arc<dyn KvHandle>) {
        self.kv = Some(kv);
    }

    /// Access the KV store.
    ///
    /// Returns an error if the runtime did not supply a KV handle (this should
    /// not happen in production; it can only occur in unit tests that construct
    /// a bare `QueryContext` without going through the runtime).
    pub fn kv(&self) -> crate::error::Result<&dyn KvHandle> {
        self.kv
            .as_deref()
            .ok_or_else(|| crate::error::ForgeError::Internal("KV store not available".into()))
    }

    /// Database handle with automatic `db.query` tracing spans.
    ///
    /// Works directly with sqlx compile-time checked macros:
    /// ```ignore
    /// sqlx::query_as!(User, "SELECT * FROM users")
    ///     .fetch_all(ctx.db())
    ///     .await?
    /// ```
    pub fn db(&self) -> ForgeDb {
        ForgeDb(self.db_pool.clone())
    }

    /// Get a `DbConn` for use in shared helper functions.
    ///
    /// Returns a pool-backed `DbConn` that can be passed to functions
    /// accepting `DbConn<'_>` for cross-context reuse.
    ///
    /// ```ignore
    /// pub async fn list_items(db: DbConn<'_>) -> Result<Vec<Item>> { ... }
    ///
    /// #[forge::query]
    /// pub async fn get_items(ctx: &QueryContext) -> Result<Vec<Item>> {
    ///     list_items(ctx.db_conn()).await
    /// }
    /// ```
    pub fn db_conn(&self) -> DbConn<'_> {
        DbConn::Pool(self.db_pool.clone())
    }

    /// Get the authenticated user's UUID. Returns 401 if not authenticated.
    pub fn user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Get the tenant ID from JWT claims, if present.
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.auth.tenant_id()
    }

    /// Look up a custom JWT claim by name. Reserved JWT claims
    /// (`iss`, `aud`, `nbf`, `jti`, `sub`, `iat`, `exp`, `roles`) are
    /// filtered out by [`AuthContext::claim`] to prevent injection via
    /// `#[serde(flatten)]`. Shortcut for `self.auth.claim(key)`.
    pub fn claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.auth.claim(key)
    }
}

impl EnvAccess for QueryContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Token TTL configuration resolved from `[auth]` in forge.toml.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuthTokenTtl {
    /// Access token lifetime in seconds (default 3600).
    pub access_token_secs: i64,
    /// Refresh token lifetime in days (default 30).
    pub refresh_token_days: i64,
}

impl AuthTokenTtl {
    /// Construct token TTLs from raw seconds and days.
    pub fn new(access_token_secs: i64, refresh_token_days: i64) -> Self {
        Self {
            access_token_secs,
            refresh_token_days,
        }
    }
}

impl Default for AuthTokenTtl {
    fn default() -> Self {
        Self {
            access_token_secs: 3600,
            refresh_token_days: 30,
        }
    }
}

/// Context for mutation functions (transactional database access).
#[non_exhaustive]
pub struct MutationContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for transactional operations.
    db_pool: sqlx::PgPool,
    /// HTTP client with circuit breaker for external requests.
    http_client: CircuitBreakerClient,
    /// Default timeout for outbound HTTP requests made through the
    /// circuit-breaker client. `None` means unlimited.
    http_timeout: Option<Duration>,
    /// Optional job dispatcher for dispatching background jobs.
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    /// Optional workflow dispatcher for starting workflows.
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
    /// Transaction handle for transactional mutations.
    ///
    /// Wrapped in `Option` so the executor can `take()` the transaction back
    /// after the handler returns without ever needing `Arc::try_unwrap`.
    tx: Option<Arc<AsyncMutex<Option<Transaction<'static, Postgres>>>>>,
    /// Optional token issuer for signing JWTs (available when HMAC auth is configured).
    token_issuer: Option<Arc<dyn TokenIssuer>>,
    /// Token TTL config from forge.toml.
    token_ttl: AuthTokenTtl,
    /// Running count of background jobs dispatched by this mutation.
    dispatched_job_count: Arc<AtomicUsize>,
    /// Maximum number of jobs a single mutation may dispatch. 0 = unlimited.
    max_jobs_per_request: usize,
    /// KV store handle. `None` until threaded in by the runtime.
    kv: Option<Arc<dyn KvHandle>>,
}

impl MutationContext {
    /// Create a new mutation context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self {
            auth,
            request,
            db_pool,
            http_client: CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            http_timeout: None,
            job_dispatch: None,
            workflow_dispatch: None,
            env_provider: RealEnvProvider::shared(),
            tx: None,
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
            dispatched_job_count: Arc::new(AtomicUsize::new(0)),
            max_jobs_per_request: 0,
            kv: None,
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
            http_timeout: None,
            job_dispatch,
            workflow_dispatch,
            env_provider: RealEnvProvider::shared(),
            tx: None,
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
            dispatched_job_count: Arc::new(AtomicUsize::new(0)),
            max_jobs_per_request: 0,
            kv: None,
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
            http_timeout: None,
            job_dispatch,
            workflow_dispatch,
            env_provider,
            tx: None,
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
            dispatched_job_count: Arc::new(AtomicUsize::new(0)),
            max_jobs_per_request: 0,
            kv: None,
        }
    }

    /// Build a transactional mutation context.
    ///
    /// Jobs/workflows dispatched through the returned context insert their
    /// rows directly on `tx`, so they commit atomically with the mutation
    /// and are rolled back on failure.
    ///
    /// The caller retains ownership of the transaction via the returned
    /// handle; commit it after the handler returns successfully.
    pub fn with_transaction(
        db_pool: sqlx::PgPool,
        tx: Transaction<'static, Postgres>,
        auth: AuthContext,
        request: RequestMetadata,
        http_client: CircuitBreakerClient,
        job_dispatch: Option<Arc<dyn JobDispatch>>,
        workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
    ) -> (
        Self,
        Arc<AsyncMutex<Option<Transaction<'static, Postgres>>>>,
    ) {
        let tx_handle = Arc::new(AsyncMutex::new(Some(tx)));

        let ctx = Self {
            auth,
            request,
            db_pool,
            http_client,
            http_timeout: None,
            job_dispatch,
            workflow_dispatch,
            env_provider: RealEnvProvider::shared(),
            tx: Some(tx_handle.clone()),
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
            dispatched_job_count: Arc::new(AtomicUsize::new(0)),
            max_jobs_per_request: 0,
            kv: None,
        };

        (ctx, tx_handle)
    }

    /// Attach a KV store handle. Called by the runtime before handing the
    /// context to the handler.
    pub fn set_kv(&mut self, kv: Arc<dyn KvHandle>) {
        self.kv = Some(kv);
    }

    /// Access the KV store.
    ///
    /// Returns an error if the runtime did not supply a KV handle (this should
    /// not happen in production; it can only occur in unit tests that construct
    /// a bare `MutationContext` without going through the runtime).
    pub fn kv(&self) -> crate::error::Result<&dyn KvHandle> {
        self.kv
            .as_deref()
            .ok_or_else(|| crate::error::ForgeError::Internal("KV store not available".into()))
    }

    pub fn is_transactional(&self) -> bool {
        self.tx.is_some()
    }

    /// Acquire a connection compatible with sqlx compile-time checked macros.
    ///
    /// In transactional mode, returns a guard over the active transaction.
    /// Otherwise acquires a fresh connection from the pool.
    ///
    /// ```ignore
    /// let mut conn = ctx.conn().await?;
    /// sqlx::query_as!(User, "INSERT INTO users ... RETURNING *", ...)
    ///     .fetch_one(&mut *conn)
    ///     .await?
    /// ```
    pub async fn conn(&self) -> sqlx::Result<ForgeConn<'_>> {
        match &self.tx {
            Some(tx) => Ok(ForgeConn::Tx(tx.lock().await)),
            None => Ok(ForgeConn::Pool(self.db_pool.acquire().await?)),
        }
    }

    /// Direct pool access that **bypasses the active transaction**.
    ///
    /// In a transactional mutation, this returns the raw [`sqlx::PgPool`] and
    /// any queries run on it execute outside the transaction — so they will
    /// not see uncommitted writes and will not be rolled back if the mutation
    /// fails. Prefer [`MutationContext::conn`] or [`MutationContext::db`] for
    /// anything that should participate in the transaction. Reach for this
    /// only for operations that fundamentally cannot run inside a transaction
    /// (e.g. `LISTEN`/`NOTIFY`, advisory locks, or background pool work).
    pub fn bypass_pool(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get a `DbConn` for use in shared helper functions.
    ///
    /// In transactional mode, returns a transaction-backed `DbConn`.
    /// Otherwise returns a pool-backed `DbConn`.
    ///
    /// ```ignore
    /// pub async fn list_items(db: DbConn<'_>) -> Result<Vec<Item>> { ... }
    ///
    /// #[forge::mutation]
    /// pub async fn items_snapshot(ctx: &MutationContext, input: Input) -> Result<Vec<Item>> {
    ///     list_items(ctx.tx()).await
    /// }
    /// ```
    pub fn tx(&self) -> DbConn<'_> {
        match &self.tx {
            Some(tx) => DbConn::Transaction(tx.clone(), &self.db_pool),
            None => DbConn::Pool(self.db_pool.clone()),
        }
    }

    /// Get a `DbConn` for use in shared helper functions (alias for `tx()`).
    pub fn db_conn(&self) -> DbConn<'_> {
        self.tx()
    }

    /// Get the HTTP client for external requests.
    ///
    /// Requests go through the circuit breaker automatically. When the handler
    /// declared an explicit `timeout`, that timeout is also applied to outbound
    /// HTTP requests unless the request overrides it.
    pub fn http(&self) -> crate::http::HttpClient {
        self.http_client.with_timeout(self.http_timeout)
    }

    /// Get the raw reqwest client, bypassing circuit breaker execution.
    pub fn raw_http(&self) -> &reqwest::Client {
        self.http_client.inner()
    }

    /// Set the default outbound HTTP request timeout for this context.
    pub fn set_http_timeout(&mut self, timeout: Option<Duration>) {
        self.http_timeout = timeout;
    }

    /// Get the authenticated user's UUID. Returns 401 if not authenticated.
    pub fn user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Get the tenant ID from JWT claims, if present.
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.auth.tenant_id()
    }

    /// Look up a custom JWT claim by name. Reserved JWT claims (`iss`,
    /// `aud`, `nbf`, `jti`, `sub`, `iat`, `exp`, `roles`) are filtered
    /// out. Shortcut for `self.auth.claim(key)`.
    pub fn claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.auth.claim(key)
    }

    /// Set the token issuer for this context.
    pub fn set_token_issuer(&mut self, issuer: Arc<dyn TokenIssuer>) {
        self.token_issuer = Some(issuer);
    }

    /// Set the token TTL configuration (from forge.toml `[auth]`).
    pub fn set_token_ttl(&mut self, ttl: AuthTokenTtl) {
        self.token_ttl = ttl;
    }

    /// Set the maximum number of background jobs this mutation may dispatch.
    /// A value of 0 disables the limit.
    pub fn set_max_jobs_per_request(&mut self, limit: usize) {
        self.max_jobs_per_request = limit;
    }

    /// Issue a signed JWT from the given claims.
    ///
    /// Only available when HMAC auth is configured in `forge.toml`.
    /// Returns an error if auth is not configured or uses an external provider (RSA/JWKS).
    ///
    /// ```ignore
    /// let claims = Claims::builder()
    ///     .user_id(user.id)
    ///     .duration_secs(7 * 24 * 3600)
    ///     .build()
    ///     .map_err(|e| ForgeError::Internal(e))?;
    ///
    /// let token = ctx.issue_token(&claims)?;
    /// ```
    pub fn issue_token(&self, claims: &Claims) -> crate::error::Result<String> {
        let issuer = self.token_issuer.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal(
                "Token issuer not available. Configure [auth] with an HMAC algorithm in forge.toml"
                    .into(),
            )
        })?;
        issuer.sign(claims)
    }

    /// Issue an access + refresh token pair for the given user.
    ///
    /// Stores the refresh token hash in `forge_refresh_tokens` and returns
    /// both tokens. Use `rotate_refresh_token()` to exchange a refresh token
    /// for a new pair, and `revoke_refresh_token()` to invalidate one.
    ///
    /// TTLs come from `[auth]` in forge.toml:
    /// - `access_token_ttl` (default "1h")
    /// - `refresh_token_ttl` (default "30d")
    pub async fn issue_token_pair(
        &self,
        user_id: Uuid,
        roles: &[&str],
    ) -> crate::error::Result<crate::auth::TokenPair> {
        let issuer = self.token_issuer.clone().ok_or_else(|| {
            crate::error::ForgeError::Internal(
                "Token issuer not available. Configure [auth] in forge.toml".into(),
            )
        })?;
        let access_ttl = self.token_ttl.access_token_secs;
        let refresh_ttl = self.token_ttl.refresh_token_days;
        crate::auth::tokens::issue_token_pair(
            &self.db_pool,
            user_id,
            roles,
            access_ttl,
            refresh_ttl,
            move |uid, r, ttl| {
                let claims = Claims::builder()
                    .subject(uid)
                    .roles(r.iter().map(|s| s.to_string()).collect())
                    .duration_secs(ttl)
                    .build()
                    .map_err(crate::error::ForgeError::Internal)?;
                issuer.sign(&claims)
            },
        )
        .await
    }

    /// Rotate a refresh token: validate the old one, issue a new pair.
    ///
    /// The old token is atomically deleted and a new access + refresh pair
    /// is returned. Fails if the token is invalid or expired.
    pub async fn rotate_refresh_token(
        &self,
        old_refresh_token: &str,
    ) -> crate::error::Result<crate::auth::TokenPair> {
        let issuer = self.token_issuer.clone().ok_or_else(|| {
            crate::error::ForgeError::Internal(
                "Token issuer not available. Configure [auth] in forge.toml".into(),
            )
        })?;
        let access_ttl = self.token_ttl.access_token_secs;
        let refresh_ttl = self.token_ttl.refresh_token_days;
        crate::auth::tokens::rotate_refresh_token(
            &self.db_pool,
            old_refresh_token,
            access_ttl,
            refresh_ttl,
            move |uid, r, ttl| {
                let claims = Claims::builder()
                    .subject(uid)
                    .roles(r.iter().map(|s| s.to_string()).collect())
                    .duration_secs(ttl)
                    .build()
                    .map_err(crate::error::ForgeError::Internal)?;
                issuer.sign(&claims)
            },
        )
        .await
    }

    /// Revoke a specific refresh token (e.g., on logout).
    pub async fn revoke_refresh_token(&self, refresh_token: &str) -> crate::error::Result<()> {
        crate::auth::tokens::revoke_refresh_token(&self.db_pool, refresh_token).await
    }

    /// Revoke all refresh tokens for a user (e.g., on password change or account deletion).
    pub async fn revoke_all_refresh_tokens(&self, user_id: Uuid) -> crate::error::Result<()> {
        crate::auth::tokens::revoke_all_refresh_tokens(&self.db_pool, user_id).await
    }

    /// Dispatch a background job.
    ///
    /// In transactional mutations the job row is inserted on the active
    /// transaction, so it only becomes visible to workers after commit.
    /// Outside a transaction the dispatcher writes through the pool directly.
    ///
    /// Returns `ForgeError::Validation` when the call would exceed the
    /// per-request job dispatch cap configured via `max_jobs_per_request`.
    pub async fn dispatch_job<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> crate::error::Result<Uuid> {
        if self.max_jobs_per_request > 0 {
            let count = self.dispatched_job_count.fetch_add(1, Ordering::Relaxed);
            if count >= self.max_jobs_per_request {
                // Undo the increment so repeated calls after the limit give a
                // consistent count rather than growing without bound.
                self.dispatched_job_count.fetch_sub(1, Ordering::Relaxed);
                return Err(crate::error::ForgeError::Validation(format!(
                    "max_jobs_per_request limit of {} exceeded",
                    self.max_jobs_per_request
                )));
            }
        }

        let args_json = serde_json::to_value(args)?;
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".into())
        })?;

        if let Some(tx) = &self.tx {
            let mut guard = tx.lock().await;
            let conn = guard.as_mut().ok_or_else(|| {
                crate::error::ForgeError::Internal(
                    "Transaction already taken; cannot dispatch job".into(),
                )
            })?;
            return dispatcher
                .dispatch_in_conn(conn, job_type, args_json, self.auth.principal_id())
                .await;
        }

        dispatcher
            .dispatch_by_name(job_type, args_json, self.auth.principal_id())
            .await
    }

    /// Dispatch a background job at a specific time.
    ///
    /// The job row is inserted immediately but workers will not pick it up
    /// until `scheduled_at` is reached. In transactional mutations the insert
    /// participates in the active transaction, so the job is only committed
    /// (and becomes schedulable) once the mutation succeeds.
    ///
    /// Returns `ForgeError::Validation` when the call would exceed the
    /// per-request job dispatch cap.
    pub async fn dispatch_job_at<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        scheduled_at: DateTime<Utc>,
    ) -> crate::error::Result<Uuid> {
        if self.max_jobs_per_request > 0 {
            let count = self.dispatched_job_count.fetch_add(1, Ordering::Relaxed);
            if count >= self.max_jobs_per_request {
                self.dispatched_job_count.fetch_sub(1, Ordering::Relaxed);
                return Err(crate::error::ForgeError::Validation(format!(
                    "max_jobs_per_request limit of {} exceeded",
                    self.max_jobs_per_request
                )));
            }
        }

        let args_json = serde_json::to_value(args)?;
        let dispatcher = self.job_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Job dispatch not available".into())
        })?;

        if let Some(tx) = &self.tx {
            let mut guard = tx.lock().await;
            let conn = guard.as_mut().ok_or_else(|| {
                crate::error::ForgeError::Internal(
                    "Transaction already taken; cannot dispatch job".into(),
                )
            })?;
            return dispatcher
                .dispatch_in_conn_at(
                    conn,
                    job_type,
                    args_json,
                    scheduled_at,
                    self.auth.principal_id(),
                )
                .await;
        }

        dispatcher
            .dispatch_by_name_at(job_type, args_json, scheduled_at, self.auth.principal_id())
            .await
    }

    /// Dispatch a background job after a delay.
    ///
    /// Equivalent to `dispatch_job_at(job_type, args, Utc::now() + delay)`.
    /// The delay is computed at call time.
    ///
    /// Returns `ForgeError::Validation` when the call would exceed the
    /// per-request job dispatch cap, or when `delay` is too large to
    /// represent as a chrono duration.
    pub async fn dispatch_job_after<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        delay: Duration,
    ) -> crate::error::Result<Uuid> {
        let scheduled_at = Utc::now()
            + chrono::Duration::from_std(delay).map_err(|_| {
                crate::error::ForgeError::InvalidArgument("delay too large".into())
            })?;
        self.dispatch_job_at(job_type, args, scheduled_at).await
    }

    /// Type-safe dispatch: resolves the job name from the type's `ForgeJob`
    /// impl and serializes the args at the call site.
    pub async fn dispatch<J: crate::ForgeJob>(&self, args: J::Args) -> crate::error::Result<Uuid> {
        self.dispatch_job(J::info().name, args).await
    }

    /// Type-safe dispatch at a specific time.
    pub async fn dispatch_at<J: crate::ForgeJob>(
        &self,
        args: J::Args,
        scheduled_at: DateTime<Utc>,
    ) -> crate::error::Result<Uuid> {
        self.dispatch_job_at(J::info().name, args, scheduled_at)
            .await
    }

    /// Type-safe dispatch after a delay.
    pub async fn dispatch_after<J: crate::ForgeJob>(
        &self,
        args: J::Args,
        delay: Duration,
    ) -> crate::error::Result<Uuid> {
        self.dispatch_job_after(J::info().name, args, delay).await
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

    /// Start a durable workflow.
    ///
    /// In transactional mutations the run row and its `$workflow_resume`
    /// job are written on the active transaction, so the worker only picks
    /// the run up after commit. Outside a transaction the dispatcher writes
    /// through the pool directly.
    pub async fn start_workflow<T: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> crate::error::Result<Uuid> {
        let input_json = serde_json::to_value(input)?;
        let dispatcher = self.workflow_dispatch.as_ref().ok_or_else(|| {
            crate::error::ForgeError::Internal("Workflow dispatch not available".into())
        })?;

        let trace_id = Some(self.request.trace_id().to_string());

        if let Some(tx) = &self.tx {
            let mut guard = tx.lock().await;
            let conn = guard.as_mut().ok_or_else(|| {
                crate::error::ForgeError::Internal(
                    "Transaction already taken; cannot start workflow".into(),
                )
            })?;
            return dispatcher
                .start_in_conn(
                    conn,
                    workflow_name,
                    input_json,
                    self.auth.principal_id(),
                    trace_id,
                )
                .await;
        }

        dispatcher
            .start_by_name(
                workflow_name,
                input_json,
                self.auth.principal_id(),
                trace_id,
            )
            .await
    }

    /// Type-safe workflow start: resolves the workflow name from the type's
    /// `ForgeWorkflow` impl.
    pub async fn start<W: crate::ForgeWorkflow>(
        &self,
        input: W::Input,
    ) -> crate::error::Result<Uuid> {
        self.start_workflow(W::info().name, input).await

    }
}

impl EnvAccess for MutationContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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

    #[test]
    fn auth_context_without_uuid_carries_claims_but_no_user_id() {
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user@example.com"));
        let ctx = AuthContext::authenticated_without_uuid(vec!["user".to_string()], claims);

        assert!(ctx.is_authenticated());
        assert!(ctx.user_id().is_none());
        assert!(ctx.require_user_id().is_err());
        assert_eq!(ctx.subject(), Some("user@example.com"));
        assert!(ctx.has_role("user"));
    }

    #[test]
    fn require_subject_errors_when_unauthenticated() {
        let ctx = AuthContext::unauthenticated();
        let err = ctx.require_subject().unwrap_err();
        assert!(matches!(err, crate::error::ForgeError::Unauthorized(_)));
    }

    #[test]
    fn require_subject_errors_when_authenticated_without_sub_claim() {
        // Authenticated context (e.g., UUID-based) with no `sub` claim should
        // still error from `require_subject`, since the raw subject is what
        // non-UUID providers care about.
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], HashMap::new());
        let err = ctx.require_subject().unwrap_err();
        assert!(matches!(err, crate::error::ForgeError::Unauthorized(_)));
    }

    #[test]
    fn require_subject_returns_sub_claim_when_present() {
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("abc"));
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], claims);
        assert_eq!(ctx.require_subject().unwrap(), "abc");
    }

    #[test]
    fn principal_id_prefers_sub_claim_over_uuid() {
        let user_id = Uuid::new_v4();
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("string-sub"));
        let ctx = AuthContext::authenticated(user_id, vec![], claims);
        // sub takes precedence over uuid.
        assert_eq!(ctx.principal_id(), Some("string-sub".to_string()));
    }

    #[test]
    fn principal_id_falls_back_to_uuid_when_no_sub_claim() {
        let user_id = Uuid::new_v4();
        let ctx = AuthContext::authenticated(user_id, vec![], HashMap::new());
        assert_eq!(ctx.principal_id(), Some(user_id.to_string()));
    }

    #[test]
    fn principal_id_is_none_for_unauthenticated_with_no_sub() {
        let ctx = AuthContext::unauthenticated();
        assert_eq!(ctx.principal_id(), None);
    }

    #[test]
    fn is_admin_only_true_when_admin_role_present() {
        let plain = AuthContext::authenticated(Uuid::new_v4(), vec!["user".into()], HashMap::new());
        assert!(!plain.is_admin());
        let admin =
            AuthContext::authenticated(Uuid::new_v4(), vec!["admin".into()], HashMap::new());
        assert!(admin.is_admin());
        // Unauthenticated never admin.
        assert!(!AuthContext::unauthenticated().is_admin());
    }

    #[test]
    fn tenant_id_parses_valid_uuid_claim() {
        let tenant = Uuid::new_v4();
        let mut claims = HashMap::new();
        claims.insert(
            "tenant_id".to_string(),
            serde_json::json!(tenant.to_string()),
        );
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], claims);
        assert_eq!(ctx.tenant_id(), Some(tenant));
    }

    #[test]
    fn tenant_id_returns_none_for_missing_or_invalid_claim() {
        // Missing.
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], HashMap::new());
        assert!(ctx.tenant_id().is_none());

        // Non-string.
        let mut claims = HashMap::new();
        claims.insert("tenant_id".to_string(), serde_json::json!(123));
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], claims);
        assert!(ctx.tenant_id().is_none());

        // Garbage string.
        let mut claims = HashMap::new();
        claims.insert("tenant_id".to_string(), serde_json::json!("not-a-uuid"));
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], claims);
        assert!(ctx.tenant_id().is_none());
    }

    #[test]
    fn token_exp_round_trips_and_drives_expiry_check() {
        let ctx = AuthContext::authenticated(Uuid::new_v4(), vec![], HashMap::new());
        // No token exp by default ⇒ never expired.
        assert!(!ctx.token_is_expired());
        assert!(ctx.token_exp().is_none());

        // Past timestamp ⇒ expired.
        let expired = ctx.clone().with_token_exp(1);
        assert_eq!(expired.token_exp(), Some(1));
        assert!(expired.token_is_expired());

        // Far-future timestamp ⇒ not expired.
        let live = ctx.with_token_exp(chrono::Utc::now().timestamp() + 3600);
        assert!(!live.token_is_expired());
    }

    #[test]
    fn token_is_expired_false_for_unauthenticated_without_exp() {
        let ctx = AuthContext::unauthenticated();
        assert!(!ctx.token_is_expired());
    }

    #[test]
    fn claims_and_roles_accessors_return_stored_values() {
        let mut claims = HashMap::new();
        claims.insert("k".to_string(), serde_json::json!("v"));
        let ctx = AuthContext::authenticated(
            Uuid::new_v4(),
            vec!["a".into(), "b".into()],
            claims.clone(),
        );

        assert_eq!(ctx.claims(), &claims);
        assert_eq!(ctx.roles(), &["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn request_metadata_setters_mutate_fields() {
        let mut meta = RequestMetadata::new();
        meta.set_client_ip(Some("1.2.3.4".to_string()));
        meta.set_user_agent(Some("ua/1".to_string()));
        meta.set_correlation_id(Some("corr-1".to_string()));

        assert_eq!(meta.client_ip(), Some("1.2.3.4"));
        assert_eq!(meta.user_agent(), Some("ua/1"));
        assert_eq!(meta.correlation_id(), Some("corr-1"));

        meta.set_client_ip(None);
        assert!(meta.client_ip().is_none());
    }

    #[test]
    fn request_metadata_internal_constructor_carries_fields() {
        let rid = Uuid::new_v4();
        let meta = RequestMetadata::__build_internal(
            rid,
            "t-1".into(),
            Some("ip".into()),
            Some("ua".into()),
            Some("corr".into()),
        );
        assert_eq!(meta.request_id(), rid);
        assert_eq!(meta.trace_id(), "t-1");
        assert_eq!(meta.client_ip(), Some("ip"));
        assert_eq!(meta.user_agent(), Some("ua"));
        assert_eq!(meta.correlation_id(), Some("corr"));
    }

    #[test]
    fn request_metadata_default_matches_new() {
        let a = RequestMetadata::default();
        let b = RequestMetadata::new();
        // Trace IDs differ (random), but field shape should match.
        assert!(a.client_ip().is_none());
        assert!(b.user_agent().is_none());
    }

    #[test]
    fn auth_token_ttl_default_is_one_hour_and_thirty_days() {
        let ttl = AuthTokenTtl::default();
        assert_eq!(ttl.access_token_secs, 3600);
        assert_eq!(ttl.refresh_token_days, 30);

        let custom = AuthTokenTtl::new(60, 7);
        assert_eq!(custom.access_token_secs, 60);
        assert_eq!(custom.refresh_token_days, 7);
    }

    #[test]
    fn sql_operation_classifies_common_prefixes() {
        assert_eq!(sql_operation("SELECT 1"), "SELECT");
        assert_eq!(sql_operation("  select * from users"), "SELECT");
        assert_eq!(sql_operation("Insert into x values (1)"), "INSERT");
        assert_eq!(sql_operation("UPDATE x SET v = 1"), "UPDATE");
        assert_eq!(sql_operation("delete from x"), "DELETE");
    }

    #[test]
    fn sql_operation_falls_back_to_other_for_unknown_or_short() {
        assert_eq!(
            sql_operation("WITH cte AS (SELECT 1) SELECT * FROM cte"),
            "OTHER"
        );
        assert_eq!(sql_operation("BEGIN"), "OTHER");
        assert_eq!(sql_operation(""), "OTHER");
        assert_eq!(sql_operation("hi"), "OTHER");
    }
}

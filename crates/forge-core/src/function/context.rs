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
//!   ├── ctx.db().execute(...)
//!   ├── ctx.dispatch_job("send_email", ...)  // INSERT into forge_jobs on this tx
//!   └── return Ok(result)
//! COMMIT
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use sqlx::postgres::{PgConnection, PgQueryResult, PgRow};
use sqlx::{Postgres, Transaction};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use tracing::Instrument;

use super::dispatch::{JobDispatch, WorkflowDispatch};
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
/// daemons, webhooks, MCP tools) or via `ctx.db()` on `MutationContext`.
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

/// A job buffered for dispatch after transaction commit.
///
/// This is internal runtime plumbing exposed only so test contexts can
/// inspect what was buffered. Construction is owned by the framework.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PendingJob {
    pub id: Uuid,
    pub job_type: String,
    pub args: serde_json::Value,
    pub context: serde_json::Value,
    pub owner_subject: Option<String>,
    pub priority: i32,
    pub max_attempts: i32,
    pub worker_capability: Option<String>,
}

/// A workflow buffered for dispatch after transaction commit.
///
/// This is internal runtime plumbing exposed only so test contexts can
/// inspect what was buffered. Construction is owned by the framework.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PendingWorkflow {
    pub id: Uuid,
    pub workflow_name: String,
    pub workflow_version: String,
    pub workflow_signature: String,
    pub input: serde_json::Value,
    pub owner_subject: Option<String>,
}

/// Buffer for jobs and workflows dispatched during a transactional mutation.
///
/// Entries are flushed to the database atomically after the mutation transaction commits.
/// If the transaction rolls back, buffered dispatches are discarded.
///
/// This is internal runtime plumbing. Use [`OutboxBuffer::new`] for construction
/// when needed (e.g. inside the runtime crate).
#[derive(Default)]
#[non_exhaustive]
pub struct OutboxBuffer {
    pub jobs: Vec<PendingJob>,
    pub workflows: Vec<PendingWorkflow>,
}

impl OutboxBuffer {
    /// Construct a new buffer holding the given pending dispatches.
    pub fn new(jobs: Vec<PendingJob>, workflows: Vec<PendingWorkflow>) -> Self {
        Self { jobs, workflows }
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
            env_provider: Arc::new(RealEnvProvider::new()),
            tx: None,
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
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
            env_provider: Arc::new(RealEnvProvider::new()),
            tx: None,
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
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
            env_provider: Arc::new(RealEnvProvider::new()),
            tx: Some(tx_handle.clone()),
            token_issuer: None,
            token_ttl: AuthTokenTtl::default(),
        };

        (ctx, tx_handle)
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
    ///     list_items(ctx.db()).await
    /// }
    /// ```
    pub fn db(&self) -> DbConn<'_> {
        match &self.tx {
            Some(tx) => DbConn::Transaction(tx.clone(), &self.db_pool),
            None => DbConn::Pool(self.db_pool.clone()),
        }
    }

    /// Get a `DbConn` for use in shared helper functions (alias for `db()`).
    pub fn db_conn(&self) -> DbConn<'_> {
        self.db()
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
    pub async fn dispatch_job<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> crate::error::Result<Uuid> {
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
}

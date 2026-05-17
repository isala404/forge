use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use super::context::{MutationContext, QueryContext};
use crate::error::Result;
use crate::metadata::HandlerMetadata;

/// Information about a registered function.
///
/// Constructed by the `#[query]` / `#[mutation]` macros — adding a field here
/// is technically a breaking change for hand-written `ForgeQuery` / `ForgeMutation`
/// impls, so any extension must ship a major bump or be staged through a
/// builder. Macro-emitted impls track the field set automatically.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// Function name (used for routing).
    pub name: &'static str,
    /// Human-readable description.
    pub description: Option<&'static str>,
    /// Kind of function.
    pub kind: FunctionKind,
    /// Required role (if any, implies auth required).
    pub required_role: Option<&'static str>,
    /// Whether this function is public (no auth).
    pub is_public: bool,
    /// Cache TTL in seconds (for queries).
    pub cache_ttl: Option<u64>,
    /// Execution timeout for the handler. `None` falls back to the runtime
    /// default. Mirrors the `Duration`-typed timeout fields on Job/Cron/Workflow
    /// info so consumers don't have to remember which kinds use seconds.
    pub timeout: Option<Duration>,
    /// Default timeout for outbound HTTP requests made via the circuit-breaker
    /// client. `None` means no request timeout is applied.
    pub http_timeout: Option<Duration>,
    /// Rate limit: requests per time window.
    pub rate_limit_requests: Option<u32>,
    /// Rate limit: time window in seconds.
    pub rate_limit_per_secs: Option<u64>,
    /// Rate limit: bucket key type.
    pub rate_limit_key: Option<crate::rate_limit::RateLimitKey>,
    /// Log level for access logging. Defaults to "debug" for queries, "info" for mutations.
    pub log_level: Option<LogLevel>,
    /// Table dependencies extracted at compile time for reactive subscriptions.
    /// Empty slice means tables could not be determined (dynamic SQL).
    pub table_dependencies: &'static [&'static str],
    /// Columns referenced in SELECT clauses, extracted at compile time.
    /// Used for fine-grained invalidation: skip re-execution when changed columns
    /// don't intersect with selected columns. Empty means unknown (invalidate always).
    pub selected_columns: &'static [&'static str],
    /// Columns written by INSERT/UPDATE statements, extracted at compile time.
    /// For mutations, lets the cache invalidator skip queries whose
    /// `selected_columns` don't overlap with what the mutation actually changed.
    /// Empty for queries; empty on a mutation means "could touch any column"
    /// (treated as full invalidation).
    pub changed_columns: &'static [&'static str],
    /// Whether this mutation should be wrapped in a database transaction.
    /// Only applies to mutations. When true, jobs are buffered and inserted
    /// atomically with the mutation via the outbox pattern.
    pub transactional: bool,
    /// Force this query to read from the primary database instead of replicas.
    /// Use for read-after-write consistency (e.g., post-mutation confirmation,
    /// permission checks depending on just-written state).
    pub consistent: bool,
    /// Per-function maximum upload size in bytes. Overrides gateway max_body_size.
    pub max_upload_size_bytes: Option<usize>,
    /// Whether this query's SQL scopes on `tenant_id`. When true, the runtime
    /// rejects dispatch if the auth context has no tenant claim, preventing
    /// silent empty-result bugs from `WHERE tenant_id = NULL`.
    pub requires_tenant_scope: bool,
}

/// The kind of function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FunctionKind {
    Query,
    Mutation,
    /// An inbound webhook endpoint. Registered in `FunctionRegistry` for
    /// metadata access (info, MCP tool list, observability) but executed
    /// exclusively through the dedicated webhook HTTP route with signature
    /// validation — never via the RPC dispatcher.
    Webhook,
}

/// Log level for per-function access logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

impl LogLevel {
    /// Convert to the lowercase string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Off => "off",
        }
    }
}

impl std::fmt::Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionKind::Query => write!(f, "query"),
            FunctionKind::Mutation => write!(f, "mutation"),
            FunctionKind::Webhook => write!(f, "webhook"),
        }
    }
}

/// A query function (read-only, cacheable, subscribable).
///
/// Queries:
/// - Can only read from the database
/// - Are automatically cached based on arguments
/// - Can be subscribed to for real-time updates
/// - Should be deterministic (same inputs → same outputs)
/// - Should not have side effects
pub trait ForgeQuery: crate::__sealed::Sealed + Send + Sync + 'static {
    /// The input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// The output type.
    type Output: Serialize + Send;

    /// Function metadata.
    fn info() -> FunctionInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    /// Execute the query.
    fn execute(
        ctx: &QueryContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// A mutation function (transactional write).
///
/// Mutations:
/// - Run within a database transaction
/// - Can read and write to the database
/// - Should NOT call external APIs (use Actions)
/// - Are atomic: all changes commit or none do
pub trait ForgeMutation: crate::__sealed::Sealed + Send + Sync + 'static {
    /// The input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// The output type.
    type Output: Serialize + Send;

    /// Function metadata.
    fn info() -> FunctionInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    /// Execute the mutation within a transaction.
    fn execute(
        ctx: &MutationContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_function_kind_display() {
        assert_eq!(format!("{}", FunctionKind::Query), "query");
        assert_eq!(format!("{}", FunctionKind::Mutation), "mutation");
        assert_eq!(format!("{}", FunctionKind::Webhook), "webhook");
    }

    #[test]
    fn test_function_info() {
        let info = FunctionInfo {
            name: "get_user",
            description: Some("Get a user by ID"),
            kind: FunctionKind::Query,
            required_role: None,
            is_public: false,
            cache_ttl: Some(300),
            timeout: Some(Duration::from_secs(30)),
            http_timeout: Some(Duration::from_secs(5)),
            rate_limit_requests: Some(100),
            rate_limit_per_secs: Some(60),
            rate_limit_key: Some(crate::rate_limit::RateLimitKey::User),
            log_level: Some(LogLevel::Debug),
            table_dependencies: &["users"],
            selected_columns: &["id", "name", "email"],
            changed_columns: &[],
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            requires_tenant_scope: false,
        };

        assert_eq!(info.name, "get_user");
        assert_eq!(info.kind, FunctionKind::Query);
        assert_eq!(info.cache_ttl, Some(300));
        assert_eq!(info.http_timeout, Some(Duration::from_secs(5)));
        assert_eq!(info.rate_limit_requests, Some(100));
        assert_eq!(info.log_level, Some(LogLevel::Debug));
        assert_eq!(info.table_dependencies, &["users"]);
    }
}

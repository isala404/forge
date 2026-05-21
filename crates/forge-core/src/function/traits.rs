use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use super::context::{MutationContext, QueryContext};
use crate::error::Result;

/// Metadata for a registered query or mutation function.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub kind: FunctionKind,
    pub required_role: Option<&'static str>,
    pub is_public: bool,
    pub cache_ttl: Option<u64>,
    /// `None` falls back to the runtime default.
    pub timeout: Option<Duration>,
    /// Default timeout for outbound HTTP requests via the circuit-breaker client.
    pub http_timeout: Option<Duration>,
    pub rate_limit_requests: Option<u32>,
    pub rate_limit_per_secs: Option<u64>,
    pub rate_limit_key: Option<crate::rate_limit::RateLimitKey>,
    pub log_level: Option<LogLevel>,
    /// Compile-time extracted tables for reactive subscriptions.
    pub table_dependencies: &'static [&'static str],
    /// Columns in SELECT clauses for fine-grained invalidation.
    pub selected_columns: &'static [&'static str],
    /// Columns written by INSERT/UPDATE for cache invalidation scoping.
    pub changed_columns: &'static [&'static str],
    /// Whether this mutation runs inside a database transaction.
    pub transactional: bool,
    /// Force reads from primary instead of replicas.
    pub consistent: bool,
    pub max_upload_size_bytes: Option<usize>,
    /// When true, runtime rejects dispatch if auth context has no tenant claim.
    pub requires_tenant_scope: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FunctionKind {
    Query,
    Mutation,
    Webhook,
}

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

impl FunctionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
            Self::Webhook => "webhook",
        }
    }
}

impl std::fmt::Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A read-only, cacheable, subscribable query function.
pub trait ForgeQuery: crate::__sealed::Sealed + Send + Sync + 'static {
    type Args: DeserializeOwned + Serialize + Send + Sync;
    type Output: Serialize + Send;

    fn info() -> FunctionInfo;

    fn execute(
        ctx: &QueryContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// A transactional write function.
pub trait ForgeMutation: crate::__sealed::Sealed + Send + Sync + 'static {
    type Args: DeserializeOwned + Serialize + Send + Sync;
    type Output: Serialize + Send;

    fn info() -> FunctionInfo;

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

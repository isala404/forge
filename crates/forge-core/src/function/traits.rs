use std::future::Future;
use std::pin::Pin;

use serde::{de::DeserializeOwned, Serialize};

use crate::error::Result;
use super::context::{QueryContext, MutationContext, ActionContext};

/// Information about a registered function.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// Function name (used for routing).
    pub name: &'static str,
    /// Human-readable description.
    pub description: Option<&'static str>,
    /// Kind of function.
    pub kind: FunctionKind,
    /// Whether authentication is required.
    pub requires_auth: bool,
    /// Required role (if any).
    pub required_role: Option<&'static str>,
    /// Whether this function is public (no auth).
    pub is_public: bool,
    /// Cache TTL in seconds (for queries).
    pub cache_ttl: Option<u64>,
    /// Timeout in seconds.
    pub timeout: Option<u64>,
}

/// The kind of function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Query,
    Mutation,
    Action,
}

impl std::fmt::Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionKind::Query => write!(f, "query"),
            FunctionKind::Mutation => write!(f, "mutation"),
            FunctionKind::Action => write!(f, "action"),
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
pub trait ForgeQuery: Send + Sync + 'static {
    /// The input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// The output type.
    type Output: Serialize + Send;

    /// Function metadata.
    fn info() -> FunctionInfo;

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
pub trait ForgeMutation: Send + Sync + 'static {
    /// The input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// The output type.
    type Output: Serialize + Send;

    /// Function metadata.
    fn info() -> FunctionInfo;

    /// Execute the mutation within a transaction.
    fn execute(
        ctx: &MutationContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// An action function (side effects, external APIs).
///
/// Actions:
/// - Can call external APIs
/// - Are NOT transactional by default
/// - Can call queries and mutations
/// - May be slow (external network calls)
/// - Can have timeouts and retries
pub trait ForgeAction: Send + Sync + 'static {
    /// The input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// The output type.
    type Output: Serialize + Send;

    /// Function metadata.
    fn info() -> FunctionInfo;

    /// Execute the action.
    fn execute(
        ctx: &ActionContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_kind_display() {
        assert_eq!(format!("{}", FunctionKind::Query), "query");
        assert_eq!(format!("{}", FunctionKind::Mutation), "mutation");
        assert_eq!(format!("{}", FunctionKind::Action), "action");
    }

    #[test]
    fn test_function_info() {
        let info = FunctionInfo {
            name: "get_user",
            description: Some("Get a user by ID"),
            kind: FunctionKind::Query,
            requires_auth: true,
            required_role: None,
            is_public: false,
            cache_ttl: Some(300),
            timeout: Some(30),
        };

        assert_eq!(info.name, "get_user");
        assert_eq!(info.kind, FunctionKind::Query);
        assert!(info.requires_auth);
        assert_eq!(info.cache_ttl, Some(300));
    }
}

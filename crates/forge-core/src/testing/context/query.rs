//! Test context for query functions.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::function::{AuthContext, RequestMetadata};
use crate::Result;

/// Test context for query functions.
///
/// Provides an isolated testing environment for queries with configurable
/// authentication and optional database access.
///
/// # Example
///
/// ```ignore
/// // Simple authenticated context
/// let ctx = TestQueryContext::authenticated(Uuid::new_v4());
///
/// // Context with roles and claims
/// let ctx = TestQueryContext::builder()
///     .as_user(Uuid::new_v4())
///     .with_role("admin")
///     .with_claim("org_id", json!("org-123"))
///     .build();
/// ```
pub struct TestQueryContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Optional database pool for integration tests.
    pool: Option<PgPool>,
}

impl TestQueryContext {
    /// Create a new builder.
    pub fn builder() -> TestQueryContextBuilder {
        TestQueryContextBuilder::default()
    }

    /// Create a minimal unauthenticated context (no database).
    pub fn minimal() -> Self {
        Self::builder().build()
    }

    /// Create an authenticated context with the given user ID (no database).
    pub fn authenticated(user_id: Uuid) -> Self {
        Self::builder().as_user(user_id).build()
    }

    /// Create a context with a database pool.
    pub fn with_pool(pool: PgPool, user_id: Option<Uuid>) -> Self {
        let mut builder = Self::builder().with_pool(pool);
        if let Some(id) = user_id {
            builder = builder.as_user(id);
        }
        builder.build()
    }

    /// Get the database pool (if available).
    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Get the authenticated user ID or return an error.
    pub fn require_user_id(&self) -> Result<Uuid> {
        self.auth.require_user_id()
    }

    /// Check if a specific role is present.
    pub fn has_role(&self, role: &str) -> bool {
        self.auth.has_role(role)
    }

    /// Get a claim value.
    pub fn claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.auth.claim(key)
    }
}

/// Builder for TestQueryContext.
#[derive(Default)]
pub struct TestQueryContextBuilder {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
}

impl TestQueryContextBuilder {
    /// Set the authenticated user.
    pub fn as_user(mut self, id: Uuid) -> Self {
        self.user_id = Some(id);
        self
    }

    /// Add a role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Add multiple roles.
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles.extend(roles);
        self
    }

    /// Add a custom claim.
    pub fn with_claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(key.into(), value);
        self
    }

    /// Set the database pool.
    pub fn with_pool(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Build the test context.
    pub fn build(self) -> TestQueryContext {
        let auth = if let Some(user_id) = self.user_id {
            AuthContext::authenticated(user_id, self.roles, self.claims)
        } else {
            AuthContext::unauthenticated()
        };

        TestQueryContext {
            auth,
            request: RequestMetadata::default(),
            pool: self.pool,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_context() {
        let ctx = TestQueryContext::minimal();
        assert!(!ctx.auth.is_authenticated());
        assert!(ctx.db().is_none());
    }

    #[test]
    fn test_authenticated_context() {
        let user_id = Uuid::new_v4();
        let ctx = TestQueryContext::authenticated(user_id);
        assert!(ctx.auth.is_authenticated());
        assert_eq!(ctx.require_user_id().unwrap(), user_id);
    }

    #[test]
    fn test_context_with_roles() {
        let ctx = TestQueryContext::builder()
            .as_user(Uuid::new_v4())
            .with_role("admin")
            .with_role("user")
            .build();

        assert!(ctx.has_role("admin"));
        assert!(ctx.has_role("user"));
        assert!(!ctx.has_role("superuser"));
    }

    #[test]
    fn test_context_with_claims() {
        let ctx = TestQueryContext::builder()
            .as_user(Uuid::new_v4())
            .with_claim("org_id", serde_json::json!("org-123"))
            .build();

        assert_eq!(ctx.claim("org_id"), Some(&serde_json::json!("org-123")));
        assert!(ctx.claim("nonexistent").is_none());
    }
}

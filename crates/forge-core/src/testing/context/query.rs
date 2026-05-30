//! Test context for query functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use super::build_test_auth;
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::{AuthContext, RequestMetadata};

/// Test context for query functions with optional DB access.
pub struct TestQueryContext {
    pub auth: AuthContext,
    pub request: RequestMetadata,
    pool: Option<PgPool>,
    tenant_id: Option<Uuid>,
    env_provider: Arc<MockEnvProvider>,
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

    /// Get the authenticated user's UUID. Returns 401 if not authenticated.
    pub fn user_id(&self) -> Result<Uuid> {
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

    /// Get the tenant ID (if set).
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id
    }

    /// Get the mock env provider for verification.
    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }
}

impl EnvAccess for TestQueryContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Builder for TestQueryContext.
#[derive(Default)]
pub struct TestQueryContextBuilder {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    tenant_id: Option<Uuid>,
    pool: Option<PgPool>,
    env_vars: HashMap<String, String>,
}

impl_test_auth_builder!(TestQueryContextBuilder);
impl_test_env_builder!(TestQueryContextBuilder);
impl_test_tenant_builder!(TestQueryContextBuilder);

impl TestQueryContextBuilder {
    /// Build the test context.
    pub fn build(self) -> TestQueryContext {
        TestQueryContext {
            auth: build_test_auth(self.user_id, self.roles, self.claims),
            request: RequestMetadata::default(),
            pool: self.pool,
            tenant_id: self.tenant_id,
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_with_env() {
        let ctx = TestQueryContext::builder()
            .with_env("API_KEY", "test_key_123")
            .with_env("TIMEOUT", "30")
            .build();

        // Test env access via EnvAccess trait
        assert_eq!(ctx.env("API_KEY"), Some("test_key_123".to_string()));
        assert_eq!(ctx.env_or("TIMEOUT", "10"), "30");
        assert_eq!(ctx.env_or("MISSING", "default"), "default");

        // Test env_require
        assert!(ctx.env_require("API_KEY").is_ok());
        assert!(ctx.env_require("MISSING").is_err());

        // Test env_parse
        let timeout: u32 = ctx.env_parse("TIMEOUT").unwrap();
        assert_eq!(timeout, 30);

        // Verify access tracking
        ctx.env_mock().assert_accessed("API_KEY");
        ctx.env_mock().assert_accessed("TIMEOUT");
    }
}

use std::collections::HashMap;

use uuid::Uuid;

/// Authentication context available to all functions.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// The authenticated user ID (if any).
    user_id: Option<Uuid>,
    /// User roles.
    roles: Vec<String>,
    /// Custom claims from JWT.
    claims: HashMap<String, serde_json::Value>,
    /// Whether the request is authenticated.
    authenticated: bool,
}

impl AuthContext {
    /// Create an unauthenticated context.
    pub fn unauthenticated() -> Self {
        Self {
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            authenticated: false,
        }
    }

    /// Create an authenticated context.
    pub fn authenticated(user_id: Uuid, roles: Vec<String>, claims: HashMap<String, serde_json::Value>) -> Self {
        Self {
            user_id: Some(user_id),
            roles,
            claims,
            authenticated: true,
        }
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

    /// Get all roles.
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
}

/// Request metadata available to all functions.
#[derive(Debug, Clone)]
pub struct RequestMetadata {
    /// Unique request ID for tracing.
    pub request_id: Uuid,
    /// Trace ID for distributed tracing.
    pub trace_id: String,
    /// Client IP address.
    pub client_ip: Option<String>,
    /// User agent string.
    pub user_agent: Option<String>,
    /// Request timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl RequestMetadata {
    /// Create new request metadata.
    pub fn new() -> Self {
        Self {
            request_id: Uuid::new_v4(),
            trace_id: Uuid::new_v4().to_string(),
            client_ip: None,
            user_agent: None,
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
            timestamp: chrono::Utc::now(),
        }
    }
}

impl Default for RequestMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Context for query functions (read-only database access).
pub struct QueryContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for read operations.
    db_pool: sqlx::PgPool,
}

impl QueryContext {
    /// Create a new query context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self {
            auth,
            request,
            db_pool,
        }
    }

    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the authenticated user ID or return an error.
    pub fn require_user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }
}

/// Context for mutation functions (transactional database access).
pub struct MutationContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for transactional operations.
    db_pool: sqlx::PgPool,
}

impl MutationContext {
    /// Create a new mutation context.
    pub fn new(db_pool: sqlx::PgPool, auth: AuthContext, request: RequestMetadata) -> Self {
        Self {
            auth,
            request,
            db_pool,
        }
    }

    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the authenticated user ID or return an error.
    pub fn require_user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }
}

/// Context for action functions (can call external APIs).
pub struct ActionContext {
    /// Authentication context.
    pub auth: AuthContext,
    /// Request metadata.
    pub request: RequestMetadata,
    /// Database pool for database operations.
    db_pool: sqlx::PgPool,
    /// HTTP client for external requests.
    http_client: reqwest::Client,
}

impl ActionContext {
    /// Create a new action context.
    pub fn new(
        db_pool: sqlx::PgPool,
        auth: AuthContext,
        request: RequestMetadata,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            auth,
            request,
            db_pool,
            http_client,
        }
    }

    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get a reference to the HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get the authenticated user ID or return an error.
    pub fn require_user_id(&self) -> crate::error::Result<Uuid> {
        self.auth.require_user_id()
    }
}

#[cfg(test)]
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

        assert_eq!(
            ctx.claim("org_id"),
            Some(&serde_json::json!("org-123"))
        );
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

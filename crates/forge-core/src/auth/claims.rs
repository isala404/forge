use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// JWT claims structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject (user ID).
    pub sub: String,
    /// Issued at (Unix timestamp).
    pub iat: i64,
    /// Expiration time (Unix timestamp).
    pub exp: i64,
    /// User roles.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Custom claims.
    #[serde(flatten)]
    pub custom: HashMap<String, serde_json::Value>,
}

impl Claims {
    /// Get the user ID as UUID.
    pub fn user_id(&self) -> Option<Uuid> {
        Uuid::parse_str(&self.sub).ok()
    }

    /// Check if the token is expired.
    pub fn is_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        self.exp < now
    }

    /// Check if the user has a role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Get a custom claim value.
    pub fn get_claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.custom.get(key)
    }

    /// Create a builder for constructing claims.
    pub fn builder() -> ClaimsBuilder {
        ClaimsBuilder::new()
    }
}

/// Builder for JWT claims.
#[derive(Debug, Default)]
pub struct ClaimsBuilder {
    sub: Option<String>,
    roles: Vec<String>,
    custom: HashMap<String, serde_json::Value>,
    duration_secs: i64,
}

impl ClaimsBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            sub: None,
            roles: Vec::new(),
            custom: HashMap::new(),
            duration_secs: 3600, // 1 hour default
        }
    }

    /// Set the subject (user ID).
    pub fn subject(mut self, sub: impl Into<String>) -> Self {
        self.sub = Some(sub.into());
        self
    }

    /// Set the user ID from UUID.
    pub fn user_id(mut self, id: Uuid) -> Self {
        self.sub = Some(id.to_string());
        self
    }

    /// Add a role.
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Set multiple roles.
    pub fn roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }

    /// Add a custom claim.
    pub fn claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.custom.insert(key.into(), value);
        self
    }

    /// Set token duration in seconds.
    pub fn duration_secs(mut self, secs: i64) -> Self {
        self.duration_secs = secs;
        self
    }

    /// Build the claims.
    pub fn build(self) -> Result<Claims, String> {
        let sub = self.sub.ok_or("Subject is required")?;
        let now = chrono::Utc::now().timestamp();

        Ok(Claims {
            sub,
            iat: now,
            exp: now + self.duration_secs,
            roles: self.roles,
            custom: self.custom,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claims_builder() {
        let user_id = Uuid::new_v4();
        let claims = Claims::builder()
            .user_id(user_id)
            .role("admin")
            .role("user")
            .claim("org_id", serde_json::json!("org-123"))
            .duration_secs(7200)
            .build()
            .unwrap();

        assert_eq!(claims.user_id(), Some(user_id));
        assert!(claims.has_role("admin"));
        assert!(claims.has_role("user"));
        assert!(!claims.has_role("superadmin"));
        assert_eq!(
            claims.get_claim("org_id"),
            Some(&serde_json::json!("org-123"))
        );
        assert!(!claims.is_expired());
    }

    #[test]
    fn test_claims_expiration() {
        let claims = Claims {
            sub: "user-1".to_string(),
            iat: 0,
            exp: 1, // Expired timestamp
            roles: vec![],
            custom: HashMap::new(),
        };

        assert!(claims.is_expired());
    }

    #[test]
    fn test_claims_serialization() {
        let claims = Claims::builder()
            .subject("user-1")
            .role("admin")
            .build()
            .unwrap();

        let json = serde_json::to_string(&claims).unwrap();
        let deserialized: Claims = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.sub, claims.sub);
        assert_eq!(deserialized.roles, claims.roles);
    }
}

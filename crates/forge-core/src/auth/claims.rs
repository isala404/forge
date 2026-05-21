use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// JWT claims structure.
///
/// Fields are intentionally crate-private. Construct via [`Claims::builder`]
/// and read via the accessor methods. The `custom` map is gated by
/// [`Claims::get_claim`] / [`Claims::sanitized_custom`] so reserved JWT
/// claim names (`iss`, `aud`, `nbf`, `jti`, …) can never be retrieved as
/// custom data, even when serde's `#[serde(flatten)]` lets them in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Claims {
    /// Subject (user ID). Use [`Claims::sub`] / [`Claims::user_id`].
    pub(crate) sub: String,
    /// Issued at (Unix timestamp). Use [`Claims::iat`].
    pub(crate) iat: i64,
    /// Expiration time (Unix timestamp). Use [`Claims::exp`] /
    /// [`Claims::is_expired`].
    pub(crate) exp: i64,
    /// Audience (`aud` claim). Use [`Claims::audience`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aud: Option<String>,
    /// User roles. Use [`Claims::roles`] / [`Claims::has_role`].
    #[serde(default)]
    pub(crate) roles: Vec<String>,
    /// Custom claims, with reserved JWT claims filtered out on read.
    /// Use [`Claims::get_claim`] / [`Claims::sanitized_custom`].
    #[serde(flatten)]
    pub(crate) custom: HashMap<String, serde_json::Value>,
}

impl Claims {
    /// Get the subject (raw `sub` claim).
    pub fn sub(&self) -> &str {
        &self.sub
    }

    /// Get the issued-at Unix timestamp.
    pub fn iat(&self) -> i64 {
        self.iat
    }

    /// Get the expiration Unix timestamp.
    pub fn exp(&self) -> i64 {
        self.exp
    }

    /// Get the audience (`aud` claim), if set.
    pub fn audience(&self) -> Option<&str> {
        self.aud.as_deref()
    }

    /// Get the user roles.
    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    /// Consume the claims and return the owned roles vector.
    pub fn into_roles(self) -> Vec<String> {
        self.roles
    }

    /// Consume the claims and return the owned subject string.
    pub fn into_sub(self) -> String {
        self.sub
    }

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

    /// Reserved JWT claim names that should not be treated as custom claims.
    const RESERVED_CLAIMS: &'static [&'static str] =
        &["iss", "aud", "nbf", "jti", "sub", "iat", "exp", "roles"];

    /// Get a custom claim value.
    ///
    /// Returns `None` for reserved JWT claims (iss, aud, nbf, jti, etc.)
    /// to prevent claim injection via `#[serde(flatten)]`.
    pub fn get_claim(&self, key: &str) -> Option<&serde_json::Value> {
        if Self::RESERVED_CLAIMS.contains(&key) {
            return None;
        }
        self.custom.get(key)
    }

    /// Get custom claims with reserved JWT claims filtered out.
    ///
    /// Prevents claim injection where standard JWT claims like `iss`, `aud`,
    /// or `jti` end up in the custom claims map via `#[serde(flatten)]`.
    pub fn sanitized_custom(&self) -> HashMap<String, serde_json::Value> {
        self.custom
            .iter()
            .filter(|(k, _)| !Self::RESERVED_CLAIMS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get the tenant ID if present in claims.
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.custom
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
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
    aud: Option<String>,
    roles: Vec<String>,
    custom: HashMap<String, serde_json::Value>,
    duration_secs: i64,
}

impl ClaimsBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            sub: None,
            aud: None,
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
    ///
    /// Rejects reserved JWT claim names to prevent duplicate-keyed tokens where
    /// structural fields (`sub`, `exp`, …) and a flattened custom key both serialize
    /// under the same JSON key — some validators read one, `ctx.claim()` reads the other.
    ///
    /// Use the typed setters instead:
    /// - `sub` / `iat` / `exp` → `.subject()` / `.user_id()` / `.duration_secs()`
    /// - `roles` → `.role()` / `.roles()`
    /// - `aud` → `.audience()`
    /// - `nbf`, `jti`, `iss` are not supported by this builder
    pub fn claim(
        mut self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> crate::Result<Self> {
        let key = key.into();
        if Claims::RESERVED_CLAIMS.contains(&key.as_str()) {
            return Err(crate::ForgeError::InvalidArgument(format!(
                "'{key}' is a reserved JWT claim name; use the typed setter instead"
            )));
        }
        self.custom.insert(key, value);
        Ok(self)
    }

    /// Set the token audience (`aud` claim).
    pub fn audience(mut self, aud: impl Into<String>) -> Self {
        self.aud = Some(aud.into());
        self
    }

    /// Set the tenant ID.
    pub fn tenant_id(mut self, id: Uuid) -> Self {
        self.custom
            .insert("tenant_id".to_string(), serde_json::json!(id.to_string()));
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
            aud: self.aud,
            roles: self.roles,
            custom: self.custom,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
            .unwrap()
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
    fn claim_rejects_reserved_names() {
        for reserved in Claims::RESERVED_CLAIMS {
            let result = Claims::builder()
                .subject("user-1")
                .claim(*reserved, serde_json::json!("value"));
            assert!(
                result.is_err(),
                "Expected '{reserved}' to be rejected but it was accepted"
            );
        }
    }

    #[test]
    fn claim_accepts_custom_names() {
        let result = Claims::builder()
            .subject("user-1")
            .claim("org_id", serde_json::json!("org-123"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_claims_expiration() {
        let claims = Claims {
            sub: "user-1".to_string(),
            iat: 0,
            exp: 1, // Expired timestamp
            aud: None,
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

    #[test]
    fn build_errors_when_subject_missing() {
        let result = Claims::builder().role("user").build();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Subject is required"));
    }

    #[test]
    fn duration_secs_sets_exp_offset_from_iat() {
        let claims = Claims::builder()
            .subject("u")
            .duration_secs(120)
            .build()
            .unwrap();
        // exp == iat + duration_secs.
        assert_eq!(claims.exp() - claims.iat(), 120);
    }

    #[test]
    fn default_duration_secs_is_one_hour() {
        let claims = Claims::builder().subject("u").build().unwrap();
        assert_eq!(claims.exp() - claims.iat(), 3600);
    }

    #[test]
    fn is_expired_false_for_future_exp() {
        let now = chrono::Utc::now().timestamp();
        let claims = Claims {
            sub: "u".into(),
            iat: now,
            exp: now + 3600,
            aud: None,
            roles: vec![],
            custom: HashMap::new(),
        };
        assert!(!claims.is_expired());
    }

    #[test]
    fn user_id_returns_none_for_non_uuid_subject() {
        let claims = Claims::builder().subject("not-a-uuid").build().unwrap();
        assert!(claims.user_id().is_none());
        // sub accessor still returns the raw string verbatim.
        assert_eq!(claims.sub(), "not-a-uuid");
    }

    #[test]
    fn user_id_set_via_builder_round_trips_through_sub() {
        let id = Uuid::new_v4();
        let claims = Claims::builder().user_id(id).build().unwrap();
        assert_eq!(claims.user_id(), Some(id));
        assert_eq!(claims.sub(), id.to_string());
    }

    #[test]
    fn into_methods_consume_owned_values() {
        let claims = Claims::builder()
            .subject("user-x")
            .role("a")
            .role("b")
            .build()
            .unwrap();
        // Clone to use after into_*.
        let roles = claims.clone().into_roles();
        assert_eq!(roles, vec!["a".to_string(), "b".to_string()]);
        let sub = claims.into_sub();
        assert_eq!(sub, "user-x");
    }

    #[test]
    fn roles_setter_replaces_prior_calls() {
        let claims = Claims::builder()
            .subject("u")
            .role("first")
            .roles(vec!["one".into(), "two".into()])
            .build()
            .unwrap();
        // `.roles()` replaces, doesn't extend, so "first" is gone.
        assert_eq!(claims.roles(), &["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn get_claim_returns_none_for_reserved_names_even_if_present() {
        // Reserved names can leak in via deserialization (#[serde(flatten)] +
        // duplicate keys). Construct directly to bypass the builder guard.
        let mut custom = HashMap::new();
        custom.insert("iss".to_string(), serde_json::json!("evil"));
        custom.insert("jti".to_string(), serde_json::json!("evil"));
        custom.insert("safe".to_string(), serde_json::json!("ok"));
        let claims = Claims {
            sub: "u".into(),
            iat: 0,
            exp: i64::MAX,
            aud: None,
            roles: vec![],
            custom,
        };
        assert!(claims.get_claim("iss").is_none());
        assert!(claims.get_claim("jti").is_none());
        assert_eq!(claims.get_claim("safe"), Some(&serde_json::json!("ok")));
    }

    #[test]
    fn get_claim_returns_none_for_missing_custom_key() {
        let claims = Claims::builder().subject("u").build().unwrap();
        assert!(claims.get_claim("nope").is_none());
    }

    #[test]
    fn sanitized_custom_filters_reserved_names() {
        let mut custom = HashMap::new();
        for reserved in Claims::RESERVED_CLAIMS {
            custom.insert((*reserved).to_string(), serde_json::json!("smuggled"));
        }
        custom.insert("org_id".into(), serde_json::json!("o1"));
        let claims = Claims {
            sub: "u".into(),
            iat: 0,
            exp: i64::MAX,
            aud: None,
            roles: vec![],
            custom,
        };
        let safe = claims.sanitized_custom();
        // Only the non-reserved key survives.
        assert_eq!(safe.len(), 1);
        assert_eq!(safe.get("org_id"), Some(&serde_json::json!("o1")));
        for reserved in Claims::RESERVED_CLAIMS {
            assert!(
                !safe.contains_key(*reserved),
                "{reserved} should be filtered out"
            );
        }
    }

    #[test]
    fn tenant_id_round_trips_via_builder() {
        let tenant = Uuid::new_v4();
        let claims = Claims::builder()
            .subject("u")
            .tenant_id(tenant)
            .build()
            .unwrap();
        assert_eq!(claims.tenant_id(), Some(tenant));
    }

    #[test]
    fn tenant_id_returns_none_when_value_is_not_string_or_uuid() {
        // Not a string: numeric.
        let mut custom = HashMap::new();
        custom.insert("tenant_id".to_string(), serde_json::json!(42));
        let claims = Claims {
            sub: "u".into(),
            iat: 0,
            exp: i64::MAX,
            aud: None,
            roles: vec![],
            custom,
        };
        assert!(claims.tenant_id().is_none());

        // String but not UUID.
        let mut custom = HashMap::new();
        custom.insert("tenant_id".to_string(), serde_json::json!("garbage"));
        let claims = Claims {
            sub: "u".into(),
            iat: 0,
            exp: i64::MAX,
            aud: None,
            roles: vec![],
            custom,
        };
        assert!(claims.tenant_id().is_none());
    }

    #[test]
    fn audience_round_trips_through_typed_field() {
        let claims = Claims::builder()
            .subject("u")
            .audience("my-service")
            .build()
            .unwrap();
        assert_eq!(claims.audience(), Some("my-service"));
        // Serializes into the JWT as "aud"
        let json = serde_json::to_value(&claims).unwrap();
        assert_eq!(json.get("aud"), Some(&serde_json::json!("my-service")));
        // Does not leak into the custom map
        assert!(!claims.custom.contains_key("aud"));
    }

    #[test]
    fn audience_deserializes_from_jwt() {
        let claims = Claims::builder()
            .subject("u")
            .audience("svc-1")
            .build()
            .unwrap();
        let json = serde_json::to_string(&claims).unwrap();
        let restored: Claims = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.audience(), Some("svc-1"));
    }

    #[test]
    fn reserved_claims_set_matches_documented_list() {
        // Lock the reserved list so future additions are intentional code review.
        let expected: std::collections::HashSet<&str> =
            ["iss", "aud", "nbf", "jti", "sub", "iat", "exp", "roles"]
                .into_iter()
                .collect();
        let actual: std::collections::HashSet<&str> =
            Claims::RESERVED_CLAIMS.iter().copied().collect();
        assert_eq!(actual, expected);
    }
}

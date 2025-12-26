use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use forge_core::auth::Claims;
use forge_core::function::AuthContext;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use uuid::Uuid;

/// Authentication configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// JWT secret for HMAC signing.
    pub jwt_secret: String,
    /// JWT algorithm (HS256, HS384, HS512).
    pub algorithm: JwtAlgorithm,
    /// Whether to allow unauthenticated requests.
    pub allow_anonymous: bool,
    /// Skip signature verification (DEV MODE ONLY - NEVER USE IN PRODUCTION).
    /// This allows testing with any JWT token without a valid signature.
    pub skip_verification: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            algorithm: JwtAlgorithm::HS256,
            allow_anonymous: true,
            skip_verification: false,
        }
    }
}

impl AuthConfig {
    /// Create a new auth config with the given secret.
    pub fn with_secret(secret: impl Into<String>) -> Self {
        Self {
            jwt_secret: secret.into(),
            ..Default::default()
        }
    }

    /// Create a dev mode config that skips signature verification.
    /// WARNING: Only use this for development and testing!
    pub fn dev_mode() -> Self {
        Self {
            jwt_secret: String::new(),
            algorithm: JwtAlgorithm::HS256,
            allow_anonymous: true,
            skip_verification: true,
        }
    }
}

/// Supported JWT algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtAlgorithm {
    HS256,
    HS384,
    HS512,
}

impl Default for JwtAlgorithm {
    fn default() -> Self {
        Self::HS256
    }
}

impl From<JwtAlgorithm> for Algorithm {
    fn from(alg: JwtAlgorithm) -> Self {
        match alg {
            JwtAlgorithm::HS256 => Algorithm::HS256,
            JwtAlgorithm::HS384 => Algorithm::HS384,
            JwtAlgorithm::HS512 => Algorithm::HS512,
        }
    }
}

/// Authentication middleware.
#[derive(Clone)]
pub struct AuthMiddleware {
    config: Arc<AuthConfig>,
    decoding_key: Option<DecodingKey>,
}

impl std::fmt::Debug for AuthMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthMiddleware")
            .field("config", &self.config)
            .field("decoding_key", &self.decoding_key.is_some())
            .finish()
    }
}

impl AuthMiddleware {
    /// Create a new auth middleware.
    pub fn new(config: AuthConfig) -> Self {
        let decoding_key = if config.skip_verification || config.jwt_secret.is_empty() {
            None
        } else {
            Some(DecodingKey::from_secret(config.jwt_secret.as_bytes()))
        };

        Self {
            config: Arc::new(config),
            decoding_key,
        }
    }

    /// Create a middleware that allows all requests (development mode).
    /// WARNING: This skips signature verification! Never use in production.
    pub fn permissive() -> Self {
        Self::new(AuthConfig::dev_mode())
    }

    /// Get the config.
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Validate a JWT token and extract claims.
    pub fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {
        if self.config.skip_verification {
            // DEV MODE: Skip signature verification
            self.decode_without_verification(token)
        } else if let Some(ref key) = self.decoding_key {
            self.decode_with_verification(token, key)
        } else {
            Err(AuthError::InvalidToken(
                "JWT secret not configured".to_string(),
            ))
        }
    }

    /// Decode and verify JWT token using jsonwebtoken crate.
    fn decode_with_verification(
        &self,
        token: &str,
        key: &DecodingKey,
    ) -> Result<Claims, AuthError> {
        let mut validation = Validation::new(self.config.algorithm.into());

        // Configure validation
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.leeway = 60; // 60 seconds clock skew tolerance

        // Require exp claim
        validation.set_required_spec_claims(&["exp", "sub"]);

        let token_data = decode::<Claims>(token, key, &validation).map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
            jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                AuthError::InvalidToken("Invalid signature".to_string())
            }
            jsonwebtoken::errors::ErrorKind::InvalidToken => {
                AuthError::InvalidToken("Invalid token format".to_string())
            }
            jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(claim) => {
                AuthError::InvalidToken(format!("Missing required claim: {}", claim))
            }
            _ => AuthError::InvalidToken(e.to_string()),
        })?;

        Ok(token_data.claims)
    }

    /// Decode JWT token without signature verification (DEV MODE ONLY).
    /// This parses the token structure but does not validate the signature.
    fn decode_without_verification(&self, token: &str) -> Result<Claims, AuthError> {
        // Create validation that skips signature verification
        let mut validation = Validation::new(Algorithm::HS256);
        validation.insecure_disable_signature_validation();
        validation.validate_exp = false; // We'll check expiration manually
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();

        // Use a dummy key since we're not validating signature
        let dummy_key = DecodingKey::from_secret(b"dummy");

        let token_data =
            decode::<Claims>(token, &dummy_key, &validation).map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::InvalidToken => {
                    AuthError::InvalidToken("Invalid token format".to_string())
                }
                _ => AuthError::InvalidToken(e.to_string()),
            })?;

        // Still check expiration in dev mode
        if token_data.claims.is_expired() {
            return Err(AuthError::TokenExpired);
        }

        Ok(token_data.claims)
    }
}

/// Authentication errors.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthError {
    #[error("Missing authorization header")]
    MissingHeader,
    #[error("Invalid authorization header format")]
    InvalidHeader,
    #[error("Invalid token: {0}")]
    InvalidToken(String),
    #[error("Token expired")]
    TokenExpired,
}

/// Extract auth context from request.
pub fn extract_auth_context(req: &Request<Body>, middleware: &AuthMiddleware) -> AuthContext {
    // Try to extract Authorization header
    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            Some(header.trim_start_matches("Bearer ").trim())
        }
        _ => None,
    };

    match token {
        Some(token) => match middleware.validate_token(token) {
            Ok(claims) => {
                let user_id = claims.user_id().unwrap_or_else(Uuid::nil);
                let custom_claims: HashMap<String, serde_json::Value> = claims.custom;
                AuthContext::authenticated(user_id, claims.roles, custom_claims)
            }
            Err(_) => AuthContext::unauthenticated(),
        },
        None => AuthContext::unauthenticated(),
    }
}

/// Authentication middleware function.
pub async fn auth_middleware(
    State(middleware): State<Arc<AuthMiddleware>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let auth_context = extract_auth_context(&req, &middleware);

    // Store auth context in request extensions
    let mut req = req;
    req.extensions_mut().insert(auth_context);

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    fn create_test_claims(expired: bool) -> Claims {
        use forge_core::auth::ClaimsBuilder;

        let mut builder = ClaimsBuilder::new().subject("test-user-id").role("user");

        if expired {
            builder = builder.duration_secs(-3600); // Expired 1 hour ago
        } else {
            builder = builder.duration_secs(3600); // Valid for 1 hour
        }

        builder.build().unwrap()
    }

    fn create_test_token(claims: &Claims, secret: &str) -> String {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn test_auth_config_default() {
        let config = AuthConfig::default();
        assert!(config.allow_anonymous);
        assert_eq!(config.algorithm, JwtAlgorithm::HS256);
        assert!(!config.skip_verification);
    }

    #[test]
    fn test_auth_config_dev_mode() {
        let config = AuthConfig::dev_mode();
        assert!(config.skip_verification);
        assert!(config.allow_anonymous);
    }

    #[test]
    fn test_auth_middleware_permissive() {
        let middleware = AuthMiddleware::permissive();
        assert!(middleware.config.skip_verification);
    }

    #[test]
    fn test_valid_token_with_correct_secret() {
        let secret = "test-secret-key";
        let config = AuthConfig::with_secret(secret);
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let token = create_test_token(&claims, secret);

        let result = middleware.validate_token(&token);
        assert!(result.is_ok());
        let validated_claims = result.unwrap();
        assert_eq!(validated_claims.sub, "test-user-id");
    }

    #[test]
    fn test_valid_token_with_wrong_secret() {
        let config = AuthConfig::with_secret("correct-secret");
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let token = create_test_token(&claims, "wrong-secret");

        let result = middleware.validate_token(&token);
        assert!(result.is_err());
        match result {
            Err(AuthError::InvalidToken(_)) => {}
            _ => panic!("Expected InvalidToken error"),
        }
    }

    #[test]
    fn test_expired_token() {
        let secret = "test-secret";
        let config = AuthConfig::with_secret(secret);
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(true); // Expired
        let token = create_test_token(&claims, secret);

        let result = middleware.validate_token(&token);
        assert!(result.is_err());
        match result {
            Err(AuthError::TokenExpired) => {}
            _ => panic!("Expected TokenExpired error"),
        }
    }

    #[test]
    fn test_tampered_token() {
        let secret = "test-secret";
        let config = AuthConfig::with_secret(secret);
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let mut token = create_test_token(&claims, secret);

        // Tamper with the token by modifying a character in the signature
        if let Some(last_char) = token.pop() {
            let replacement = if last_char == 'a' { 'b' } else { 'a' };
            token.push(replacement);
        }

        let result = middleware.validate_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_dev_mode_skips_signature() {
        let config = AuthConfig::dev_mode();
        let middleware = AuthMiddleware::new(config);

        // Create token with any secret
        let claims = create_test_claims(false);
        let token = create_test_token(&claims, "any-secret");

        // Should still validate in dev mode
        let result = middleware.validate_token(&token);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dev_mode_still_checks_expiration() {
        let config = AuthConfig::dev_mode();
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(true); // Expired
        let token = create_test_token(&claims, "any-secret");

        let result = middleware.validate_token(&token);
        assert!(result.is_err());
        match result {
            Err(AuthError::TokenExpired) => {}
            _ => panic!("Expected TokenExpired error even in dev mode"),
        }
    }

    #[test]
    fn test_invalid_token_format() {
        let config = AuthConfig::with_secret("secret");
        let middleware = AuthMiddleware::new(config);

        let result = middleware.validate_token("not-a-valid-jwt");
        assert!(result.is_err());
        match result {
            Err(AuthError::InvalidToken(_)) => {}
            _ => panic!("Expected InvalidToken error"),
        }
    }

    #[test]
    fn test_algorithm_conversion() {
        assert_eq!(Algorithm::from(JwtAlgorithm::HS256), Algorithm::HS256);
        assert_eq!(Algorithm::from(JwtAlgorithm::HS384), Algorithm::HS384);
        assert_eq!(Algorithm::from(JwtAlgorithm::HS512), Algorithm::HS512);
    }
}

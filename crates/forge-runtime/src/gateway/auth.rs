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
use uuid::Uuid;

/// Authentication configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// JWT secret for HS256 signing.
    pub jwt_secret: String,
    /// JWT algorithm (currently only HS256 supported).
    pub algorithm: JwtAlgorithm,
    /// Whether to allow unauthenticated requests.
    pub allow_anonymous: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            algorithm: JwtAlgorithm::HS256,
            allow_anonymous: true,
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

/// Authentication middleware.
#[derive(Debug, Clone)]
pub struct AuthMiddleware {
    config: Arc<AuthConfig>,
}

impl AuthMiddleware {
    /// Create a new auth middleware.
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Create a middleware that allows all requests (development mode).
    pub fn permissive() -> Self {
        Self::new(AuthConfig {
            jwt_secret: String::new(),
            algorithm: JwtAlgorithm::HS256,
            allow_anonymous: true,
        })
    }

    /// Get the config.
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Validate a JWT token and extract claims.
    pub fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {
        // For now, implement basic JWT validation
        // In production, use a proper JWT library like jsonwebtoken
        self.decode_jwt(token)
    }

    /// Decode JWT token (simplified implementation).
    fn decode_jwt(&self, token: &str) -> Result<Claims, AuthError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(AuthError::InvalidToken("Invalid JWT format".into()));
        }

        // Decode payload (middle part)
        let payload = parts[1];
        let decoded = base64_decode_url_safe(payload)
            .map_err(|_| AuthError::InvalidToken("Failed to decode payload".into()))?;

        let claims: Claims = serde_json::from_slice(&decoded)
            .map_err(|e| AuthError::InvalidToken(format!("Failed to parse claims: {}", e)))?;

        // Check expiration
        if claims.is_expired() {
            return Err(AuthError::TokenExpired);
        }

        // TODO: Verify signature with jwt_secret
        // For now, we trust the token structure

        Ok(claims)
    }
}

/// Decode base64 URL-safe encoded data.
fn base64_decode_url_safe(input: &str) -> Result<Vec<u8>, String> {
    // Add padding if needed
    let padded = match input.len() % 4 {
        2 => format!("{}==", input),
        3 => format!("{}=", input),
        _ => input.to_string(),
    };

    // Convert URL-safe to standard base64
    let standard = padded.replace('-', "+").replace('_', "/");

    // Decode using simple base64 decoding
    decode_base64(&standard)
}

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let input = input.as_bytes();
    let mut output = Vec::with_capacity(input.len() * 3 / 4);

    let mut buffer = 0u32;
    let mut bits = 0u8;

    for &byte in input {
        if byte == b'=' {
            break;
        }

        let value = ALPHABET
            .iter()
            .position(|&c| c == byte)
            .ok_or_else(|| "Invalid base64 character".to_string())?;

        buffer = (buffer << 6) | (value as u32);
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    Ok(output)
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

    #[test]
    fn test_auth_config_default() {
        let config = AuthConfig::default();
        assert!(config.allow_anonymous);
        assert_eq!(config.algorithm, JwtAlgorithm::HS256);
    }

    #[test]
    fn test_auth_middleware_permissive() {
        let middleware = AuthMiddleware::permissive();
        assert!(middleware.config.allow_anonymous);
    }

    #[test]
    fn test_base64_decode() {
        // "hello" in base64 is "aGVsbG8="
        let decoded = decode_base64("aGVsbG8=").unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn test_base64_url_safe_decode() {
        // Test URL-safe base64 decoding with padding
        let decoded = base64_decode_url_safe("aGVsbG8").unwrap();
        assert_eq!(decoded, b"hello");
    }
}

//! Authentication and JWT configuration.

use crate::error::{ForgeError, Result};
use serde::{Deserialize, Serialize};

/// JWT signing algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
#[non_exhaustive]
pub enum JwtAlgorithm {
    /// HMAC using SHA-256 (symmetric, requires jwt_secret).
    #[default]
    HS256,
    /// RSA using SHA-256 (asymmetric, requires jwks_url).
    RS256,
}

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuthConfig {
    /// JWT secret for HMAC algorithms (HS256).
    /// Required when using HMAC algorithms.
    pub jwt_secret: Option<String>,

    /// JWT signing algorithm.
    /// HS256 (default) requires jwt_secret.
    /// RS256 requires jwks_url.
    #[serde(default)]
    pub jwt_algorithm: JwtAlgorithm,

    /// Expected token issuer (iss claim).
    /// If set, tokens with a different issuer are rejected.
    pub jwt_issuer: Option<String>,

    /// Expected audience (aud claim).
    /// If set, tokens with a different audience are rejected.
    pub jwt_audience: Option<String>,

    /// Access token lifetime (e.g., "15m", "1h").
    /// Used by `ctx.issue_token_pair()`. Defaults to "1h".
    pub access_token_ttl: Option<String>,

    /// Refresh token lifetime (e.g., "7d", "30d").
    /// Used by `ctx.issue_token_pair()`. Defaults to "30d".
    pub refresh_token_ttl: Option<String>,

    /// JWKS URL for RSA algorithms (RS256).
    /// Keys are fetched and cached automatically.
    pub jwks_url: Option<String>,

    /// JWKS cache TTL duration (e.g. "1h", "30m").
    #[serde(default = "default_jwks_cache_ttl")]
    pub jwks_cache_ttl: String,

    /// Session TTL duration (e.g. "7d", "24h"). Used for WebSocket sessions.
    #[serde(default = "default_session_ttl")]
    pub session_ttl: String,

    /// Clock-skew tolerance for `exp` / `nbf` validation (e.g. "60s", "5m").
    /// Sites with NTP-synchronized clocks can drop this to "5s"; older deployments
    /// or clients with drifting clocks may need higher. Defaults to "60s".
    #[serde(default = "default_jwt_leeway")]
    pub jwt_leeway: String,

    /// When `true` (default), `jwt_audience` must be set when auth is enabled.
    /// Set to `false` only during migration. Enforce it again once all clients
    /// send an `aud` claim.
    #[serde(default = "default_audience_required")]
    pub audience_required: bool,

    /// JWT spec claims that must be present in every token.
    /// Defaults to `["exp", "sub"]`. Add `"aud"` here if you want claim-level
    /// enforcement in addition to the `jwt_audience` equality check.
    #[serde(default = "default_required_claims")]
    pub required_claims: Vec<String>,

    /// Session cookie lifetime (e.g., "1h", "24h").
    /// Used for OAuth consent flow cookies. Defaults to the access token TTL.
    pub session_cookie_ttl: Option<String>,

    /// Old HMAC secrets still accepted for validation (never for signing).
    /// Rotate by adding the outgoing secret here, swapping `jwt_secret` to the
    /// new value, then removing it after one access-token TTL elapses.
    #[serde(default)]
    pub legacy_secrets: Vec<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: None,
            jwt_algorithm: JwtAlgorithm::default(),
            jwt_issuer: None,
            jwt_audience: None,
            access_token_ttl: None,
            refresh_token_ttl: None,
            jwks_url: None,
            jwks_cache_ttl: default_jwks_cache_ttl(),
            session_ttl: default_session_ttl(),
            jwt_leeway: default_jwt_leeway(),
            audience_required: default_audience_required(),
            required_claims: default_required_claims(),
            session_cookie_ttl: None,
            legacy_secrets: Vec::new(),
        }
    }
}

impl AuthConfig {
    /// JWKS cache TTL in seconds, parsed from the `jwks_cache_ttl` string.
    pub fn jwks_cache_ttl_secs(&self) -> u64 {
        super::parse_duration_secs(&self.jwks_cache_ttl, 3600)
    }

    /// Session TTL in seconds, parsed from the `session_ttl` string.
    pub fn session_ttl_secs(&self) -> u64 {
        super::parse_duration_secs(&self.session_ttl, 7 * 24 * 3600)
    }

    /// JWT clock-skew leeway in seconds, parsed from the `jwt_leeway` string.
    pub fn jwt_leeway_secs(&self) -> u64 {
        super::parse_duration_secs(&self.jwt_leeway, 60)
    }

    /// Resolved access token TTL in seconds.
    /// Parses `access_token_ttl`, default 3600s (1h).
    /// Minimum 1 second to prevent zero-lifetime tokens.
    pub fn access_token_ttl_secs(&self) -> i64 {
        self.access_token_ttl
            .as_deref()
            .and_then(crate::util::parse_duration)
            .map(|d| (d.as_secs() as i64).max(1))
            .unwrap_or(3600)
    }

    /// Resolved refresh token TTL in days.
    /// Parses `refresh_token_ttl`, default 30 days.
    pub fn refresh_token_ttl_days(&self) -> i64 {
        self.refresh_token_ttl
            .as_deref()
            .and_then(crate::util::parse_duration)
            .map(|d| (d.as_secs() / 86400) as i64)
            .map(|d| if d == 0 { 1 } else { d })
            .unwrap_or(30)
    }

    /// Resolved session cookie TTL in seconds.
    /// Falls back to `access_token_ttl_secs()` when not explicitly set.
    pub fn session_cookie_ttl_secs(&self) -> i64 {
        self.session_cookie_ttl
            .as_deref()
            .and_then(crate::util::parse_duration)
            .map(|d| (d.as_secs() as i64).max(1))
            .unwrap_or_else(|| self.access_token_ttl_secs())
    }

    /// Check if auth is configured (any credential or claim validation is set).
    pub fn is_configured(&self) -> bool {
        self.jwt_secret.is_some()
            || self.jwks_url.is_some()
            || self.jwt_issuer.is_some()
            || self.jwt_audience.is_some()
    }

    /// Validate that the configuration is complete for the chosen algorithm.
    /// Skips validation if no auth settings are configured (auth disabled).
    pub fn validate(&self) -> Result<()> {
        if !self.is_configured() {
            return Ok(());
        }

        match self.jwt_algorithm {
            JwtAlgorithm::HS256 => {
                if self.jwt_secret.is_none() {
                    return Err(ForgeError::Config(
                        "auth.jwt_secret is required for HMAC algorithms (HS256). \
                         Set auth.jwt_secret to a secure random string, \
                         or switch to RS256 and provide auth.jwks_url for external identity providers."
                            .into(),
                    ));
                }
                if let Some(secret) = &self.jwt_secret
                    && secret.len() < 32
                {
                    return Err(ForgeError::Config(format!(
                        "auth.jwt_secret is {} bytes but must be at least 32 bytes for HMAC \
                         to be collision-resistant. Generate one with: \
                         openssl rand -base64 32",
                        secret.len()
                    )));
                }
            }
            JwtAlgorithm::RS256 => {
                if self.jwks_url.is_none() {
                    return Err(ForgeError::Config(
                        "auth.jwks_url is required for RSA algorithms (RS256). \
                         Set auth.jwks_url to your identity provider's JWKS endpoint, \
                         or switch to HS256 and provide auth.jwt_secret for symmetric signing."
                            .into(),
                    ));
                }
            }
        }

        if self.audience_required && self.jwt_audience.is_none() {
            return Err(ForgeError::Config(
                "auth.jwt_audience is required when auth is enabled. \
                 Set auth.jwt_audience to your application's audience identifier (e.g. \"https://api.example.com\"), \
                 or set auth.audience_required = false to opt out during migration."
                    .into(),
            ));
        }

        Ok(())
    }

    /// Check if this config uses HMAC (symmetric) algorithms.
    pub fn is_hmac(&self) -> bool {
        matches!(self.jwt_algorithm, JwtAlgorithm::HS256)
    }

    /// Check if this config uses RSA (asymmetric) algorithms.
    pub fn is_rsa(&self) -> bool {
        matches!(self.jwt_algorithm, JwtAlgorithm::RS256)
    }
}

fn default_jwks_cache_ttl() -> String {
    "1h".to_string()
}

fn default_session_ttl() -> String {
    "7d".to_string()
}

fn default_jwt_leeway() -> String {
    "60s".to_string()
}

fn default_audience_required() -> bool {
    true
}

fn default_required_claims() -> Vec<String> {
    vec!["exp".into(), "sub".into()]
}

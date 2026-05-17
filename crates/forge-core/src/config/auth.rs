//! Authentication and JWT configuration.

use std::time::Duration;

use crate::error::{ForgeError, Result};
use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// JWT signing algorithm.
///
/// Supported values in forge.toml: `"HS256"` (default), `"RS256"`.
/// Any other value produces a deserialization error at startup.
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

/// A retired HMAC secret kept around for a bounded window so tokens minted
/// before rotation still validate. After `valid_until` the entry is silently
/// dropped at startup and never used for verification — leaked old keys
/// cannot extend their reach indefinitely.
///
/// Rotate by adding the outgoing secret here with `valid_until` set one
/// access-token TTL into the future, swap `jwt_secret` to the new value,
/// then remove the entry once the window closes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacySecret {
    /// HMAC secret bytes (treated as opaque; min length is not re-enforced
    /// here — the active `jwt_secret` validation already covers minimum
    /// strength, and a previously-active key already satisfied it).
    pub secret: String,
    /// RFC 3339 timestamp after which this key is no longer accepted.
    pub valid_until: chrono::DateTime<chrono::Utc>,
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
    pub access_token_ttl: Option<DurationStr>,

    /// Refresh token lifetime (e.g., "7d", "30d").
    /// Used by `ctx.issue_token_pair()`. Defaults to "30d".
    pub refresh_token_ttl: Option<DurationStr>,

    /// JWKS URL for RSA algorithms (RS256).
    /// Keys are fetched and cached automatically.
    pub jwks_url: Option<String>,

    /// JWKS cache TTL duration (e.g. "1h", "30m").
    #[serde(default = "default_jwks_cache_ttl")]
    pub jwks_cache_ttl: DurationStr,

    /// Session TTL duration (e.g. "7d", "24h"). Used for WebSocket sessions.
    #[serde(default = "default_session_ttl")]
    pub session_ttl: DurationStr,

    /// Clock-skew tolerance for `exp` / `nbf` validation (e.g. "60s", "5m").
    /// Sites with NTP-synchronized clocks can drop this to "5s"; older deployments
    /// or clients with drifting clocks may need higher. Defaults to "60s".
    #[serde(default = "default_jwt_leeway")]
    pub jwt_leeway: DurationStr,

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
    pub session_cookie_ttl: Option<DurationStr>,

    /// Old HMAC secrets still accepted for validation (never for signing).
    /// Each entry carries a mandatory `valid_until` timestamp; expired entries
    /// are silently dropped at middleware construction.
    #[serde(default)]
    pub legacy_secrets: Vec<LegacySecret>,
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
    /// Resolved access token TTL in seconds.
    /// Parses `access_token_ttl`, default 3600s (1h).
    /// Minimum 1 second to prevent zero-lifetime tokens.
    pub fn access_token_ttl_secs(&self) -> i64 {
        self.access_token_ttl
            .map(|d| (d.as_secs() as i64).max(1))
            .unwrap_or(3600)
    }

    /// Resolved refresh token TTL in days.
    /// Parses `refresh_token_ttl`, default 30 days.
    pub fn refresh_token_ttl_days(&self) -> i64 {
        self.refresh_token_ttl
            .map(|d| {
                let days = (d.as_secs() / 86400) as i64;
                if days == 0 { 1 } else { days }
            })
            .unwrap_or(30)
    }

    /// Resolved session cookie TTL in seconds.
    /// Falls back to `access_token_ttl_secs()` when not explicitly set.
    pub fn session_cookie_ttl_secs(&self) -> i64 {
        self.session_cookie_ttl
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

fn default_jwks_cache_ttl() -> DurationStr {
    DurationStr::new(Duration::from_secs(3600))
}

fn default_session_ttl() -> DurationStr {
    DurationStr::new(Duration::from_secs(604800))
}

fn default_jwt_leeway() -> DurationStr {
    DurationStr::new(Duration::from_secs(60))
}

fn default_audience_required() -> bool {
    true
}

fn default_required_claims() -> Vec<String> {
    vec!["exp".into(), "sub".into()]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn strong_secret() -> String {
        "x".repeat(32)
    }

    #[test]
    fn default_algorithm_is_hs256() {
        assert_eq!(JwtAlgorithm::default(), JwtAlgorithm::HS256);
    }

    #[test]
    fn default_required_claims_are_exp_and_sub() {
        let claims = default_required_claims();
        assert!(claims.contains(&"exp".to_string()));
        assert!(claims.contains(&"sub".to_string()));
    }

    #[test]
    fn default_audience_is_required() {
        assert!(default_audience_required());
    }

    #[test]
    fn is_configured_false_when_completely_empty() {
        let cfg = AuthConfig::default();
        assert!(!cfg.is_configured());
    }

    #[test]
    fn is_configured_true_when_jwt_secret_set() {
        let cfg = AuthConfig {
            jwt_secret: Some("anything".into()),
            ..AuthConfig::default()
        };
        assert!(cfg.is_configured());
    }

    #[test]
    fn is_configured_true_when_only_jwt_issuer_set() {
        // Issuer-only is enough to flip is_configured even without a secret.
        let cfg = AuthConfig {
            jwt_issuer: Some("https://issuer".into()),
            ..AuthConfig::default()
        };
        assert!(cfg.is_configured());
    }

    #[test]
    fn is_configured_true_when_only_jwks_url_set() {
        let cfg = AuthConfig {
            jwks_url: Some("https://jwks".into()),
            ..AuthConfig::default()
        };
        assert!(cfg.is_configured());
    }

    #[test]
    fn validate_passes_when_auth_disabled() {
        // Empty config means auth is off; validation must not complain.
        let cfg = AuthConfig::default();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_hs256_rejects_missing_secret() {
        // Issuer alone trips is_configured but HS256 still needs a secret.
        let cfg = AuthConfig {
            jwt_algorithm: JwtAlgorithm::HS256,
            jwt_issuer: Some("https://issuer".into()),
            jwt_audience: Some("api".into()),
            ..AuthConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        let ForgeError::Config(msg) = err else {
            panic!("expected Config error");
        };
        assert!(msg.contains("jwt_secret"));
    }

    #[test]
    fn validate_hs256_rejects_short_secret() {
        let cfg = AuthConfig {
            jwt_secret: Some("too-short".into()),
            jwt_audience: Some("api".into()),
            ..AuthConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        let ForgeError::Config(msg) = err else {
            panic!("expected Config error");
        };
        assert!(msg.contains("32 bytes"), "{msg}");
    }

    #[test]
    fn validate_hs256_accepts_exactly_32_byte_secret() {
        let cfg = AuthConfig {
            jwt_secret: Some(strong_secret()),
            jwt_audience: Some("api".into()),
            ..AuthConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rs256_rejects_missing_jwks_url() {
        let cfg = AuthConfig {
            jwt_algorithm: JwtAlgorithm::RS256,
            jwt_issuer: Some("https://issuer".into()),
            jwt_audience: Some("api".into()),
            ..AuthConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        let ForgeError::Config(msg) = err else {
            panic!("expected Config error");
        };
        assert!(msg.contains("jwks_url"));
    }

    #[test]
    fn validate_rs256_does_not_require_jwt_secret() {
        let cfg = AuthConfig {
            jwt_algorithm: JwtAlgorithm::RS256,
            jwks_url: Some("https://jwks".into()),
            jwt_audience: Some("api".into()),
            ..AuthConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_audience_required_rejects_missing_audience() {
        let cfg = AuthConfig {
            jwt_secret: Some(strong_secret()),
            audience_required: true,
            jwt_audience: None,
            ..AuthConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        let ForgeError::Config(msg) = err else {
            panic!("expected Config error");
        };
        assert!(msg.contains("jwt_audience"));
    }

    #[test]
    fn validate_audience_opt_out_passes_without_audience() {
        let cfg = AuthConfig {
            jwt_secret: Some(strong_secret()),
            audience_required: false,
            jwt_audience: None,
            ..AuthConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn access_token_ttl_default_is_one_hour() {
        let cfg = AuthConfig::default();
        assert_eq!(cfg.access_token_ttl_secs(), 3600);
    }

    #[test]
    fn access_token_ttl_clamps_to_at_least_one_second() {
        // Zero-duration TTLs would mint already-expired tokens; the floor of 1
        // keeps them at least nominally valid so client retry logic doesn't loop.
        let cfg = AuthConfig {
            access_token_ttl: Some(DurationStr::new(Duration::from_secs(0))),
            ..AuthConfig::default()
        };
        assert_eq!(cfg.access_token_ttl_secs(), 1);
    }

    #[test]
    fn refresh_token_ttl_default_is_30_days() {
        let cfg = AuthConfig::default();
        assert_eq!(cfg.refresh_token_ttl_days(), 30);
    }

    #[test]
    fn refresh_token_ttl_sub_day_rounds_to_one() {
        // Anything less than a day still grants one full day so tokens are usable.
        let cfg = AuthConfig {
            refresh_token_ttl: Some(DurationStr::new(Duration::from_secs(60))),
            ..AuthConfig::default()
        };
        assert_eq!(cfg.refresh_token_ttl_days(), 1);
    }

    #[test]
    fn refresh_token_ttl_seven_days_passes_through() {
        let cfg = AuthConfig {
            refresh_token_ttl: Some(DurationStr::new(Duration::from_secs(7 * 86400))),
            ..AuthConfig::default()
        };
        assert_eq!(cfg.refresh_token_ttl_days(), 7);
    }

    #[test]
    fn session_cookie_ttl_falls_back_to_access_token_ttl() {
        let cfg = AuthConfig {
            access_token_ttl: Some(DurationStr::new(Duration::from_secs(900))),
            session_cookie_ttl: None,
            ..AuthConfig::default()
        };
        assert_eq!(cfg.session_cookie_ttl_secs(), 900);
    }

    #[test]
    fn session_cookie_ttl_overrides_access_token_ttl_when_set() {
        let cfg = AuthConfig {
            access_token_ttl: Some(DurationStr::new(Duration::from_secs(900))),
            session_cookie_ttl: Some(DurationStr::new(Duration::from_secs(1200))),
            ..AuthConfig::default()
        };
        assert_eq!(cfg.session_cookie_ttl_secs(), 1200);
    }

    #[test]
    fn is_hmac_and_is_rsa_are_mutually_exclusive() {
        let hs = AuthConfig {
            jwt_algorithm: JwtAlgorithm::HS256,
            ..AuthConfig::default()
        };
        assert!(hs.is_hmac());
        assert!(!hs.is_rsa());

        let rs = AuthConfig {
            jwt_algorithm: JwtAlgorithm::RS256,
            ..AuthConfig::default()
        };
        assert!(rs.is_rsa());
        assert!(!rs.is_hmac());
    }

    #[test]
    fn jwt_algorithm_deserializes_uppercase_strings() {
        let hs: JwtAlgorithm = serde_json::from_str(r#""HS256""#).unwrap();
        let rs: JwtAlgorithm = serde_json::from_str(r#""RS256""#).unwrap();
        assert_eq!(hs, JwtAlgorithm::HS256);
        assert_eq!(rs, JwtAlgorithm::RS256);
        // Unknown values must fail loudly so a typo in forge.toml is caught at boot.
        assert!(serde_json::from_str::<JwtAlgorithm>(r#""ES256""#).is_err());
    }
}

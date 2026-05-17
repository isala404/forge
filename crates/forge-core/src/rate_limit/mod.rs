use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::ForgeError;
use crate::util::parse_duration;

mod backend;

pub use backend::RateLimiterBackend;

/// Rate limit key type for bucketing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateLimitKey {
    /// Per-user rate limiting.
    #[default]
    User,
    /// Per-IP rate limiting.
    Ip,
    /// Per-tenant rate limiting.
    Tenant,
    /// Per-user-action rate limiting (combines user and action).
    UserAction,
    /// Global rate limiting (single bucket for all requests).
    Global,
    /// Custom claim-based bucketing. The inner string is the JWT claim name
    /// whose value is used as the bucket discriminator. Backends receive this
    /// variant via `build_key` and may further customise the resolution logic.
    ///
    /// **Macro syntax**: `#[query(rate_limit(requests = 100, per = "1m", key = "custom:claim_name"))]`
    Custom(String),
}

impl RateLimitKey {
    /// Convert to a string representation.
    ///
    /// For [`Self::Custom`] this returns `"custom"` — use [`Self::custom_name`]
    /// to retrieve the inner claim name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Ip => "ip",
            Self::Tenant => "tenant",
            Self::UserAction => "user_action",
            Self::Global => "global",
            Self::Custom(_) => "custom",
        }
    }

    /// Return the inner claim name for [`Self::Custom`], or `None` for the
    /// standard variants.
    pub fn custom_name(&self) -> Option<&str> {
        match self {
            Self::Custom(name) => Some(name.as_str()),
            _ => None,
        }
    }
}

/// Error returned when parsing an unknown rate limit key string.
#[derive(Debug, Clone)]
pub struct ParseRateLimitKeyError(pub String);

impl std::fmt::Display for ParseRateLimitKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown rate limit key \"{}\". Expected one of: user, ip, tenant, user_action, global, custom:<name>",
            self.0
        )
    }
}

impl std::error::Error for ParseRateLimitKeyError {}

impl FromStr for RateLimitKey {
    type Err = ParseRateLimitKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "ip" => Ok(Self::Ip),
            "tenant" => Ok(Self::Tenant),
            "user_action" => Ok(Self::UserAction),
            "global" => Ok(Self::Global),
            _ if s.starts_with("custom:") => {
                Ok(Self::Custom(s.trim_start_matches("custom:").to_string()))
            }
            _ => Err(ParseRateLimitKeyError(s.to_string())),
        }
    }
}

/// Rate limit configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RateLimitConfig {
    /// Maximum requests allowed.
    pub requests: u32,
    /// Time window for the limit.
    pub per: Duration,
    /// Key type for bucketing.
    pub key: RateLimitKey,
}

impl RateLimitConfig {
    /// Create a new rate limit config.
    pub fn new(requests: u32, per: Duration) -> Self {
        Self {
            requests,
            per,
            key: RateLimitKey::default(),
        }
    }

    /// Create with a specific key type.
    pub fn with_key(mut self, key: RateLimitKey) -> Self {
        self.key = key;
        self
    }

    /// Calculate the refill rate (tokens per second).
    pub fn refill_rate(&self) -> f64 {
        self.requests as f64 / self.per.as_secs_f64()
    }

    /// Parse duration from string like "1m", "1h", "1d", "100ms".
    /// Delegates to [`crate::util::parse_duration`].
    pub fn parse_duration(s: &str) -> Option<Duration> {
        parse_duration(s)
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests: 100,
            per: Duration::from_secs(60),
            key: RateLimitKey::User,
        }
    }
}

/// Result of a rate limit check.
#[derive(Debug, Clone)]
pub struct RateLimitResult {
    /// Whether the request is allowed.
    pub allowed: bool,
    /// Remaining requests in the current window.
    pub remaining: u32,
    /// When the limit resets.
    pub reset_at: DateTime<Utc>,
    /// Time to wait before retrying (if not allowed).
    pub retry_after: Option<Duration>,
}

impl RateLimitResult {
    /// Create a result for an allowed request.
    pub fn allowed(remaining: u32, reset_at: DateTime<Utc>) -> Self {
        Self {
            allowed: true,
            remaining,
            reset_at,
            retry_after: None,
        }
    }

    /// Create a result for a denied request.
    pub fn denied(remaining: u32, reset_at: DateTime<Utc>, retry_after: Duration) -> Self {
        Self {
            allowed: false,
            remaining,
            reset_at,
            retry_after: Some(retry_after),
        }
    }

    /// Convert to a ForgeError if rate limited.
    pub fn to_error(&self, limit: u32) -> Option<ForgeError> {
        if self.allowed {
            None
        } else {
            Some(ForgeError::RateLimitExceeded {
                retry_after: self.retry_after.unwrap_or(Duration::from_secs(1)),
                limit,
                remaining: self.remaining,
            })
        }
    }
}

/// HTTP headers for rate limiting.
#[derive(Debug, Clone)]
pub struct RateLimitHeaders {
    /// X-RateLimit-Limit header value.
    pub limit: u32,
    /// X-RateLimit-Remaining header value.
    pub remaining: u32,
    /// X-RateLimit-Reset header value (Unix timestamp).
    pub reset: i64,
    /// Retry-After header value (seconds).
    pub retry_after: Option<u64>,
}

impl RateLimitHeaders {
    /// Create headers from a rate limit result.
    pub fn from_result(result: &RateLimitResult, limit: u32) -> Self {
        Self {
            limit,
            remaining: result.remaining,
            reset: result.reset_at.timestamp(),
            retry_after: result.retry_after.map(|d| d.as_secs()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_key() {
        assert_eq!(RateLimitKey::User.as_str(), "user");
        assert_eq!(RateLimitKey::Ip.as_str(), "ip");
        assert_eq!(RateLimitKey::Global.as_str(), "global");
        assert_eq!(
            RateLimitKey::Custom("tenant_id".to_string()).as_str(),
            "custom"
        );
        assert_eq!(
            RateLimitKey::Custom("tenant_id".to_string()).custom_name(),
            Some("tenant_id")
        );

        assert_eq!("user".parse::<RateLimitKey>().unwrap(), RateLimitKey::User);
        assert_eq!("ip".parse::<RateLimitKey>().unwrap(), RateLimitKey::Ip);
        assert_eq!(
            "custom:tenant_id".parse::<RateLimitKey>().unwrap(),
            RateLimitKey::Custom("tenant_id".to_string())
        );
    }

    #[test]
    fn test_rate_limit_config() {
        let config = RateLimitConfig::new(100, Duration::from_secs(60));
        assert_eq!(config.requests, 100);
        assert_eq!(config.per, Duration::from_secs(60));
        assert!((config.refill_rate() - 1.666666).abs() < 0.01);
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(
            RateLimitConfig::parse_duration("1s"),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            RateLimitConfig::parse_duration("1m"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            RateLimitConfig::parse_duration("1h"),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(
            RateLimitConfig::parse_duration("1d"),
            Some(Duration::from_secs(86400))
        );
        assert_eq!(RateLimitConfig::parse_duration("invalid"), None);
    }

    #[test]
    fn test_rate_limit_result_allowed() {
        let result = RateLimitResult::allowed(99, Utc::now());
        assert!(result.allowed);
        assert!(result.retry_after.is_none());
        assert!(result.to_error(100).is_none());
    }

    #[test]
    fn test_rate_limit_result_denied() {
        let result = RateLimitResult::denied(0, Utc::now(), Duration::from_secs(30));
        assert!(!result.allowed);
        assert!(result.retry_after.is_some());
        assert!(result.to_error(100).is_some());
    }

    #[test]
    fn rate_limit_key_default_is_user() {
        // Defaulting to User means handlers without an explicit key are scoped
        // per-principal — flipping this to Global would silently make limits
        // shared across all callers.
        assert_eq!(RateLimitKey::default(), RateLimitKey::User);
    }

    #[test]
    fn rate_limit_key_as_str_covers_all_standard_variants() {
        assert_eq!(RateLimitKey::Tenant.as_str(), "tenant");
        assert_eq!(RateLimitKey::UserAction.as_str(), "user_action");
    }

    #[test]
    fn rate_limit_key_custom_name_is_none_for_standard_variants() {
        for variant in [
            RateLimitKey::User,
            RateLimitKey::Ip,
            RateLimitKey::Tenant,
            RateLimitKey::UserAction,
            RateLimitKey::Global,
        ] {
            assert_eq!(variant.custom_name(), None);
        }
    }

    #[test]
    fn rate_limit_key_parse_covers_all_named_variants() {
        assert_eq!(
            "tenant".parse::<RateLimitKey>().unwrap(),
            RateLimitKey::Tenant
        );
        assert_eq!(
            "user_action".parse::<RateLimitKey>().unwrap(),
            RateLimitKey::UserAction
        );
        assert_eq!(
            "global".parse::<RateLimitKey>().unwrap(),
            RateLimitKey::Global
        );
    }

    #[test]
    fn rate_limit_key_parse_custom_extracts_inner_name() {
        let parsed = "custom:org_id".parse::<RateLimitKey>().unwrap();
        assert_eq!(parsed, RateLimitKey::Custom("org_id".to_string()));
        assert_eq!(parsed.custom_name(), Some("org_id"));

        // Empty inner name is still accepted at parse time; the backend decides
        // what to do with it.
        let empty = "custom:".parse::<RateLimitKey>().unwrap();
        assert_eq!(empty, RateLimitKey::Custom(String::new()));
    }

    #[test]
    fn rate_limit_key_parse_unknown_returns_descriptive_error() {
        let err = "bogus".parse::<RateLimitKey>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus"), "error should echo input: {msg}");
        assert!(
            msg.contains("user, ip, tenant, user_action, global, custom:<name>"),
            "error should list valid keys: {msg}"
        );
    }

    #[test]
    fn rate_limit_config_default_matches_documented_values() {
        let cfg = RateLimitConfig::default();
        assert_eq!(cfg.requests, 100);
        assert_eq!(cfg.per, Duration::from_secs(60));
        assert_eq!(cfg.key, RateLimitKey::User);
    }

    #[test]
    fn rate_limit_config_with_key_overrides_default() {
        let cfg = RateLimitConfig::new(10, Duration::from_secs(1)).with_key(RateLimitKey::Ip);
        assert_eq!(cfg.key, RateLimitKey::Ip);
        assert_eq!(cfg.requests, 10);
        assert_eq!(cfg.per, Duration::from_secs(1));
    }

    #[test]
    fn rate_limit_config_refill_rate_handles_burst_window() {
        // 60 requests/30s = 2 tokens/sec
        let cfg = RateLimitConfig::new(60, Duration::from_secs(30));
        assert!((cfg.refill_rate() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn to_error_carries_retry_after_and_limit_metadata() {
        let result = RateLimitResult::denied(3, Utc::now(), Duration::from_secs(42));
        let err = result.to_error(100).expect("denied result yields error");
        match err {
            ForgeError::RateLimitExceeded {
                retry_after,
                limit,
                remaining,
            } => {
                assert_eq!(retry_after, Duration::from_secs(42));
                assert_eq!(limit, 100);
                assert_eq!(remaining, 3);
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn to_error_falls_back_to_1s_when_retry_after_missing() {
        // Construct a denied-shaped result with no retry_after to confirm the
        // documented 1-second fallback in to_error.
        let result = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset_at: Utc::now(),
            retry_after: None,
        };
        match result.to_error(5).expect("denied yields error") {
            ForgeError::RateLimitExceeded { retry_after, .. } => {
                assert_eq!(retry_after, Duration::from_secs(1));
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_headers_from_allowed_result_omits_retry_after() {
        let reset = Utc::now();
        let result = RateLimitResult::allowed(7, reset);
        let headers = RateLimitHeaders::from_result(&result, 10);
        assert_eq!(headers.limit, 10);
        assert_eq!(headers.remaining, 7);
        assert_eq!(headers.reset, reset.timestamp());
        assert_eq!(headers.retry_after, None);
    }

    #[test]
    fn rate_limit_headers_from_denied_result_carries_retry_after_seconds() {
        let reset = Utc::now();
        let result = RateLimitResult::denied(0, reset, Duration::from_secs(15));
        let headers = RateLimitHeaders::from_result(&result, 10);
        assert_eq!(headers.retry_after, Some(15));
        assert_eq!(headers.remaining, 0);
    }
}

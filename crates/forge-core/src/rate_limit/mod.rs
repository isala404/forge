use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::ForgeError;

/// Rate limit key type for bucketing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
}

impl RateLimitKey {
    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Ip => "ip",
            Self::Tenant => "tenant",
            Self::UserAction => "user_action",
            Self::Global => "global",
        }
    }
}

impl FromStr for RateLimitKey {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "user" => Self::User,
            "ip" => Self::Ip,
            "tenant" => Self::Tenant,
            "user_action" => Self::UserAction,
            "global" => Self::Global,
            _ => Self::User,
        })
    }
}

/// Rate limit configuration.
#[derive(Debug, Clone)]
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

    /// Parse duration from string like "1m", "1h", "1d".
    pub fn parse_duration(s: &str) -> Option<Duration> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        let (num_str, unit) = s.split_at(s.len() - 1);
        let num: u64 = num_str.parse().ok()?;

        match unit {
            "s" => Some(Duration::from_secs(num)),
            "m" => Some(Duration::from_secs(num * 60)),
            "h" => Some(Duration::from_secs(num * 3600)),
            "d" => Some(Duration::from_secs(num * 86400)),
            _ => None,
        }
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
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_key() {
        assert_eq!(RateLimitKey::User.as_str(), "user");
        assert_eq!(RateLimitKey::Ip.as_str(), "ip");
        assert_eq!(RateLimitKey::Global.as_str(), "global");

        assert_eq!("user".parse::<RateLimitKey>().unwrap(), RateLimitKey::User);
        assert_eq!("ip".parse::<RateLimitKey>().unwrap(), RateLimitKey::Ip);
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
}

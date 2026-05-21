use std::time::Duration;

use thiserror::Error;

use crate::workflow::SuspendReason;

/// Core error type mapping variants to HTTP status codes.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ForgeError {
    #[error("Configuration error: {context}")]
    Config {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Job cancelled: {0}")]
    JobCancelled(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Internal error: {context}")]
    Internal {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Rate limit exceeded: retry after {retry_after:?}")]
    RateLimitExceeded {
        retry_after: Duration,
        limit: u32,
        remaining: u32,
    },

    /// Service unavailable (503).
    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    /// Internal control signal raised when a workflow handler suspends
    /// (`ctx.sleep(...)` / `ctx.wait_for_event(...)`). The executor handles
    /// it before any HTTP mapping layer; it is never returned to a client.
    #[error("Workflow suspended")]
    WorkflowSuspended(SuspendReason),
}

impl ForgeError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        ForgeError::NotFound(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        ForgeError::Config {
            context: msg.into(),
            source: None,
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        ForgeError::Unauthorized(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        ForgeError::Forbidden(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        ForgeError::Validation(msg.into())
    }

    pub fn timeout(msg: impl Into<String>) -> Self {
        ForgeError::Timeout(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        ForgeError::Internal {
            context: msg.into(),
            source: None,
        }
    }

    pub fn internal_with(
        msg: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        ForgeError::Internal {
            context: msg.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn config_with(
        msg: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        ForgeError::Config {
            context: msg.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Canonical variant-to-HTTP-status mapping.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::Unauthorized(_) => 401,
            Self::Forbidden(_) => 403,
            Self::Validation(_) => 400,
            Self::InvalidArgument(_) => 400,
            Self::Deserialization(_) => 400,
            Self::Timeout(_) => 504,
            Self::RateLimitExceeded { .. } => 429,
            Self::JobCancelled(_) => 409,
            Self::ServiceUnavailable(_) => 503,
            _ => 500,
        }
    }

    pub fn is_client_error(&self) -> bool {
        let status = self.http_status();
        (400..500).contains(&status)
    }

    pub fn is_server_error(&self) -> bool {
        self.http_status() >= 500
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ServiceUnavailable(_) | Self::Timeout(_) | Self::RateLimitExceeded { .. }
        )
    }
}

impl From<serde_json::Error> for ForgeError {
    fn from(e: serde_json::Error) -> Self {
        ForgeError::Serialization(e.to_string())
    }
}

impl From<crate::http::CircuitBreakerError> for ForgeError {
    fn from(e: crate::http::CircuitBreakerError) -> Self {
        match e {
            crate::http::CircuitBreakerError::CircuitOpen(open) => {
                ForgeError::Timeout(open.to_string())
            }
            crate::http::CircuitBreakerError::Request(err) if err.is_timeout() => {
                ForgeError::Timeout(err.to_string())
            }
            crate::http::CircuitBreakerError::Request(err) => ForgeError::Internal {
                context: "HTTP request failed".to_string(),
                source: Some(Box::new(err)),
            },
            crate::http::CircuitBreakerError::PrivateHostBlocked(host) => {
                ForgeError::Forbidden(format!("Outbound request to private host '{host}' blocked"))
            }
        }
    }
}

/// Result type alias using ForgeError.
pub type Result<T> = std::result::Result<T, ForgeError>;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn display_preserves_inner_message() {
        let cases: Vec<(ForgeError, &str)> = vec![
            (
                ForgeError::config("bad toml"),
                "Configuration error: bad toml",
            ),
            (
                ForgeError::Database(sqlx::Error::RowNotFound),
                "Database error: no rows returned by a query that expected to return at least one row",
            ),
            (
                ForgeError::JobCancelled("user request".into()),
                "Job cancelled: user request",
            ),
            (
                ForgeError::Serialization("bad json".into()),
                "Serialization error: bad json",
            ),
            (
                ForgeError::Deserialization("missing field".into()),
                "Deserialization error: missing field",
            ),
            (
                ForgeError::InvalidArgument("negative id".into()),
                "Invalid argument: negative id",
            ),
            (ForgeError::NotFound("user 42".into()), "Not found: user 42"),
            (
                ForgeError::Unauthorized("expired token".into()),
                "Unauthorized: expired token",
            ),
            (
                ForgeError::Forbidden("admin only".into()),
                "Forbidden: admin only",
            ),
            (
                ForgeError::Validation("email required".into()),
                "Validation error: email required",
            ),
            (
                ForgeError::Timeout("5s exceeded".into()),
                "Timeout: 5s exceeded",
            ),
            (
                ForgeError::internal("null pointer"),
                "Internal error: null pointer",
            ),
            (
                ForgeError::InvalidState("already completed".into()),
                "Invalid state: already completed",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected, "Display mismatch for variant");
        }
    }

    #[test]
    fn display_rate_limit_includes_retry_after() {
        let err = ForgeError::RateLimitExceeded {
            retry_after: Duration::from_secs(30),
            limit: 100,
            remaining: 0,
        };
        let msg = err.to_string();
        assert!(msg.contains("30"), "Expected retry_after in message: {msg}");
    }

    #[test]
    fn from_serde_json_error_maps_to_serialization() {
        let bad_json = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let forge_err: ForgeError = bad_json.into();
        match forge_err {
            ForgeError::Serialization(msg) => assert!(!msg.is_empty()),
            other => panic!("Expected Serialization, got: {other:?}"),
        }
    }

    #[test]
    fn from_io_error_maps_to_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let forge_err: ForgeError = io_err.into();
        match forge_err {
            ForgeError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("Expected Io, got: {other:?}"),
        }
    }

    #[test]
    fn from_circuit_breaker_open_maps_to_timeout() {
        let open = crate::http::CircuitBreakerError::CircuitOpen(crate::http::CircuitBreakerOpen {
            host: "api.example.com".into(),
            retry_after: Duration::from_secs(60),
        });
        let forge_err: ForgeError = open.into();
        match forge_err {
            ForgeError::Timeout(msg) => {
                assert!(
                    msg.contains("api.example.com"),
                    "Expected host in message: {msg}"
                );
            }
            other => panic!("Expected Timeout, got: {other:?}"),
        }
    }

    #[test]
    fn variants_are_distinguishable_via_pattern_match() {
        let errors: Vec<ForgeError> = vec![
            ForgeError::NotFound("x".into()),
            ForgeError::Unauthorized("x".into()),
            ForgeError::Forbidden("x".into()),
            ForgeError::Validation("x".into()),
            ForgeError::InvalidArgument("x".into()),
            ForgeError::Timeout("x".into()),
            ForgeError::internal("x"),
        ];

        for (i, err) in errors.iter().enumerate() {
            let matched = match err {
                ForgeError::NotFound(_) => 0,
                ForgeError::Unauthorized(_) => 1,
                ForgeError::Forbidden(_) => 2,
                ForgeError::Validation(_) => 3,
                ForgeError::InvalidArgument(_) => 4,
                ForgeError::Timeout(_) => 5,
                ForgeError::Internal { .. } => 6,
                _ => usize::MAX,
            };
            assert_eq!(matched, i, "Variant at index {i} matched wrong pattern");
        }
    }

    #[test]
    fn rate_limit_fields_accessible() {
        let err = ForgeError::RateLimitExceeded {
            retry_after: Duration::from_secs(60),
            limit: 100,
            remaining: 0,
        };

        match err {
            ForgeError::RateLimitExceeded {
                retry_after,
                limit,
                remaining,
            } => {
                assert_eq!(retry_after, Duration::from_secs(60));
                assert_eq!(limit, 100);
                assert_eq!(remaining, 0);
            }
            _ => panic!("Expected RateLimitExceeded"),
        }
    }

    #[test]
    fn error_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ForgeError>();
        assert_sync::<ForgeError>();
    }

    #[test]
    fn http_status_returns_correct_codes() {
        assert_eq!(ForgeError::NotFound("x".into()).http_status(), 404);
        assert_eq!(ForgeError::Unauthorized("x".into()).http_status(), 401);
        assert_eq!(ForgeError::Forbidden("x".into()).http_status(), 403);
        assert_eq!(ForgeError::Validation("x".into()).http_status(), 400);
        assert_eq!(ForgeError::InvalidArgument("x".into()).http_status(), 400);
        assert_eq!(ForgeError::Deserialization("x".into()).http_status(), 400);
        assert_eq!(ForgeError::Timeout("x".into()).http_status(), 504);
        assert_eq!(ForgeError::JobCancelled("x".into()).http_status(), 409);
        assert_eq!(
            ForgeError::RateLimitExceeded {
                retry_after: Duration::from_secs(1),
                limit: 10,
                remaining: 0,
            }
            .http_status(),
            429
        );
        for err in [
            ForgeError::internal("x"),
            ForgeError::Database(sqlx::Error::RowNotFound),
            ForgeError::config("x"),
            ForgeError::InvalidState("x".into()),
        ] {
            assert_eq!(err.http_status(), 500, "expected 500 for {err:?}");
        }
    }

    #[test]
    fn is_client_error_for_4xx() {
        assert!(ForgeError::not_found("x").is_client_error());
        assert!(ForgeError::unauthorized("x").is_client_error());
        assert!(ForgeError::forbidden("x").is_client_error());
        assert!(ForgeError::validation("x").is_client_error());
        assert!(!ForgeError::internal("x").is_client_error());
        assert!(!ForgeError::timeout("x").is_client_error());
    }

    #[test]
    fn is_server_error_for_5xx() {
        assert!(ForgeError::internal("x").is_server_error());
        assert!(ForgeError::timeout("x").is_server_error());
        assert!(ForgeError::config("x").is_server_error());
        assert!(!ForgeError::not_found("x").is_server_error());
        assert!(!ForgeError::unauthorized("x").is_server_error());
    }

    #[test]
    fn is_retryable_for_transient_errors() {
        assert!(ForgeError::ServiceUnavailable("x".into()).is_retryable());
        assert!(ForgeError::timeout("x").is_retryable());
        assert!(
            ForgeError::RateLimitExceeded {
                retry_after: Duration::from_secs(1),
                limit: 10,
                remaining: 0,
            }
            .is_retryable()
        );
        assert!(!ForgeError::not_found("x").is_retryable());
        assert!(!ForgeError::internal("x").is_retryable());
        assert!(!ForgeError::validation("x").is_retryable());
    }

    #[test]
    fn internal_with_preserves_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broken");
        let err = ForgeError::internal_with("connection failed", io_err);
        assert_eq!(err.to_string(), "Internal error: connection failed");
        assert!(err.source().is_some(), "source should be preserved");
    }
}

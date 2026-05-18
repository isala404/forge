use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// RPC response for function calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RpcResponse {
    /// Whether the call succeeded.
    pub success: bool,
    /// Result data (if successful).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error information (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    /// Request ID for tracing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl RpcResponse {
    /// Create a successful response.
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            request_id: None,
        }
    }

    /// Create an error response.
    pub fn error(error: RpcError) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            request_id: None,
        }
    }

    /// Add request ID to the response.
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

impl IntoResponse for RpcResponse {
    fn into_response(self) -> Response {
        let status = if self.success {
            StatusCode::OK
        } else {
            self.error
                .as_ref()
                .map(|e| e.status_code())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        };

        let retry_after = self.error.as_ref().and_then(|e| e.retry_after_secs);

        let mut response = (status, Json(self)).into_response();
        if let Some(secs) = retry_after
            && let Ok(val) = axum::http::HeaderValue::from_str(&secs.to_string())
        {
            response.headers_mut().insert("Retry-After", val);
        }
        response
    }
}

/// RPC error information.
///
/// Wire shape: `{ code, message, retry_after_secs?, details? }`.
/// All frontend clients (SvelteKit, Dioxus) deserialize into this same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RpcError {
    /// Error code (e.g. `"NOT_FOUND"`, `"RATE_LIMITED"`).
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Seconds to wait before retrying. Set for `RATE_LIMITED` errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// Additional error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    // Not sent to clients; drives IntoResponse status selection.
    #[serde(skip)]
    http_status: u16,
}

impl RpcError {
    /// Create a new error. HTTP status is inferred from `code` via the standard mapping;
    /// prefer the typed constructors (`not_found`, `unauthorized`, etc.) or
    /// `From<ForgeError>` when the status must be exact.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let http_status = code_to_status(&code);
        Self {
            code,
            message: message.into(),
            retry_after_secs: None,
            details: None,
            http_status,
        }
    }

    /// Create an error with details.
    pub fn with_details(
        code: impl Into<String>,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        let mut e = Self::new(code, message);
        e.details = Some(details);
        e
    }

    /// Create a rate-limit error with the retry delay promoted to the top level.
    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self {
            code: "RATE_LIMITED".into(),
            message: "Rate limit exceeded".into(),
            retry_after_secs: Some(retry_after_secs),
            details: None,
            http_status: 429,
        }
    }

    /// Returns the HTTP status code for this error.
    pub fn status_code(&self) -> StatusCode {
        StatusCode::from_u16(self.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Create a not found error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("NOT_FOUND", message)
    }

    /// Create an unauthorized error.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new("UNAUTHORIZED", message)
    }

    /// Create a forbidden error.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new("FORBIDDEN", message)
    }

    /// Create a validation error.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new("VALIDATION_ERROR", message)
    }

    /// Create an internal error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("INTERNAL_ERROR", message)
    }
}

// Fallback for manually-constructed RpcError values (e.g. RpcError::new("NOT_FOUND", ...))
// that don't go through From<ForgeError>. Must stay in sync with ForgeError::http_status().
fn code_to_status(code: &str) -> u16 {
    match code {
        "NOT_FOUND" => 404,
        "UNAUTHORIZED" => 401,
        "FORBIDDEN" => 403,
        "VALIDATION_ERROR" | "INVALID_ARGUMENT" => 400,
        "TIMEOUT" => 504,
        "REQUEST_TIMEOUT" => 408,
        "RATE_LIMITED" => 429,
        "JOB_CANCELLED" | "CONFLICT" => 409,
        "UNPROCESSABLE_ENTITY" => 422,
        "SERVICE_UNAVAILABLE" => 503,
        "PAYLOAD_TOO_LARGE" | "RESULT_TOO_LARGE" => 413,
        "SUBSCRIPTION_GAPPED" => 410,
        "UNSUPPORTED_API_VERSION" => 406,
        _ => 500,
    }
}

impl From<forge_core::error::ForgeError> for RpcError {
    fn from(err: forge_core::error::ForgeError) -> Self {
        use forge_core::error::ForgeError as E;

        // Capture before the match consumes `err`; status lives on RpcError so
        // IntoResponse doesn't need to re-derive it from the string code.
        let http_status = err.http_status();

        let mut rpc = match err {
            E::NotFound(msg) => Self::not_found(msg),
            E::Unauthorized(msg) => Self::unauthorized(msg),
            E::Forbidden(msg) => Self::forbidden(msg),
            E::Validation(msg) => Self::validation(msg),
            E::InvalidArgument(msg) => Self::new("INVALID_ARGUMENT", msg),
            E::Timeout(msg) => Self::new("TIMEOUT", msg),
            E::JobCancelled(msg) => Self::new("JOB_CANCELLED", msg),
            E::Deserialization(msg) => {
                tracing::warn!(error = %msg, "Deserialization error in RPC handler");
                Self::new("INVALID_ARGUMENT", "Invalid input format")
            }
            ref e @ E::Database(_) => {
                tracing::error!(error = %e, "Database error in RPC handler");
                Self::internal("Internal server error")
            }
            ref e @ (E::Internal(_)
            | E::Serialization(_)
            | E::Config(_)
            | E::Io(_)
            | E::InvalidState(_)
            | E::WorkflowSuspended(_)) => {
                tracing::error!(error = %e, "Internal error in RPC handler");
                Self::internal("Internal server error")
            }
            E::Conflict(msg) => Self::new("CONFLICT", msg),
            E::UnprocessableEntity(msg) => Self::new("UNPROCESSABLE_ENTITY", msg),
            E::ServiceUnavailable(msg) => Self::new("SERVICE_UNAVAILABLE", msg),
            E::RateLimitExceeded { retry_after, .. } => Self::rate_limited(retry_after.as_secs()),
            ref e => {
                tracing::error!(error = %e, "Unmapped ForgeError variant");
                Self::internal("Internal server error")
            }
        };

        rpc.http_status = http_status;
        rpc
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use sqlx;

    #[test]
    fn test_success_response() {
        let resp = RpcResponse::success(serde_json::json!({"id": 1}));
        assert!(resp.success);
        assert!(resp.data.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_error_response() {
        let resp = RpcResponse::error(RpcError::not_found("User not found"));
        assert!(!resp.success);
        assert!(resp.data.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.as_ref().unwrap().code, "NOT_FOUND");
    }

    #[test]
    fn test_error_status_codes() {
        assert_eq!(RpcError::not_found("").status_code(), StatusCode::NOT_FOUND);
        assert_eq!(
            RpcError::unauthorized("").status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(RpcError::forbidden("").status_code(), StatusCode::FORBIDDEN);
        assert_eq!(
            RpcError::validation("").status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            RpcError::internal("").status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn test_with_request_id() {
        let resp = RpcResponse::success(serde_json::json!(null)).with_request_id("req-123");
        assert_eq!(resp.request_id, Some("req-123".to_string()));
    }

    // --- ForgeError -> RpcError conversion (HTTP boundary contract) ---

    #[test]
    fn forge_not_found_maps_to_not_found_404() {
        let rpc: RpcError = forge_core::ForgeError::NotFound("user 42".into()).into();
        assert_eq!(rpc.code, "NOT_FOUND");
        assert_eq!(rpc.message, "user 42");
        assert_eq!(rpc.status_code(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn forge_unauthorized_maps_to_401() {
        let rpc: RpcError = forge_core::ForgeError::Unauthorized("expired".into()).into();
        assert_eq!(rpc.code, "UNAUTHORIZED");
        assert_eq!(rpc.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn forge_forbidden_maps_to_403() {
        let rpc: RpcError = forge_core::ForgeError::Forbidden("admin only".into()).into();
        assert_eq!(rpc.code, "FORBIDDEN");
        assert_eq!(rpc.status_code(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn forge_validation_maps_to_400() {
        let rpc: RpcError = forge_core::ForgeError::Validation("email required".into()).into();
        assert_eq!(rpc.code, "VALIDATION_ERROR");
        assert_eq!(rpc.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn forge_invalid_argument_maps_to_400() {
        let rpc: RpcError = forge_core::ForgeError::InvalidArgument("negative id".into()).into();
        assert_eq!(rpc.code, "INVALID_ARGUMENT");
        assert_eq!(rpc.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn forge_timeout_maps_to_504() {
        let rpc: RpcError = forge_core::ForgeError::Timeout("5s".into()).into();
        assert_eq!(rpc.code, "TIMEOUT");
        assert_eq!(rpc.status_code(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[test]
    fn forge_job_cancelled_maps_to_409() {
        let rpc: RpcError = forge_core::ForgeError::JobCancelled("user request".into()).into();
        assert_eq!(rpc.code, "JOB_CANCELLED");
        assert_eq!(rpc.status_code(), StatusCode::CONFLICT);
    }

    #[test]
    fn forge_rate_limit_maps_to_429_with_retry_after() {
        let rpc: RpcError = forge_core::ForgeError::RateLimitExceeded {
            retry_after: std::time::Duration::from_secs(60),
            limit: 100,
            remaining: 0,
        }
        .into();
        assert_eq!(rpc.code, "RATE_LIMITED");
        assert_eq!(rpc.status_code(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rpc.retry_after_secs, Some(60));
    }

    #[test]
    fn forge_deserialization_hides_internal_details() {
        let rpc: RpcError =
            forge_core::ForgeError::Deserialization("missing field `id`".into()).into();
        assert_eq!(rpc.code, "INVALID_ARGUMENT");
        // Must NOT leak internal error details to clients
        assert_eq!(rpc.message, "Invalid input format");
        assert_eq!(rpc.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn forge_database_error_hides_internals() {
        let rpc: RpcError = forge_core::ForgeError::Database(sqlx::Error::RowNotFound).into();
        assert_eq!(rpc.code, "INTERNAL_ERROR");
        assert_eq!(rpc.message, "Internal server error");
        assert_eq!(rpc.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn forge_internal_variants_all_map_to_500() {
        let internals: Vec<forge_core::ForgeError> = vec![
            forge_core::ForgeError::Internal("oops".into()),
            forge_core::ForgeError::Serialization("bad".into()),
            forge_core::ForgeError::Config("bad toml".into()),
            forge_core::ForgeError::InvalidState("done".into()),
            forge_core::ForgeError::WorkflowSuspended(
                forge_core::workflow::SuspendReason::Sleep {
                    wake_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                },
            ),
        ];

        for err in internals {
            let rpc: RpcError = err.into();
            assert_eq!(
                rpc.status_code(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "Expected 500 for code: {}",
                rpc.code
            );
            // Must never leak internal details
            assert_eq!(rpc.message, "Internal server error");
        }
    }

    #[test]
    fn rpc_response_serialization_round_trip() {
        let resp = RpcResponse::success(serde_json::json!({"users": [1, 2, 3]}))
            .with_request_id("req-abc");
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: RpcResponse = serde_json::from_str(&json).unwrap();
        assert!(deserialized.success);
        assert_eq!(deserialized.request_id, Some("req-abc".to_string()));
        assert_eq!(deserialized.data.unwrap()["users"][0], 1);
    }

    #[test]
    fn rpc_error_with_details_serialization() {
        let err = RpcError::with_details(
            "CUSTOM_ERROR",
            "something broke",
            serde_json::json!({"field": "email"}),
        );
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: RpcError = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.code, "CUSTOM_ERROR");
        assert_eq!(deserialized.details.unwrap()["field"], "email");
    }
}

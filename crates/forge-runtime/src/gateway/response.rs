use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// RPC response for function calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

        (status, Json(self)).into_response()
    }
}

/// RPC error information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Additional error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl RpcError {
    /// Create a new error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Create an error with details.
    pub fn with_details(
        code: impl Into<String>,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Some(details),
        }
    }

    /// Get HTTP status code for this error.
    pub fn status_code(&self) -> StatusCode {
        match self.code.as_str() {
            "NOT_FOUND" => StatusCode::NOT_FOUND,
            "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
            "FORBIDDEN" => StatusCode::FORBIDDEN,
            "VALIDATION_ERROR" => StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT" => StatusCode::BAD_REQUEST,
            "TIMEOUT" => StatusCode::GATEWAY_TIMEOUT,
            "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
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

impl From<forge_core::error::ForgeError> for RpcError {
    fn from(err: forge_core::error::ForgeError) -> Self {
        match err {
            forge_core::error::ForgeError::NotFound(msg) => Self::not_found(msg),
            forge_core::error::ForgeError::Unauthorized(msg) => Self::unauthorized(msg),
            forge_core::error::ForgeError::Forbidden(msg) => Self::forbidden(msg),
            forge_core::error::ForgeError::Validation(msg) => Self::validation(msg),
            forge_core::error::ForgeError::InvalidArgument(msg) => {
                Self::new("INVALID_ARGUMENT", msg)
            }
            forge_core::error::ForgeError::Timeout(msg) => Self::new("TIMEOUT", msg),
            forge_core::error::ForgeError::Database(msg) => {
                Self::internal(format!("Database error: {}", msg))
            }
            forge_core::error::ForgeError::Function(msg) => {
                Self::internal(format!("Function error: {}", msg))
            }
            _ => Self::internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

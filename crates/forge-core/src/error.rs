use std::time::Duration;

use thiserror::Error;

/// Core error type for FORGE operations.
#[derive(Error, Debug)]
pub enum ForgeError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Function error: {0}")]
    Function(String),

    #[error("Job error: {0}")]
    Job(String),

    #[error("Cluster error: {0}")]
    Cluster(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQL error: {0}")]
    Sql(#[from] sqlx::Error),

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

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Workflow suspended")]
    WorkflowSuspended,

    #[error("Rate limit exceeded: retry after {retry_after:?}")]
    RateLimitExceeded {
        retry_after: Duration,
        limit: u32,
        remaining: u32,
    },
}

impl From<serde_json::Error> for ForgeError {
    fn from(e: serde_json::Error) -> Self {
        ForgeError::Serialization(e.to_string())
    }
}

/// Result type alias using ForgeError.
pub type Result<T> = std::result::Result<T, ForgeError>;

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
    Serialization(#[from] serde_json::Error),

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
}

/// Result type alias using ForgeError.
pub type Result<T> = std::result::Result<T, ForgeError>;

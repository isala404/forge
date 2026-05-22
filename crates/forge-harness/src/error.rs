use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("forge error: {0}")]
    Forge(#[from] forge_core::ForgeError),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("serde error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("rpc call failed: code={code} message={message}")]
    Rpc {
        code: String,
        message: String,
        status: u16,
    },

    #[error("sse stream error: {0}")]
    Sse(String),

    #[error("timeout waiting for {what}")]
    Timeout { what: String },

    #[error("setup failed: {0}")]
    Setup(String),
}

impl HarnessError {
    pub fn setup(msg: impl Into<String>) -> Self {
        Self::Setup(msg.into())
    }

    pub fn sse(msg: impl Into<String>) -> Self {
        Self::Sse(msg.into())
    }

    pub fn timeout(what: impl Into<String>) -> Self {
        Self::Timeout { what: what.into() }
    }
}

//! Security configuration.

use serde::{Deserialize, Serialize};

/// Security configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct SecurityConfig {
    /// Secret key for signing.
    pub secret_key: Option<String>,
}

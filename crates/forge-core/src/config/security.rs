//! Security configuration.

use serde::{Deserialize, Serialize};

/// Security configuration.
#[derive(Clone, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct SecurityConfig {
    pub secret_key: Option<String>,
}

impl std::fmt::Debug for SecurityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityConfig")
            .field(
                "secret_key",
                &self.secret_key.as_ref().map(|_| "***redacted***"),
            )
            .finish()
    }
}

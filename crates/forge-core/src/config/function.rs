//! Function execution configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Function execution configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FunctionConfig {
    /// Maximum concurrent function executions.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    /// Function timeout duration (e.g. "30s", "5m").
    #[serde(default = "default_function_timeout")]
    pub timeout: DurationStr,

    /// Advisory memory limit per function execution (e.g. "512mb", "1gb").
    ///
    /// This value is exposed as configuration metadata for orchestrators
    /// (e.g., Kubernetes resource requests) and observability dashboards.
    /// It is not enforced at the process level since Rust does not provide
    /// per-function memory sandboxing. Use container-level limits for hard
    /// enforcement.
    #[serde(default = "default_memory_limit")]
    pub memory_limit: String,
}

impl Default for FunctionConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
            timeout: default_function_timeout(),
            memory_limit: default_memory_limit(),
        }
    }
}

impl FunctionConfig {
    /// Advisory memory limit in bytes, parsed from the size string.
    pub fn memory_limit_bytes(&self) -> crate::Result<usize> {
        crate::util::parse_size(&self.memory_limit).ok_or_else(|| {
            crate::ForgeError::config(format!(
                "invalid function.memory_limit '{}'. Expected a size like '512mb', '1gb', or '536870912'",
                self.memory_limit
            ))
        })
    }
}

fn default_max_concurrent() -> usize {
    1000
}

fn default_function_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
}

fn default_memory_limit() -> String {
    "512mb".to_string()
}

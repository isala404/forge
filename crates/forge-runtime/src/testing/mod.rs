//! Testing utilities for FORGE applications.
//!
//! Provides TestContext for integration tests, mocking utilities,
//! and test cluster support.

mod assertions;
mod context;
mod mock;

pub use assertions::*;
pub use context::{TestContext, TestContextBuilder};
pub use mock::{MockHttp, MockHttpBuilder, MockRequest, MockResponse};

use std::time::Duration;

/// Default test timeout.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default job test timeout.
pub const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(10);

/// Default workflow test timeout.
pub const DEFAULT_WORKFLOW_TIMEOUT: Duration = Duration::from_secs(60);

/// Test configuration.
#[derive(Debug, Clone)]
pub struct TestConfig {
    /// Database URL for tests.
    pub database_url: Option<String>,
    /// Whether to run in parallel.
    pub parallel: bool,
    /// Maximum database connections.
    pub max_connections: u32,
    /// Default test timeout.
    pub default_timeout: Duration,
    /// Job timeout.
    pub job_timeout: Duration,
    /// Workflow timeout.
    pub workflow_timeout: Duration,
    /// Whether to enable logging.
    pub logging: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            database_url: std::env::var("DATABASE_URL").ok(),
            parallel: true,
            max_connections: 50,
            default_timeout: DEFAULT_TEST_TIMEOUT,
            job_timeout: DEFAULT_JOB_TIMEOUT,
            workflow_timeout: DEFAULT_WORKFLOW_TIMEOUT,
            logging: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = TestConfig::default();
        assert!(config.parallel);
        assert_eq!(config.max_connections, 50);
        assert_eq!(config.default_timeout, Duration::from_secs(30));
    }
}

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, Result};

use super::types::DurationStr;

/// Database configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL.
    #[serde(default)]
    pub url: String,

    /// Connection pool size.
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,

    /// Pool checkout timeout duration (e.g. "30s", "1m").
    #[serde(default = "default_pool_timeout")]
    pub pool_timeout: DurationStr,

    /// Statement timeout duration (e.g. "30s", "5m").
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout: DurationStr,

    /// Read replica URLs for scaling reads.
    #[serde(default)]
    pub replica_urls: Vec<String>,

    /// Whether to route read queries to replicas.
    #[serde(default)]
    pub read_from_replica: bool,

    /// Replica pool size. When unset, defaults to `pool_size / 2`.
    #[serde(default)]
    pub replica_pool_size: Option<u32>,

    /// Minimum connections to keep alive in the pool (pre-warming).
    #[serde(default)]
    pub min_pool_size: u32,

    /// Run a health check query before handing out connections.
    /// Disabling this halves round-trips for read queries.
    #[serde(default = "default_true")]
    pub test_before_acquire: bool,

    /// Connection pool isolation configuration.
    #[serde(default)]
    pub pools: PoolsConfig,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            pool_size: default_pool_size(),
            pool_timeout: default_pool_timeout(),
            statement_timeout: default_statement_timeout(),
            replica_urls: Vec::new(),
            read_from_replica: false,
            replica_pool_size: None,
            min_pool_size: 0,
            test_before_acquire: true,
            pools: PoolsConfig::default(),
        }
    }
}

impl DatabaseConfig {
    /// Create a config with a database URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }

    /// Get the database URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Validate the database configuration.
    pub fn validate(&self) -> Result<()> {
        if self.url.is_empty() {
            return Err(ForgeError::Config(
                "database.url is required. \
                 Set database.url to a PostgreSQL connection string \
                 (e.g., \"postgres://user:pass@localhost/mydb\")."
                    .into(),
            ));
        }
        Ok(())
    }
}

fn default_pool_size() -> u32 {
    50
}

fn default_pool_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
}

fn default_statement_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
}

use super::default_true;

/// Pool isolation configuration for different workloads.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct PoolsConfig {
    /// Default pool for queries/mutations.
    #[serde(default)]
    pub default: Option<PoolConfig>,

    /// Pool for background jobs.
    #[serde(default)]
    pub jobs: Option<PoolConfig>,

    /// Pool for observability writes.
    #[serde(default)]
    pub observability: Option<PoolConfig>,

    /// Pool for long-running analytics.
    #[serde(default)]
    pub analytics: Option<PoolConfig>,
}

/// Individual pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PoolConfig {
    /// Pool size.
    pub size: u32,

    /// Checkout timeout duration (e.g. "30s", "1m").
    #[serde(default = "default_pool_timeout")]
    pub timeout: DurationStr,

    /// Statement timeout duration override (e.g. "30s", "5m").
    pub statement_timeout: Option<DurationStr>,

    /// Minimum connections to keep alive.
    #[serde(default)]
    pub min_size: u32,

    /// Run a health check query before handing out connections.
    #[serde(default = "default_true")]
    pub test_before_acquire: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_default_database_config() {
        let config = DatabaseConfig::default();
        assert_eq!(config.pool_size, 50);
        assert_eq!(config.pool_timeout.as_secs(), 30);
        assert!(config.url.is_empty());
    }

    #[test]
    fn test_new_config() {
        let config = DatabaseConfig::new("postgres://localhost/test");
        assert_eq!(config.url(), "postgres://localhost/test");
    }

    #[test]
    fn test_parse_config() {
        let toml = r#"
            url = "postgres://localhost/test"
            pool_size = 100
            replica_urls = ["postgres://replica1/test", "postgres://replica2/test"]
            read_from_replica = true
        "#;

        let config: DatabaseConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.pool_size, 100);
        assert_eq!(config.url(), "postgres://localhost/test");
        assert_eq!(config.replica_urls.len(), 2);
        assert!(config.read_from_replica);
    }

    #[test]
    fn test_validate_with_url() {
        let config = DatabaseConfig::new("postgres://localhost/test");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_url() {
        let config = DatabaseConfig::default();
        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("database.url is required"));
    }
}

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, Result};

use super::default_true;
use super::types::DurationStr;

/// Database configuration. One pool, no per-workload isolation: workload
/// separation belongs at the worker level, not the connection level. The
/// single-pool contention model and sizing formula are documented at the
/// runtime side in `forge_runtime::pg::pool` module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL.
    #[serde(default)]
    pub url: String,

    /// Connection pool size. Should be sized as
    /// `worker.max_concurrent + reactor cap + expected gateway concurrency
    /// + ~6 for persistent listeners, leader holds, and headroom`. See
    /// `forge_runtime::pg::pool` module docs.
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
            return Err(ForgeError::config(
                "database.url is required. \
                 Set database.url to a PostgreSQL connection string \
                 (e.g., \"postgres://user:pass@localhost/mydb\").",
            ));
        }
        Ok(())
    }
}

fn default_pool_size() -> u32 {
    // Internal baseline with all defaults:
    //   14 worker slots (8 default + 4 workflows + 2 cron)
    //  +64 reactor max-concurrent re-executions
    //  + 6 persistent listeners, leader holds, health check, migration
    // = 84 connections consumed before any gateway traffic arrives.
    // 16 added on top as headroom for light gateway traffic, landing at 100.
    // Users running at scale should set pool_size explicitly based on their
    // expected concurrent gateway load; see the sizing formula in
    // `forge_runtime::pg::pool`.
    100
}

fn default_pool_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
}

fn default_statement_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_default_database_config() {
        let config = DatabaseConfig::default();
        assert_eq!(config.pool_size, 100);
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

    #[test]
    fn test_rejects_legacy_pools_blocks() {
        let toml = r#"
            url = "postgres://localhost/test"
            [pools.jobs]
            size = 10
        "#;
        let err = toml::from_str::<DatabaseConfig>(toml).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field"),
            "expected unknown-field error, got: {msg}"
        );
    }
}

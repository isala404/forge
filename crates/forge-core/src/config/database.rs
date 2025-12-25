use serde::{Deserialize, Serialize};

/// Database configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Primary database connection URL.
    pub url: String,

    /// Connection pool size.
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,

    /// Pool checkout timeout in seconds.
    #[serde(default = "default_pool_timeout")]
    pub pool_timeout_secs: u64,

    /// Statement timeout in seconds.
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_secs: u64,

    /// Read replica URLs for scaling reads.
    #[serde(default)]
    pub replica_urls: Vec<String>,

    /// Whether to route read queries to replicas.
    #[serde(default)]
    pub read_from_replica: bool,

    /// Connection pool isolation configuration.
    #[serde(default)]
    pub pools: PoolsConfig,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            pool_size: default_pool_size(),
            pool_timeout_secs: default_pool_timeout(),
            statement_timeout_secs: default_statement_timeout(),
            replica_urls: Vec::new(),
            read_from_replica: false,
            pools: PoolsConfig::default(),
        }
    }
}

fn default_pool_size() -> u32 {
    50
}

fn default_pool_timeout() -> u64 {
    30
}

fn default_statement_timeout() -> u64 {
    30
}

/// Pool isolation configuration for different workloads.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
pub struct PoolConfig {
    /// Pool size.
    pub size: u32,

    /// Checkout timeout in seconds.
    #[serde(default = "default_pool_timeout")]
    pub timeout_secs: u64,

    /// Statement timeout in seconds (optional override).
    pub statement_timeout_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_database_config() {
        let config = DatabaseConfig::default();
        assert_eq!(config.pool_size, 50);
        assert_eq!(config.pool_timeout_secs, 30);
    }

    #[test]
    fn test_parse_database_config() {
        let toml = r#"
            url = "postgres://localhost/test"
            pool_size = 100
            replica_urls = ["postgres://replica1/test", "postgres://replica2/test"]
            read_from_replica = true
        "#;

        let config: DatabaseConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.pool_size, 100);
        assert_eq!(config.replica_urls.len(), 2);
        assert!(config.read_from_replica);
    }
}

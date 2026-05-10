use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Cluster configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ClusterConfig {
    /// Cluster name.
    #[serde(default = "default_cluster_name")]
    pub name: String,

    /// Discovery method.
    #[serde(default)]
    pub discovery: DiscoveryMethod,

    /// Heartbeat interval duration (e.g. "5s", "10s").
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: DurationStr,

    /// Threshold duration for marking nodes as dead (e.g. "15s", "30s").
    #[serde(default = "default_dead_threshold")]
    pub dead_threshold: DurationStr,

    /// Static seed nodes (for static discovery).
    #[serde(default)]
    pub seed_nodes: Vec<String>,

    /// DNS name for discovery (for DNS discovery).
    pub dns_name: Option<String>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            name: default_cluster_name(),
            discovery: DiscoveryMethod::default(),
            heartbeat_interval: default_heartbeat_interval(),
            dead_threshold: default_dead_threshold(),
            seed_nodes: Vec::new(),
            dns_name: None,
        }
    }
}

impl ClusterConfig {
    /// Return a copy with `dns_name` set.
    pub fn with_dns_name(mut self, dns_name: String) -> Self {
        self.dns_name = Some(dns_name);
        self
    }
}

fn default_cluster_name() -> String {
    "default".to_string()
}

fn default_heartbeat_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(5))
}

fn default_dead_threshold() -> DurationStr {
    DurationStr::new(Duration::from_secs(15))
}

/// Cluster discovery method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryMethod {
    /// Use PostgreSQL table for discovery.
    #[default]
    Postgres,

    /// Use DNS for discovery.
    Dns,

    /// Use Kubernetes for discovery.
    Kubernetes,

    /// Use static seed nodes.
    Static,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_default_cluster_config() {
        let config = ClusterConfig::default();
        assert_eq!(config.name, "default");
        assert_eq!(config.discovery, DiscoveryMethod::Postgres);
        assert_eq!(config.heartbeat_interval.as_secs(), 5);
    }

    #[test]
    fn test_parse_cluster_config() {
        let toml = r#"
            name = "production"
            discovery = "kubernetes"
            heartbeat_interval = "10s"
        "#;

        let config: ClusterConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.name, "production");
        assert_eq!(config.discovery, DiscoveryMethod::Kubernetes);
    }
}

use serde::{Deserialize, Serialize};

/// Cluster configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Cluster name.
    #[serde(default = "default_cluster_name")]
    pub name: String,

    /// Discovery method.
    #[serde(default)]
    pub discovery: DiscoveryMethod,

    /// Heartbeat interval in seconds.
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,

    /// Threshold for marking nodes as dead (in seconds).
    #[serde(default = "default_dead_threshold")]
    pub dead_threshold_secs: u64,

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
            heartbeat_interval_secs: default_heartbeat_interval(),
            dead_threshold_secs: default_dead_threshold(),
            seed_nodes: Vec::new(),
            dns_name: None,
        }
    }
}

fn default_cluster_name() -> String {
    "default".to_string()
}

fn default_heartbeat_interval() -> u64 {
    5
}

fn default_dead_threshold() -> u64 {
    15
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
mod tests {
    use super::*;

    #[test]
    fn test_default_cluster_config() {
        let config = ClusterConfig::default();
        assert_eq!(config.name, "default");
        assert_eq!(config.discovery, DiscoveryMethod::Postgres);
        assert_eq!(config.heartbeat_interval_secs, 5);
    }

    #[test]
    fn test_parse_cluster_config() {
        let toml = r#"
            name = "production"
            discovery = "kubernetes"
            heartbeat_interval_secs = 10
        "#;

        let config: ClusterConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.name, "production");
        assert_eq!(config.discovery, DiscoveryMethod::Kubernetes);
    }
}

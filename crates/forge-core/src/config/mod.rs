//! Configuration types for the Forge framework.
//!
//! Each config section lives in its own sub-module for locality. The root
//! [`ForgeConfig`] struct ties them together and owns parsing, env-var
//! substitution, and cross-field validation.

mod auth;
pub mod cluster;
mod cron_config;
mod daemon_config;
mod database;
mod function;
mod gateway;
pub(crate) mod loader;
mod mcp_config;
mod node;
mod observability;
mod project;
mod rate_limit;
mod realtime_config;
mod security;
pub mod signals;
pub mod types;
mod worker;
mod workflow_config;

pub use auth::{AuthConfig, JwtAlgorithm, LegacySecret};
pub use cluster::ClusterConfig;
pub use cron_config::CronConfig;
pub use daemon_config::DaemonConfig;
pub use database::DatabaseConfig;
pub use function::FunctionConfig;
pub use gateway::{GatewayConfig, TlsConfig};
pub use mcp_config::McpConfig;
pub use node::{NodeConfig, NodeRole};
pub use observability::ObservabilityConfig;
pub use project::ProjectConfig;
pub use rate_limit::{RateLimitMode, RateLimitSettings};
pub use realtime_config::RealtimeConfig;
pub use security::SecurityConfig;
pub use signals::SignalsConfig;
pub use types::{DurationStr, SizeStr};
pub use worker::{CRON_QUEUE, DEFAULT_QUEUE, QueueWorkerConfig, WORKFLOWS_QUEUE, WorkerConfig};
pub use workflow_config::WorkflowConfig;

pub use loader::substitute_env_vars;

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{ForgeError, Result};

/// Root configuration for FORGE.
///
/// Configuration is loaded from a TOML file (typically `forge.toml`) and supports
/// `${ENV_VAR}` and `${VAR-default}` substitution throughout. Only `database.url`
/// is required; every other section has a sensible default.
///
/// # Examples
///
/// Parse a minimal TOML snippet:
///
/// ```
/// use forge_core::config::ForgeConfig;
///
/// let config = ForgeConfig::parse_toml(r#"
///     [database]
///     url = "postgres://localhost/myapp"
/// "#).unwrap();
///
/// assert_eq!(config.database.url(), "postgres://localhost/myapp");
/// assert_eq!(config.gateway.port, 9081); // default port
/// ```
///
/// Construct programmatically (useful in tests):
///
/// ```
/// use forge_core::config::ForgeConfig;
///
/// let config = ForgeConfig::default_with_database_url("postgres://localhost/test");
/// assert_eq!(config.database.url(), "postgres://localhost/test");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ForgeConfig {
    /// Project metadata.
    #[serde(default)]
    pub project: ProjectConfig,

    /// Database configuration.
    pub database: DatabaseConfig,

    /// Node configuration.
    #[serde(default)]
    pub node: NodeConfig,

    /// Gateway configuration.
    #[serde(default)]
    pub gateway: GatewayConfig,

    /// Function execution configuration.
    #[serde(default)]
    pub function: FunctionConfig,

    /// Worker configuration.
    #[serde(default)]
    pub worker: WorkerConfig,

    /// Workflow scheduler configuration.
    #[serde(default)]
    pub workflow: WorkflowConfig,

    /// Cron scheduler configuration.
    #[serde(default)]
    pub cron: CronConfig,

    /// Daemon runner configuration.
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// Cluster configuration.
    #[serde(default)]
    pub cluster: ClusterConfig,

    /// Security configuration.
    #[serde(default)]
    pub security: SecurityConfig,

    /// Authentication configuration.
    #[serde(default)]
    pub auth: AuthConfig,

    /// Observability configuration.
    #[serde(default)]
    pub observability: ObservabilityConfig,

    /// MCP server configuration.
    #[serde(default)]
    pub mcp: McpConfig,

    /// Signals configuration for product analytics and diagnostics.
    #[serde(default)]
    pub signals: SignalsConfig,

    /// Rate-limiter configuration.
    #[serde(default)]
    pub rate_limit: RateLimitSettings,

    /// Real-time subscription and SSE knobs.
    #[serde(default)]
    pub realtime: RealtimeConfig,
}

impl ForgeConfig {
    /// Load configuration from a TOML file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .map_err(|e| ForgeError::Config(format!("Failed to read config file: {}", e)))?;

        Self::parse_toml(&content)
    }

    /// Parse configuration from a TOML string.
    pub fn parse_toml(content: &str) -> Result<Self> {
        let content = loader::substitute_env_vars(content);

        let config: Self = toml::from_str(&content)
            .map_err(|e| ForgeError::Config(format!("Failed to parse config: {}", e)))?;

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration for invalid combinations.
    pub fn validate(&self) -> Result<()> {
        self.database.validate()?;
        self.auth.validate()?;
        self.mcp.validate()?;
        let body_limit = self.gateway.max_body_size.as_bytes();
        let file_limit = self.gateway.max_file_size.as_bytes();
        if file_limit > body_limit {
            return Err(ForgeError::Config(format!(
                "gateway.max_file_size ({}) cannot exceed gateway.max_body_size ({})",
                self.gateway.max_file_size, self.gateway.max_body_size
            )));
        }
        self.gateway.tls.validate()?;

        // Cross-field: OAuth requires jwt_secret for signing tokens
        if self.mcp.oauth && self.auth.jwt_secret.is_none() {
            return Err(ForgeError::Config(
                "mcp.oauth = true requires auth.jwt_secret to be set. \
                 OAuth-issued tokens are signed with this secret, even when using \
                 an external provider (JWKS) for identity verification."
                    .into(),
            ));
        }
        if self.mcp.oauth && !self.mcp.enabled {
            return Err(ForgeError::Config(
                "mcp.oauth = true requires mcp.enabled = true".into(),
            ));
        }

        if !self.gateway.cors_enabled && !self.gateway.cors_origins.is_empty() {
            return Err(ForgeError::Config(
                "gateway.cors_origins is set but gateway.cors_enabled = false. \
                 Set cors_enabled = true to activate CORS, or remove cors_origins."
                    .into(),
            ));
        }

        if self.gateway.cors_enabled {
            if self.gateway.cors_origins.is_empty() {
                return Err(ForgeError::Config(
                    "gateway.cors_enabled = true requires at least one origin. \
                     Use cors_origins = [\"*\"] to allow any origin."
                        .into(),
                ));
            }
            // Wildcard mixed with concrete origins is ignored by browsers on
            // credentialed requests and signals a misconfiguration.
            let has_wildcard = self.gateway.cors_origins.iter().any(|o| o == "*");
            let has_concrete = self.gateway.cors_origins.iter().any(|o| o != "*");
            if has_wildcard && has_concrete {
                return Err(ForgeError::Config(
                    "gateway.cors_origins cannot mix \"*\" with concrete origins. \
                     Browsers ignore wildcards on credentialed requests."
                        .into(),
                ));
            }

            for origin in &self.gateway.cors_origins {
                if origin == "*" {
                    continue;
                }
                if origin.bytes().any(|b| b < 32 || b == 127) {
                    return Err(ForgeError::Config(format!(
                        "gateway.cors_origins contains invalid origin \"{origin}\". \
                         Origins must be valid HTTP header values."
                    )));
                }
                if !origin.starts_with("http://") && !origin.starts_with("https://") {
                    return Err(ForgeError::Config(format!(
                        "gateway.cors_origins contains \"{origin}\" which is not a valid origin. \
                         Origins must start with http:// or https://."
                    )));
                }
            }
        }

        if self.gateway.max_multipart_fields < 1 {
            return Err(ForgeError::Config(
                "gateway.max_multipart_fields must be at least 1".into(),
            ));
        }

        let quiet_ms = self.realtime.debounce_quiet_window.as_millis();
        let max_ms = self.realtime.debounce_max_wait.as_millis();
        if quiet_ms > max_ms {
            return Err(ForgeError::Config(format!(
                "realtime.debounce_quiet_window ({}) cannot exceed \
                 realtime.debounce_max_wait ({})",
                self.realtime.debounce_quiet_window, self.realtime.debounce_max_wait
            )));
        }

        for entry in &self.gateway.trusted_proxies {
            if entry.parse::<std::net::IpAddr>().is_err() && entry.parse::<ipnet::IpNet>().is_err()
            {
                return Err(ForgeError::Config(format!(
                    "gateway.trusted_proxies contains invalid entry \"{entry}\". \
                     Expected an IP address (e.g. \"10.0.0.1\") or CIDR range (e.g. \"10.0.0.0/8\")."
                )));
            }
        }

        Ok(())
    }

    /// Load configuration with defaults.
    pub fn default_with_database_url(url: &str) -> Self {
        Self {
            project: ProjectConfig::default(),
            database: DatabaseConfig::new(url),
            node: NodeConfig::default(),
            gateway: GatewayConfig::default(),
            function: FunctionConfig::default(),
            worker: WorkerConfig::default(),
            workflow: WorkflowConfig::default(),
            cron: CronConfig::default(),
            daemon: DaemonConfig::default(),
            cluster: ClusterConfig::default(),
            security: SecurityConfig::default(),
            auth: AuthConfig::default(),
            observability: ObservabilityConfig::default(),
            mcp: McpConfig::default(),
            signals: SignalsConfig::default(),
            rate_limit: RateLimitSettings::default(),
            realtime: RealtimeConfig::default(),
        }
    }
}

/// `default_true` serde helper re-exported for sub-modules.
pub(crate) fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, unsafe_code)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_default_config() {
        let config = ForgeConfig::default_with_database_url("postgres://localhost/test");
        assert_eq!(config.gateway.port, 9081);
        assert_eq!(config.node.roles.len(), 4);
        assert_eq!(config.mcp.path, "/mcp");
        assert!(!config.mcp.enabled);
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
            [database]
            url = "postgres://localhost/myapp"
        "#;

        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.database.url(), "postgres://localhost/myapp");
        assert_eq!(config.gateway.port, 9081);
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
            [project]
            name = "my-app"
            version = "1.0.0"

            [database]
            url = "postgres://localhost/myapp"
            pool_size = 100

            [node]
            roles = ["gateway", "worker"]
            worker_capabilities = ["media", "general"]

            [gateway]
            port = 3000
            grpc_port = 9001
        "#;

        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.project.name, "my-app");
        assert_eq!(config.database.pool_size, 100);
        assert_eq!(config.node.roles.len(), 2);
        assert_eq!(config.gateway.port, 3000);
    }

    #[test]
    fn test_env_var_substitution() {
        unsafe {
            std::env::set_var("TEST_DB_URL", "postgres://test:test@localhost/test");
        }

        let toml = r#"
            [database]
            url = "${TEST_DB_URL}"
        "#;

        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.database.url(), "postgres://test:test@localhost/test");

        unsafe {
            std::env::remove_var("TEST_DB_URL");
        }
    }

    #[test]
    fn test_auth_validation_no_config() {
        let auth = AuthConfig::default();
        assert!(auth.validate().is_ok());
    }

    #[test]
    fn test_auth_validation_hmac_with_secret() {
        let auth = AuthConfig {
            jwt_secret: Some("a-secret-long-enough-to-pass-the-32-byte-minimum".into()),
            jwt_algorithm: JwtAlgorithm::HS256,
            jwt_audience: Some("https://api.example.com".into()),
            ..Default::default()
        };
        assert!(auth.validate().is_ok());
    }

    #[test]
    fn test_auth_validation_hmac_missing_secret() {
        let auth = AuthConfig {
            jwt_issuer: Some("my-issuer".into()),
            jwt_algorithm: JwtAlgorithm::HS256,
            ..Default::default()
        };
        let result = auth.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("jwt_secret is required"));
    }

    #[test]
    fn test_auth_validation_rsa_with_jwks() {
        let auth = AuthConfig {
            jwks_url: Some("https://example.com/.well-known/jwks.json".into()),
            jwt_algorithm: JwtAlgorithm::RS256,
            jwt_audience: Some("https://api.example.com".into()),
            ..Default::default()
        };
        assert!(auth.validate().is_ok());
    }

    #[test]
    fn test_auth_validation_rsa_missing_jwks() {
        let auth = AuthConfig {
            jwt_issuer: Some("my-issuer".into()),
            jwt_algorithm: JwtAlgorithm::RS256,
            ..Default::default()
        };
        let result = auth.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("jwks_url is required"));
    }

    #[test]
    fn test_forge_config_validation_fails_on_empty_url() {
        let toml = r#"
            [database]

            url = ""
        "#;

        let result = ForgeConfig::parse_toml(toml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("database.url is required"));
    }

    #[test]
    fn test_forge_config_validation_fails_on_invalid_auth() {
        let toml = r#"
            [database]

            url = "postgres://localhost/test"

            [auth]
            jwt_issuer = "my-issuer"
            jwt_algorithm = "RS256"
        "#;

        let result = ForgeConfig::parse_toml(toml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("jwks_url is required"));
    }

    #[test]
    fn test_observability_config_default_disabled() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
        "#;

        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert!(!config.observability.enabled);
        assert!(!config.observability.otlp_active());
    }

    #[test]
    fn test_observability_config_with_env_default() {
        // Simulates what the template produces when no env vars are set
        unsafe {
            std::env::remove_var("TEST_OTEL_ENABLED");
        }

        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [observability]
            enabled = ${TEST_OTEL_ENABLED-false}
        "#;

        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert!(!config.observability.enabled);
    }

    #[test]
    fn test_mcp_config_validation_rejects_invalid_path() {
        let toml = r#"
            [database]

            url = "postgres://localhost/test"

            [mcp]
            enabled = true
            path = "mcp"
        "#;

        let result = ForgeConfig::parse_toml(toml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("mcp.path must start with '/'"));
    }

    #[test]
    fn test_access_token_ttl_defaults() {
        let auth = AuthConfig::default();
        assert_eq!(auth.access_token_ttl_secs(), 3600);
        assert_eq!(auth.refresh_token_ttl_days(), 30);
    }

    #[test]
    fn test_access_token_ttl_custom() {
        let auth = AuthConfig {
            access_token_ttl: Some(DurationStr::new(Duration::from_secs(900))),
            refresh_token_ttl: Some(DurationStr::new(Duration::from_secs(7 * 86400))),
            ..Default::default()
        };
        assert_eq!(auth.access_token_ttl_secs(), 900);
        assert_eq!(auth.refresh_token_ttl_days(), 7);
    }

    #[test]
    fn test_access_token_ttl_minimum_enforced() {
        let auth = AuthConfig {
            access_token_ttl: Some(DurationStr::new(Duration::from_secs(0))),
            ..Default::default()
        };
        // Should floor at 1, not 0
        assert_eq!(auth.access_token_ttl_secs(), 1);
    }

    #[test]
    fn test_refresh_token_ttl_minimum_enforced() {
        let auth = AuthConfig {
            refresh_token_ttl: Some(DurationStr::new(Duration::from_secs(3600))),
            ..Default::default()
        };
        // 1 hour < 1 day, so should floor at 1 day
        assert_eq!(auth.refresh_token_ttl_days(), 1);
    }

    #[test]
    fn test_max_body_size_defaults() {
        let gw = GatewayConfig::default();
        assert_eq!(gw.max_body_size.as_bytes(), 20 * 1024 * 1024);
    }

    #[test]
    fn test_max_body_size_custom() {
        let gw = GatewayConfig {
            max_body_size: SizeStr::new(100 * 1024 * 1024),
            ..Default::default()
        };
        assert_eq!(gw.max_body_size.as_bytes(), 100 * 1024 * 1024);
    }

    #[test]
    fn test_max_body_size_from_toml() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [gateway]
            max_body_size = "100mb"
        "#;
        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.gateway.max_body_size.as_bytes(), 100 * 1024 * 1024);
    }

    #[test]
    fn test_max_file_size_defaults() {
        let gw = GatewayConfig::default();
        assert_eq!(gw.max_file_size.as_bytes(), 10 * 1024 * 1024);
    }

    #[test]
    fn test_max_file_size_from_toml() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [gateway]
            max_body_size = "500mb"
            max_file_size = "200mb"
        "#;
        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.gateway.max_file_size.as_bytes(), 200 * 1024 * 1024);
    }

    #[test]
    fn test_validate_rejects_file_larger_than_body() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [gateway]
            max_body_size = "10mb"
            max_file_size = "20mb"
        "#;
        let err = ForgeConfig::parse_toml(toml).unwrap_err().to_string();
        assert!(
            err.contains("max_file_size"),
            "Expected max_file_size error, got: {err}"
        );
    }

    #[test]
    fn test_mcp_config_rejects_reserved_paths() {
        for reserved in McpConfig::RESERVED_PATHS {
            let toml = format!(
                r#"
                [database]
                url = "postgres://localhost/test"

                [mcp]
                enabled = true
                path = "{reserved}"
                "#
            );

            let result = ForgeConfig::parse_toml(&toml);
            assert!(result.is_err(), "Expected {reserved} to be rejected");
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("conflicts with a reserved gateway route"),
                "Wrong error for {reserved}: {err_msg}"
            );
        }
    }

    #[test]
    fn test_tls_disabled_default() {
        let config = ForgeConfig::default_with_database_url("postgres://localhost/test");
        assert!(!config.gateway.tls.is_enabled());
        assert!(config.gateway.tls.cert_path.is_none());
        assert!(config.gateway.tls.key_path.is_none());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_tls_file_based_valid() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [gateway.tls]
            cert_path = "/etc/forge/cert.pem"
            key_path = "/etc/forge/key.pem"
        "#;

        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert!(config.gateway.tls.is_enabled());
        assert_eq!(
            config.gateway.tls.cert_path.as_deref(),
            Some("/etc/forge/cert.pem")
        );
        assert_eq!(
            config.gateway.tls.key_path.as_deref(),
            Some("/etc/forge/key.pem")
        );
    }

    #[test]
    fn test_tls_only_cert_path_fails() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [gateway.tls]
            cert_path = "/etc/forge/cert.pem"
        "#;

        let result = ForgeConfig::parse_toml(toml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("key_path is missing"),
            "Unexpected error: {err_msg}"
        );
    }

    #[test]
    fn test_tls_only_key_path_fails() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [gateway.tls]
            key_path = "/etc/forge/key.pem"
        "#;

        let result = ForgeConfig::parse_toml(toml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cert_path is missing"),
            "Unexpected error: {err_msg}"
        );
    }

    #[test]
    fn test_tls_empty_strings_normalize_to_off() {
        // Env-var-driven deploys rely on `cert_path = "${FOO-}"` resolving
        // to `cert_path = ""` when the variable is unset. That must be
        // treated as "TLS off", not a validation error.
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [gateway.tls]
            cert_path = ""
            key_path = ""
        "#;

        let config = ForgeConfig::parse_toml(toml).expect("empty strings should normalize");
        assert!(!config.gateway.tls.is_enabled());
        assert!(config.gateway.tls.cert_path.is_none());
        assert!(config.gateway.tls.key_path.is_none());
    }

    #[test]
    fn test_tls_empty_cert_with_set_key_fails_as_half_set() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [gateway.tls]
            cert_path = ""
            key_path = "/etc/forge/key.pem"
        "#;

        let result = ForgeConfig::parse_toml(toml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cert_path is missing"),
            "Unexpected error: {err_msg}"
        );
    }

    #[test]
    fn jwt_secret_shorter_than_32_bytes_rejected() {
        let auth = AuthConfig {
            jwt_secret: Some("short".into()),
            jwt_algorithm: JwtAlgorithm::HS256,
            ..Default::default()
        };
        let err = auth.validate().unwrap_err().to_string();
        assert!(err.contains("32 bytes"), "{err}");
    }

    #[test]
    fn jwt_secret_32_bytes_accepted() {
        let auth = AuthConfig {
            jwt_secret: Some("a".repeat(32)),
            jwt_algorithm: JwtAlgorithm::HS256,
            jwt_audience: Some("https://api.example.com".into()),
            ..Default::default()
        };
        assert!(auth.validate().is_ok());
    }

    #[test]
    fn audience_required_fails_validate_when_missing() {
        let auth = AuthConfig {
            jwt_secret: Some("a-valid-32-byte-secret-for-tests!".into()),
            jwt_audience: None,
            audience_required: true,
            ..Default::default()
        };
        let err = auth.validate().unwrap_err().to_string();
        assert!(
            err.contains("jwt_audience"),
            "error should mention jwt_audience, got: {err}"
        );
    }

    #[test]
    fn audience_required_opt_out_passes_validate() {
        let auth = AuthConfig {
            jwt_secret: Some("a-valid-32-byte-secret-for-tests!".into()),
            jwt_audience: None,
            audience_required: false,
            ..Default::default()
        };
        assert!(auth.validate().is_ok());
    }

    #[test]
    fn cors_enabled_with_empty_origins_rejected() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [gateway]
            cors_enabled = true
        "#;
        let err = ForgeConfig::parse_toml(toml).unwrap_err().to_string();
        assert!(err.contains("cors_enabled"), "{err}");
    }

    #[test]
    fn cors_wildcard_only_accepted() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [gateway]
            cors_enabled = true
            cors_origins = ["*"]
        "#;
        assert!(ForgeConfig::parse_toml(toml).is_ok());
    }

    #[test]
    fn cors_mixed_wildcard_and_concrete_rejected() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [gateway]
            cors_enabled = true
            cors_origins = ["*", "https://example.com"]
        "#;
        let err = ForgeConfig::parse_toml(toml).unwrap_err().to_string();
        assert!(err.contains("cors_origins"), "{err}");
    }

    #[test]
    fn cors_disabled_does_not_require_origins() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [gateway]
            cors_enabled = false
        "#;
        assert!(ForgeConfig::parse_toml(toml).is_ok());
    }

    #[test]
    fn legacy_secrets_parse_with_valid_until_from_toml() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"

            [auth]
            jwt_secret = "active-secret-key-32-bytes-pad!!"
            jwt_audience = "https://api.example.com"

            [[auth.legacy_secrets]]
            secret = "retired-secret-key-32-bytes-pad!!"
            valid_until = "2099-01-01T00:00:00Z"
        "#;
        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.auth.legacy_secrets.len(), 1);
        let entry = &config.auth.legacy_secrets[0];
        assert_eq!(entry.secret, "retired-secret-key-32-bytes-pad!!");
        assert_eq!(entry.valid_until.to_rfc3339(), "2099-01-01T00:00:00+00:00");
    }

    #[test]
    fn realtime_quota_fields_parse_and_enforce() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [realtime]
            max_sessions_per_user = 4
            max_sessions_per_ip = 16
            max_subscriptions_per_user = 200
            max_cached_result_bytes = 1048576
        "#;
        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.realtime.max_sessions_per_user, 4);
        assert_eq!(config.realtime.max_sessions_per_ip, 16);
        assert_eq!(config.realtime.max_subscriptions_per_user, 200);
        assert_eq!(config.realtime.max_cached_result_bytes, 1024 * 1024);
    }
}

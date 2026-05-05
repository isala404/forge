//! Configuration types for the Forge framework.
//!
//! Each config section lives in its own sub-module for locality. The root
//! [`ForgeConfig`] struct ties them together and owns parsing, env-var
//! substitution, and cross-field validation.

mod auth;
pub mod cluster;
mod database;
mod function;
mod gateway;
mod mcp_config;
mod node;
mod observability;
mod project;
mod rate_limit;
mod realtime_config;
mod security;
pub mod signals;
mod worker;

pub use auth::{AuthConfig, JwtAlgorithm};
pub use cluster::ClusterConfig;
pub use database::{DatabaseConfig, PoolConfig};
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
pub use worker::WorkerConfig;

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{ForgeError, Result};

/// Root configuration for FORGE.
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
        reject_secret_defaults(content)?;

        // Substitute environment variables
        let content = substitute_env_vars(content);

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
        let body_limit = self.gateway.max_body_size_bytes()?;
        let file_limit = self.gateway.max_file_size_bytes()?;
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

        let quiet_ms = self.realtime.debounce_quiet_ms();
        let max_ms = self.realtime.debounce_max_ms();
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

        self.validate_durations()?;

        Ok(())
    }

    /// Validate all duration string fields parse correctly instead of silently
    /// falling back to defaults.
    fn validate_durations(&self) -> Result<()> {
        let fields: &[(&str, &str)] = &[
            ("gateway.request_timeout", &self.gateway.request_timeout),
            ("function.timeout", &self.function.timeout),
            ("worker.job_timeout", &self.worker.job_timeout),
            ("worker.poll_interval", &self.worker.poll_interval),
            ("auth.jwks_cache_ttl", &self.auth.jwks_cache_ttl),
            ("auth.session_ttl", &self.auth.session_ttl),
            ("auth.jwt_leeway", &self.auth.jwt_leeway),
            ("mcp.session_ttl", &self.mcp.session_ttl),
            (
                "observability.metrics_interval",
                &self.observability.metrics_interval,
            ),
            ("realtime.resync_interval", &self.realtime.resync_interval),
            (
                "realtime.debounce_quiet_window",
                &self.realtime.debounce_quiet_window,
            ),
            (
                "realtime.debounce_max_wait",
                &self.realtime.debounce_max_wait,
            ),
            (
                "cluster.heartbeat_interval",
                &self.cluster.heartbeat_interval,
            ),
            ("cluster.dead_threshold", &self.cluster.dead_threshold),
            ("database.pool_timeout", &self.database.pool_timeout),
            (
                "database.statement_timeout",
                &self.database.statement_timeout,
            ),
        ];

        for (name, value) in fields {
            if crate::util::parse_duration(value).is_none() {
                return Err(ForgeError::Config(format!(
                    "{name} = \"{value}\" is not a valid duration. \
                     Use a suffix like \"30s\", \"5m\", \"1h\", or \"200ms\"."
                )));
            }
        }

        let optional_fields: &[(&str, &Option<String>)] = &[
            ("auth.access_token_ttl", &self.auth.access_token_ttl),
            ("auth.refresh_token_ttl", &self.auth.refresh_token_ttl),
            ("auth.session_cookie_ttl", &self.auth.session_cookie_ttl),
        ];

        for (name, value) in optional_fields {
            if let Some(v) = value
                && crate::util::parse_duration(v).is_none()
            {
                return Err(ForgeError::Config(format!(
                    "{name} = \"{v}\" is not a valid duration. \
                     Use a suffix like \"30s\", \"5m\", \"1h\", or \"200ms\"."
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

// ---------------------------------------------------------------------------
// Duration / size parsing helpers (used by sub-module `impl` blocks via
// `super::parse_duration_secs` etc.)
// ---------------------------------------------------------------------------

pub(crate) fn parse_duration_secs(s: &str, default_secs: u64) -> u64 {
    crate::util::parse_duration(s)
        .map(|d| d.as_secs())
        .unwrap_or(default_secs)
}

pub(crate) fn parse_duration_millis(s: &str, default_ms: u64) -> u64 {
    crate::util::parse_duration(s)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(default_ms)
}

/// `default_true` serde helper re-exported for sub-modules.
pub(crate) fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Env-var substitution and secret-default rejection
// ---------------------------------------------------------------------------

/// Reject config patterns where secret-like env vars have hardcoded defaults.
/// Catches `${JWT_SECRET-my-default}` before it silently becomes a production secret.
fn reject_secret_defaults(content: &str) -> crate::Result<()> {
    const SECRET_KEYWORDS: &[&str] = &["secret", "password", "key"];

    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len
            && bytes.get(i) == Some(&b'$')
            && bytes.get(i + 1) == Some(&b'{')
            && let Some(end) = content.get(i + 2..).and_then(|s| s.find('}'))
        {
            let inner = &content[i + 2..i + 2 + end];
            let (var_name, default_value) = parse_var_with_default(inner);

            if default_value.is_some() {
                let var_lower = var_name.to_lowercase();
                for keyword in SECRET_KEYWORDS {
                    if var_lower.contains(keyword) {
                        return Err(ForgeError::Config(format!(
                            "${{{inner}}} uses a hardcoded default for a secret. \
                             Remove the default value and set {var_name} as an environment variable."
                        )));
                    }
                }
            }

            i += 2 + end + 1;
            continue;
        }
        i += 1;
    }

    Ok(())
}

/// Substitute environment variables in the format `${VAR_NAME}`.
///
/// Supports default values with `${VAR-default}` or `${VAR:-default}`.
/// When the env var is unset, the default is used. Without a default,
/// the literal `${VAR}` is preserved (so TOML parsing can still fail
/// loudly if a required variable is missing).
#[allow(clippy::indexing_slicing)]
pub fn substitute_env_vars(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len
            && bytes[i] == b'$'
            && bytes[i + 1] == b'{'
            && let Some(end) = content[i + 2..].find('}')
        {
            let inner = &content[i + 2..i + 2 + end];

            // Split on first `-` or `:-` for default value support
            let (var_name, default_value) = parse_var_with_default(inner);

            if is_valid_env_var_name(var_name) {
                if let Ok(value) = std::env::var(var_name) {
                    result.push_str(&value);
                } else if let Some(default) = default_value {
                    result.push_str(default);
                } else {
                    result.push_str(&content[i..i + 2 + end + 1]);
                }
                i += 2 + end + 1;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Parse `VAR-default` or `VAR:-default` into (name, optional default).
/// Both forms behave identically (fallback when unset). `:-` is checked
/// first so its `-` doesn't get matched by the plain `-` branch.
fn parse_var_with_default(inner: &str) -> (&str, Option<&str>) {
    if let Some(pos) = inner.find(":-") {
        return (&inner[..pos], Some(&inner[pos + 2..]));
    }
    if let Some(pos) = inner.find('-') {
        return (&inner[..pos], Some(&inner[pos + 1..]));
    }
    (inner, None)
}

fn is_valid_env_var_name(name: &str) -> bool {
    let first = match name.as_bytes().first() {
        Some(b) => b,
        None => return false,
    };
    (first.is_ascii_uppercase() || *first == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, unsafe_code)]
mod tests {
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
    fn test_env_var_default_used_when_unset() {
        // Ensure the var is definitely not set
        unsafe {
            std::env::remove_var("TEST_FORGE_OTEL_UNSET");
        }

        let input = r#"enabled = ${TEST_FORGE_OTEL_UNSET-false}"#;
        let result = substitute_env_vars(input);
        assert_eq!(result, "enabled = false");
    }

    #[test]
    fn test_env_var_default_overridden_when_set() {
        unsafe {
            std::env::set_var("TEST_FORGE_OTEL_SET", "true");
        }

        let input = r#"enabled = ${TEST_FORGE_OTEL_SET-false}"#;
        let result = substitute_env_vars(input);
        assert_eq!(result, "enabled = true");

        unsafe {
            std::env::remove_var("TEST_FORGE_OTEL_SET");
        }
    }

    #[test]
    fn test_env_var_colon_dash_default() {
        unsafe {
            std::env::remove_var("TEST_FORGE_ENDPOINT_UNSET");
        }

        let input = r#"endpoint = "${TEST_FORGE_ENDPOINT_UNSET:-http://localhost:4318}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"endpoint = "http://localhost:4318""#);
    }

    #[test]
    fn test_env_var_no_default_preserves_literal() {
        unsafe {
            std::env::remove_var("TEST_FORGE_MISSING");
        }

        let input = r#"url = "${TEST_FORGE_MISSING}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"url = "${TEST_FORGE_MISSING}""#);
    }

    #[test]
    fn test_env_var_default_empty_string() {
        unsafe {
            std::env::remove_var("TEST_FORGE_EMPTY_DEFAULT");
        }

        let input = r#"val = "${TEST_FORGE_EMPTY_DEFAULT-}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"val = """#);
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
            access_token_ttl: Some("15m".into()),
            refresh_token_ttl: Some("7d".into()),
            ..Default::default()
        };
        assert_eq!(auth.access_token_ttl_secs(), 900);
        assert_eq!(auth.refresh_token_ttl_days(), 7);
    }

    #[test]
    fn test_access_token_ttl_minimum_enforced() {
        let auth = AuthConfig {
            access_token_ttl: Some("0s".into()),
            ..Default::default()
        };
        // Should floor at 1, not 0
        assert_eq!(auth.access_token_ttl_secs(), 1);
    }

    #[test]
    fn test_refresh_token_ttl_minimum_enforced() {
        let auth = AuthConfig {
            refresh_token_ttl: Some("1h".into()),
            ..Default::default()
        };
        // 1 hour < 1 day, so should floor at 1 day
        assert_eq!(auth.refresh_token_ttl_days(), 1);
    }

    #[test]
    fn test_max_body_size_defaults() {
        let gw = GatewayConfig::default();
        assert_eq!(gw.max_body_size_bytes().unwrap(), 20 * 1024 * 1024);
    }

    #[test]
    fn test_max_body_size_custom() {
        let gw = GatewayConfig {
            max_body_size: "100mb".into(),
            ..Default::default()
        };
        assert_eq!(gw.max_body_size_bytes().unwrap(), 100 * 1024 * 1024);
    }

    #[test]
    fn test_max_body_size_invalid_errors() {
        let gw = GatewayConfig {
            max_body_size: "not-a-size".into(),
            ..Default::default()
        };
        assert!(gw.max_body_size_bytes().is_err());
    }

    #[test]
    fn test_max_file_size_defaults() {
        let gw = GatewayConfig::default();
        assert_eq!(gw.max_file_size_bytes().unwrap(), 10 * 1024 * 1024);
    }

    #[test]
    fn test_max_file_size_custom() {
        let gw = GatewayConfig {
            max_file_size: "200mb".into(),
            max_body_size: "500mb".into(),
            ..Default::default()
        };
        assert_eq!(gw.max_file_size_bytes().unwrap(), 200 * 1024 * 1024);
    }

    #[test]
    fn test_max_file_size_invalid_errors() {
        let gw = GatewayConfig {
            max_file_size: "nope".into(),
            ..Default::default()
        };
        assert!(gw.max_file_size_bytes().is_err());
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

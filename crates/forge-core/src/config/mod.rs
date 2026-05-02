pub mod cluster;
mod database;
pub mod signals;

pub use cluster::ClusterConfig;
pub use database::{DatabaseConfig, PoolConfig};
pub use signals::SignalsConfig;

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

/// Project metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProjectConfig {
    /// Project name.
    #[serde(default = "default_project_name")]
    pub name: String,

    /// Project version.
    #[serde(default = "default_version")]
    pub version: String,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            name: default_project_name(),
            version: default_version(),
        }
    }
}

fn default_project_name() -> String {
    "forge-app".to_string()
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// Node role configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeConfig {
    /// Roles this node should assume.
    #[serde(default = "default_roles")]
    pub roles: Vec<NodeRole>,

    /// Worker capabilities for job routing.
    #[serde(default = "default_capabilities")]
    pub worker_capabilities: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            roles: default_roles(),
            worker_capabilities: default_capabilities(),
        }
    }
}

fn default_roles() -> Vec<NodeRole> {
    vec![
        NodeRole::Gateway,
        NodeRole::Function,
        NodeRole::Worker,
        NodeRole::Scheduler,
    ]
}

fn default_capabilities() -> Vec<String> {
    vec!["general".to_string()]
}

/// Available node roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum NodeRole {
    Gateway,
    Function,
    Worker,
    Scheduler,
}

/// Gateway configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GatewayConfig {
    /// HTTP port.
    #[serde(default = "default_http_port")]
    pub port: u16,

    /// gRPC port for inter-node communication (reserved for future use).
    ///
    /// This port is registered in the cluster node info but a gRPC listener
    /// is not yet started. It will be used for efficient binary inter-node
    /// RPC in a future release.
    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,

    /// Maximum concurrent connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,

    /// Request timeout duration (e.g. "30s", "1m").
    #[serde(default = "default_request_timeout")]
    pub request_timeout: String,

    /// Enable CORS handling.
    #[serde(default = "default_cors_enabled")]
    pub cors_enabled: bool,

    /// Allowed CORS origins.
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,

    /// Routes excluded from request logs, metrics, and traces.
    /// Defaults to `["/_api/health", "/_api/ready"]`. Set to `[]` to monitor everything.
    #[serde(default = "default_quiet_paths")]
    pub quiet_paths: Vec<String>,

    /// Maximum request body size (e.g. "100mb", "1gb"). Defaults to "20mb".
    #[serde(default = "default_max_body_size")]
    pub max_body_size: String,

    /// Default per-file cap for multipart uploads (e.g. "10mb", "200mb").
    /// Applies when a mutation does not declare its own `max_size`. Set to
    /// the same value as `max_body_size` to disable the per-file guard.
    /// Defaults to "10mb".
    #[serde(default = "default_max_file_size")]
    pub max_file_size: String,

    /// TLS configuration for the gateway listener.
    #[serde(default)]
    pub tls: TlsConfig,

    /// Maximum requests in a single RPC batch call.
    #[serde(default = "default_max_rpc_batch_size")]
    pub max_rpc_batch_size: usize,

    /// Maximum file fields in a single multipart upload.
    #[serde(default = "default_max_multipart_fields")]
    pub max_multipart_fields: usize,

    /// Add standard security headers (X-Content-Type-Options, X-Frame-Options)
    /// to all responses.
    #[serde(default = "default_true")]
    pub security_headers: bool,

    /// Enable HTTP Strict Transport Security header. Off by default since
    /// local development uses plain HTTP.
    #[serde(default)]
    pub hsts: bool,

    /// IP ranges of trusted reverse proxies (e.g. `["10.0.0.0/8", "172.16.0.0/12"]`).
    /// When set, `X-Forwarded-For` is only trusted if the connecting peer IP
    /// matches one of these ranges. When empty (default), the peer socket IP
    /// is always used and forwarding headers are ignored.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_http_port(),
            grpc_port: default_grpc_port(),
            max_connections: default_max_connections(),
            request_timeout: default_request_timeout(),
            cors_enabled: default_cors_enabled(),
            cors_origins: default_cors_origins(),
            quiet_paths: default_quiet_paths(),
            max_body_size: default_max_body_size(),
            max_file_size: default_max_file_size(),
            tls: TlsConfig::default(),
            max_rpc_batch_size: default_max_rpc_batch_size(),
            max_multipart_fields: default_max_multipart_fields(),
            security_headers: true,
            hsts: false,
            trusted_proxies: Vec::new(),
        }
    }
}

impl GatewayConfig {
    /// Request timeout in seconds, parsed from the `request_timeout` string.
    pub fn request_timeout_secs(&self) -> u64 {
        parse_duration_secs(&self.request_timeout, 30)
    }

    /// Parse `max_body_size` into bytes.
    pub fn max_body_size_bytes(&self) -> crate::Result<usize> {
        crate::util::parse_size(&self.max_body_size).ok_or_else(|| {
            crate::ForgeError::Config(format!(
                "invalid gateway.max_body_size '{}'. Expected a size like '20mb', '1gb', or '1048576'",
                self.max_body_size
            ))
        })
    }

    /// Parse `max_file_size` into bytes.
    pub fn max_file_size_bytes(&self) -> crate::Result<usize> {
        crate::util::parse_size(&self.max_file_size).ok_or_else(|| {
            crate::ForgeError::Config(format!(
                "invalid gateway.max_file_size '{}'. Expected a size like '10mb', '200mb', or '1048576'",
                self.max_file_size
            ))
        })
    }
}

/// TLS configuration for the gateway listener.
///
/// TLS is enabled when both `cert_path` and `key_path` are set. Leave both
/// unset to serve plain HTTP. Setting only one is a configuration error.
///
/// Empty or whitespace-only strings normalize to unset at load time, so
/// env-var-driven configs like `cert_path = "${FORGE_TLS_CERT_PATH-}"`
/// treat an unset variable as "TLS off" instead of failing validation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Path to a PEM-encoded certificate chain file.
    #[serde(default, deserialize_with = "deserialize_optional_nonempty")]
    pub cert_path: Option<String>,

    /// Path to a PEM-encoded private key file.
    #[serde(default, deserialize_with = "deserialize_optional_nonempty")]
    pub key_path: Option<String>,
}

/// Deserialize an `Option<String>` treating empty / whitespace-only input as
/// `None`. Lets env-var-substituted fields with an empty default fall through
/// to "unset" semantics without tripping the half-set validator.
fn deserialize_optional_nonempty<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.trim().is_empty()))
}

impl TlsConfig {
    /// Return `true` when both `cert_path` and `key_path` are set.
    pub fn is_enabled(&self) -> bool {
        self.cert_path.is_some() && self.key_path.is_some()
    }

    /// Validate the TLS configuration: both paths or neither.
    pub fn validate(&self) -> crate::Result<()> {
        match (self.cert_path.as_deref(), self.key_path.as_deref()) {
            (Some(_), Some(_)) | (None, None) => Ok(()),
            (Some(_), None) => Err(crate::ForgeError::Config(
                "gateway.tls.cert_path is set but gateway.tls.key_path is missing. \
                 Set both to enable TLS, or neither to serve plain HTTP."
                    .into(),
            )),
            (None, Some(_)) => Err(crate::ForgeError::Config(
                "gateway.tls.key_path is set but gateway.tls.cert_path is missing. \
                 Set both to enable TLS, or neither to serve plain HTTP."
                    .into(),
            )),
        }
    }
}

fn default_http_port() -> u16 {
    9081
}

fn default_grpc_port() -> u16 {
    9000
}

fn default_max_connections() -> usize {
    4096
}

fn default_request_timeout() -> String {
    "30s".to_string()
}

fn default_cors_enabled() -> bool {
    false
}

fn default_cors_origins() -> Vec<String> {
    Vec::new()
}

fn default_quiet_paths() -> Vec<String> {
    vec![
        "/_api/health".to_string(),
        "/_api/ready".to_string(),
        "/_api/signal/event".to_string(),
        "/_api/signal/view".to_string(),
        "/_api/signal/user".to_string(),
        "/_api/signal/report".to_string(),
    ]
}

fn default_max_body_size() -> String {
    "20mb".to_string()
}

fn default_max_file_size() -> String {
    "10mb".to_string()
}

fn default_max_rpc_batch_size() -> usize {
    100
}

fn default_max_multipart_fields() -> usize {
    20
}

/// Function execution configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FunctionConfig {
    /// Maximum concurrent function executions.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    /// Function timeout duration (e.g. "30s", "5m").
    #[serde(default = "default_function_timeout")]
    pub timeout: String,

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
    /// Function timeout in seconds, parsed from the `timeout` string.
    pub fn timeout_secs(&self) -> u64 {
        parse_duration_secs(&self.timeout, 30)
    }

    /// Advisory memory limit in bytes, parsed from the size string.
    pub fn memory_limit_bytes(&self) -> crate::Result<usize> {
        crate::util::parse_size(&self.memory_limit).ok_or_else(|| {
            crate::ForgeError::Config(format!(
                "invalid function.memory_limit '{}'. Expected a size like '512mb', '1gb', or '536870912'",
                self.memory_limit
            ))
        })
    }
}

fn default_max_concurrent() -> usize {
    1000
}

fn default_function_timeout() -> String {
    "30s".to_string()
}

fn default_memory_limit() -> String {
    "512mb".to_string()
}

/// Worker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkerConfig {
    /// Maximum concurrent jobs.
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: usize,

    /// Job timeout duration (e.g. "1h", "30m").
    #[serde(default = "default_job_timeout")]
    pub job_timeout: String,

    /// Poll interval duration (e.g. "100ms", "1s").
    #[serde(default = "default_poll_interval")]
    pub poll_interval: String,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: default_max_concurrent_jobs(),
            job_timeout: default_job_timeout(),
            poll_interval: default_poll_interval(),
        }
    }
}

impl WorkerConfig {
    /// Job timeout in seconds, parsed from the `job_timeout` string.
    pub fn job_timeout_secs(&self) -> u64 {
        parse_duration_secs(&self.job_timeout, 3600)
    }

    /// Poll interval in milliseconds, parsed from the `poll_interval` string.
    pub fn poll_interval_ms(&self) -> u64 {
        parse_duration_millis(&self.poll_interval, 100)
    }
}

fn default_max_concurrent_jobs() -> usize {
    50
}

fn default_job_timeout() -> String {
    "1h".to_string()
}

fn default_poll_interval() -> String {
    "100ms".to_string()
}

/// Security configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct SecurityConfig {
    /// Secret key for signing.
    pub secret_key: Option<String>,
}

/// JWT signing algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
#[non_exhaustive]
pub enum JwtAlgorithm {
    /// HMAC using SHA-256 (symmetric, requires jwt_secret).
    #[default]
    HS256,
    /// HMAC using SHA-384 (symmetric, requires jwt_secret).
    HS384,
    /// HMAC using SHA-512 (symmetric, requires jwt_secret).
    HS512,
    /// RSA using SHA-256 (asymmetric, requires jwks_url).
    RS256,
    /// RSA using SHA-384 (asymmetric, requires jwks_url).
    RS384,
    /// RSA using SHA-512 (asymmetric, requires jwks_url).
    RS512,
}

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuthConfig {
    /// JWT secret for HMAC algorithms (HS256, HS384, HS512).
    /// Required when using HMAC algorithms.
    pub jwt_secret: Option<String>,

    /// JWT signing algorithm.
    /// HMAC algorithms (HS256, HS384, HS512) require jwt_secret.
    /// RSA algorithms (RS256, RS384, RS512) require jwks_url.
    #[serde(default)]
    pub jwt_algorithm: JwtAlgorithm,

    /// Expected token issuer (iss claim).
    /// If set, tokens with a different issuer are rejected.
    pub jwt_issuer: Option<String>,

    /// Expected audience (aud claim).
    /// If set, tokens with a different audience are rejected.
    pub jwt_audience: Option<String>,

    /// Access token lifetime (e.g., "15m", "1h").
    /// Used by `ctx.issue_token_pair()`. Defaults to "1h".
    pub access_token_ttl: Option<String>,

    /// Refresh token lifetime (e.g., "7d", "30d").
    /// Used by `ctx.issue_token_pair()`. Defaults to "30d".
    pub refresh_token_ttl: Option<String>,

    /// JWKS URL for RSA algorithms (RS256, RS384, RS512).
    /// Keys are fetched and cached automatically.
    pub jwks_url: Option<String>,

    /// JWKS cache TTL duration (e.g. "1h", "30m").
    #[serde(default = "default_jwks_cache_ttl")]
    pub jwks_cache_ttl: String,

    /// Session TTL duration (e.g. "7d", "24h"). Used for WebSocket sessions.
    #[serde(default = "default_session_ttl")]
    pub session_ttl: String,

    /// Clock-skew tolerance for `exp` / `nbf` validation (e.g. "60s", "5m").
    /// Sites with NTP-synchronized clocks can drop this to "5s"; older deployments
    /// or clients with drifting clocks may need higher. Defaults to "60s".
    #[serde(default = "default_jwt_leeway")]
    pub jwt_leeway: String,

    /// When `true` (default), `jwt_audience` must be set when auth is enabled.
    /// Set to `false` only during migration. Enforce it again once all clients
    /// send an `aud` claim.
    #[serde(default = "default_audience_required")]
    pub audience_required: bool,

    /// JWT spec claims that must be present in every token.
    /// Defaults to `["exp", "sub"]`. Add `"aud"` here if you want claim-level
    /// enforcement in addition to the `jwt_audience` equality check.
    #[serde(default = "default_required_claims")]
    pub required_claims: Vec<String>,

    /// Session cookie lifetime (e.g., "1h", "24h").
    /// Used for OAuth consent flow cookies. Defaults to the access token TTL.
    pub session_cookie_ttl: Option<String>,

    /// Old HMAC secrets still accepted for validation (never for signing).
    /// Rotate by adding the outgoing secret here, swapping `jwt_secret` to the
    /// new value, then removing it after one access-token TTL elapses.
    #[serde(default)]
    pub legacy_secrets: Vec<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: None,
            jwt_algorithm: JwtAlgorithm::default(),
            jwt_issuer: None,
            jwt_audience: None,
            access_token_ttl: None,
            refresh_token_ttl: None,
            jwks_url: None,
            jwks_cache_ttl: default_jwks_cache_ttl(),
            session_ttl: default_session_ttl(),
            jwt_leeway: default_jwt_leeway(),
            audience_required: default_audience_required(),
            required_claims: default_required_claims(),
            session_cookie_ttl: None,
            legacy_secrets: Vec::new(),
        }
    }
}

impl AuthConfig {
    /// JWKS cache TTL in seconds, parsed from the `jwks_cache_ttl` string.
    pub fn jwks_cache_ttl_secs(&self) -> u64 {
        parse_duration_secs(&self.jwks_cache_ttl, 3600)
    }

    /// Session TTL in seconds, parsed from the `session_ttl` string.
    pub fn session_ttl_secs(&self) -> u64 {
        parse_duration_secs(&self.session_ttl, 7 * 24 * 3600)
    }

    /// JWT clock-skew leeway in seconds, parsed from the `jwt_leeway` string.
    pub fn jwt_leeway_secs(&self) -> u64 {
        parse_duration_secs(&self.jwt_leeway, 60)
    }

    /// Resolved access token TTL in seconds.
    /// Parses `access_token_ttl`, default 3600s (1h).
    /// Minimum 1 second to prevent zero-lifetime tokens.
    pub fn access_token_ttl_secs(&self) -> i64 {
        self.access_token_ttl
            .as_deref()
            .and_then(crate::util::parse_duration)
            .map(|d| (d.as_secs() as i64).max(1))
            .unwrap_or(3600)
    }

    /// Resolved refresh token TTL in days.
    /// Parses `refresh_token_ttl`, default 30 days.
    pub fn refresh_token_ttl_days(&self) -> i64 {
        self.refresh_token_ttl
            .as_deref()
            .and_then(crate::util::parse_duration)
            .map(|d| (d.as_secs() / 86400) as i64)
            .map(|d| if d == 0 { 1 } else { d })
            .unwrap_or(30)
    }

    /// Resolved session cookie TTL in seconds.
    /// Falls back to `access_token_ttl_secs()` when not explicitly set.
    pub fn session_cookie_ttl_secs(&self) -> i64 {
        self.session_cookie_ttl
            .as_deref()
            .and_then(crate::util::parse_duration)
            .map(|d| (d.as_secs() as i64).max(1))
            .unwrap_or_else(|| self.access_token_ttl_secs())
    }

    /// Check if auth is configured (any credential or claim validation is set).
    pub fn is_configured(&self) -> bool {
        self.jwt_secret.is_some()
            || self.jwks_url.is_some()
            || self.jwt_issuer.is_some()
            || self.jwt_audience.is_some()
    }

    /// Validate that the configuration is complete for the chosen algorithm.
    /// Skips validation if no auth settings are configured (auth disabled).
    pub fn validate(&self) -> Result<()> {
        if !self.is_configured() {
            return Ok(());
        }

        match self.jwt_algorithm {
            JwtAlgorithm::HS256 | JwtAlgorithm::HS384 | JwtAlgorithm::HS512 => {
                if self.jwt_secret.is_none() {
                    return Err(ForgeError::Config(
                        "auth.jwt_secret is required for HMAC algorithms (HS256, HS384, HS512). \
                         Set auth.jwt_secret to a secure random string, \
                         or switch to RS256 and provide auth.jwks_url for external identity providers."
                            .into(),
                    ));
                }
                if let Some(secret) = &self.jwt_secret
                    && secret.len() < 32
                {
                    return Err(ForgeError::Config(format!(
                        "auth.jwt_secret is {} bytes but must be at least 32 bytes for HMAC \
                         to be collision-resistant. Generate one with: \
                         openssl rand -base64 32",
                        secret.len()
                    )));
                }
            }
            JwtAlgorithm::RS256 | JwtAlgorithm::RS384 | JwtAlgorithm::RS512 => {
                if self.jwks_url.is_none() {
                    return Err(ForgeError::Config(
                        "auth.jwks_url is required for RSA algorithms (RS256, RS384, RS512). \
                         Set auth.jwks_url to your identity provider's JWKS endpoint, \
                         or switch to HS256 and provide auth.jwt_secret for symmetric signing."
                            .into(),
                    ));
                }
            }
        }

        if self.audience_required && self.jwt_audience.is_none() {
            return Err(ForgeError::Config(
                "auth.jwt_audience is required when auth is enabled. \
                 Set auth.jwt_audience to your application's audience identifier (e.g. \"https://api.example.com\"), \
                 or set auth.audience_required = false to opt out during migration."
                    .into(),
            ));
        }

        Ok(())
    }

    /// Check if this config uses HMAC (symmetric) algorithms.
    pub fn is_hmac(&self) -> bool {
        matches!(
            self.jwt_algorithm,
            JwtAlgorithm::HS256 | JwtAlgorithm::HS384 | JwtAlgorithm::HS512
        )
    }

    /// Check if this config uses RSA (asymmetric) algorithms.
    pub fn is_rsa(&self) -> bool {
        matches!(
            self.jwt_algorithm,
            JwtAlgorithm::RS256 | JwtAlgorithm::RS384 | JwtAlgorithm::RS512
        )
    }
}

fn default_jwks_cache_ttl() -> String {
    "1h".to_string()
}

fn default_session_ttl() -> String {
    "7d".to_string()
}

fn default_jwt_leeway() -> String {
    "60s".to_string()
}

fn default_audience_required() -> bool {
    true
}

fn default_required_claims() -> Vec<String> {
    vec!["exp".into(), "sub".into()]
}

/// Observability configuration for OTLP telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ObservabilityConfig {
    /// Enable observability (traces, metrics, logs).
    #[serde(default)]
    pub enabled: bool,

    /// OTLP endpoint for telemetry export.
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,

    /// Service name for telemetry identification.
    pub service_name: Option<String>,

    /// Enable distributed tracing.
    #[serde(default = "default_true")]
    pub enable_traces: bool,

    /// Enable metrics collection.
    #[serde(default = "default_true")]
    pub enable_metrics: bool,

    /// Enable log export via OTLP.
    #[serde(default = "default_true")]
    pub enable_logs: bool,

    /// Trace sampling ratio (0.0 to 1.0).
    #[serde(default = "default_sampling_ratio")]
    pub sampling_ratio: f64,

    /// Metrics export interval duration (e.g. "15s", "1m"). OTLP collectors typically prefer 15s-60s.
    #[serde(default = "default_metrics_interval")]
    pub metrics_interval: String,

    /// Log level for the tracing subscriber (e.g., "debug", "info", "warn").
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: None,
            enable_traces: true,
            enable_metrics: true,
            enable_logs: true,
            sampling_ratio: default_sampling_ratio(),
            metrics_interval: default_metrics_interval(),
            log_level: default_log_level(),
        }
    }
}

impl ObservabilityConfig {
    /// Whether OTLP export is active (enabled + at least one signal on).
    pub fn otlp_active(&self) -> bool {
        self.enabled && (self.enable_traces || self.enable_metrics || self.enable_logs)
    }

    /// Metrics export interval in seconds, parsed from the `metrics_interval` string.
    pub fn metrics_interval_secs(&self) -> u64 {
        parse_duration_secs(&self.metrics_interval, 15)
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4318".to_string()
}

pub(crate) fn default_true() -> bool {
    true
}

/// Default trace sampling ratio. 100% so every span is visible out of the box.
/// Users can tune down for high-traffic production deployments.
fn default_sampling_ratio() -> f64 {
    1.0
}

fn default_metrics_interval() -> String {
    "15s".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct McpConfig {
    /// Enable MCP endpoint exposure.
    #[serde(default)]
    pub enabled: bool,

    /// Enable OAuth 2.1 Authorization Code + PKCE for MCP clients.
    /// When true, Forge acts as an OAuth 2.1 Authorization Server so MCP
    /// clients like Claude Code can auto-authenticate via browser login.
    /// Requires `auth.jwt_secret` to be set.
    #[serde(default)]
    pub oauth: bool,

    /// MCP endpoint path under the gateway API namespace.
    #[serde(default = "default_mcp_path")]
    pub path: String,

    /// Session TTL duration (e.g. "1h", "30m").
    #[serde(default = "default_mcp_session_ttl")]
    pub session_ttl: String,

    /// Allowed origins for Origin header validation.
    #[serde(default)]
    pub allowed_origins: Vec<String>,

    /// Enforce MCP-Protocol-Version header on post-initialize requests.
    #[serde(default = "default_true")]
    pub require_protocol_version_header: bool,

    /// Maximum total MCP sessions across all users.
    #[serde(default = "default_max_mcp_sessions")]
    pub max_sessions: usize,

    /// Maximum sessions a single authenticated user can hold.
    #[serde(default = "default_max_sessions_per_user")]
    pub max_sessions_per_user: usize,

    /// Allow unauthenticated dynamic client registration (RFC 7591).
    ///
    /// When **false** (default), `POST /_api/oauth/register` returns 403
    /// to anonymous callers. This blocks anyone on the internet from
    /// registering an OAuth client and being handed a `client_id` they
    /// can use to drive the authorization flow.
    ///
    /// Enable only if your trust model is "any caller may register a
    /// client" (typical for public IDE integrations behind a per-IP rate
    /// limit). Even when enabled, registrations remain capped by the
    /// `MAX_REGISTERED_CLIENTS` limit and the per-IP rate window.
    #[serde(default)]
    pub allow_unauthenticated_dcr: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            oauth: false,
            path: default_mcp_path(),
            session_ttl: default_mcp_session_ttl(),
            allowed_origins: Vec::new(),
            max_sessions: default_max_mcp_sessions(),
            max_sessions_per_user: default_max_sessions_per_user(),
            require_protocol_version_header: default_true(),
            allow_unauthenticated_dcr: false,
        }
    }
}

fn default_max_mcp_sessions() -> usize {
    10_000
}

fn default_max_sessions_per_user() -> usize {
    100
}

impl McpConfig {
    /// Paths reserved by the gateway that MCP must not collide with.
    const RESERVED_PATHS: &[&str] = &[
        "/health",
        "/ready",
        "/rpc",
        "/events",
        "/subscribe",
        "/unsubscribe",
        "/subscribe-job",
        "/subscribe-workflow",
        "/metrics",
    ];

    /// Session TTL in seconds, parsed from the `session_ttl` string.
    pub fn session_ttl_secs(&self) -> u64 {
        parse_duration_secs(&self.session_ttl, 3600)
    }

    pub fn validate(&self) -> Result<()> {
        if self.path.is_empty() || !self.path.starts_with('/') {
            return Err(ForgeError::Config(
                "mcp.path must start with '/' (example: /mcp)".to_string(),
            ));
        }
        if self.path.contains(' ') {
            return Err(ForgeError::Config(
                "mcp.path cannot contain spaces".to_string(),
            ));
        }
        if Self::RESERVED_PATHS.contains(&self.path.as_str()) {
            return Err(ForgeError::Config(format!(
                "mcp.path '{}' conflicts with a reserved gateway route",
                self.path
            )));
        }
        if crate::util::parse_duration(&self.session_ttl).is_none() {
            return Err(ForgeError::Config(format!(
                "mcp.session_ttl '{}' is not a valid duration (e.g. \"1h\", \"30m\")",
                self.session_ttl
            )));
        }
        if self.session_ttl_secs() == 0 {
            return Err(ForgeError::Config(
                "mcp.session_ttl must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
}

fn default_mcp_session_ttl() -> String {
    "1h".to_string()
}

fn parse_duration_secs(s: &str, default_secs: u64) -> u64 {
    crate::util::parse_duration(s)
        .map(|d| d.as_secs())
        .unwrap_or(default_secs)
}

fn parse_duration_millis(s: &str, default_ms: u64) -> u64 {
    crate::util::parse_duration(s)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(default_ms)
}

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

/// Rate-limiter mode selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitMode {
    /// Per-node DashMap fast path with PG fallback for `Global` keys.
    /// User/IP limits are approximate across N nodes — right for DDoS protection.
    #[default]
    Hybrid,
    /// Every check round-trips to PG. Cluster-wide correct — right for billing-grade quotas.
    Strict,
}

/// `[rate_limit]` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RateLimitSettings {
    /// Rate-limiter mode. Defaults to `hybrid`.
    #[serde(default)]
    pub mode: RateLimitMode,

    /// Maximum local (in-memory) rate limit buckets before eviction.
    #[serde(default = "default_max_local_buckets")]
    pub max_local_buckets: usize,
}

impl Default for RateLimitSettings {
    fn default() -> Self {
        Self {
            mode: RateLimitMode::default(),
            max_local_buckets: default_max_local_buckets(),
        }
    }
}

fn default_max_local_buckets() -> usize {
    100_000
}

/// Configuration for the real-time subscription engine and SSE transport.
///
/// All fields have production-safe defaults; only set these to tune behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RealtimeConfig {
    /// Maximum concurrent query re-executions during an invalidation flush.
    #[serde(default = "default_max_concurrent_reexecutions")]
    pub max_concurrent_reexecutions: usize,

    /// Periodic resync interval. Re-evaluates every active query group to recover
    /// from dropped NOTIFY payloads. "0s" disables the sweep (e.g. "60s", "5m").
    #[serde(default = "default_resync_interval")]
    pub resync_interval: String,

    /// Broadcast channel buffer for raw change notifications from PG.
    #[serde(
        default = "default_postgres_change_buffer_size",
        alias = "listener_channel_buffer"
    )]
    pub postgres_change_buffer_size: usize,

    /// Debounce quiet window duration. Changes arriving within this window are
    /// coalesced into a single flush (e.g. "50ms", "100ms").
    #[serde(default = "default_debounce_quiet_window", alias = "debounce_quiet")]
    pub debounce_quiet_window: String,

    /// Absolute maximum debounce wait before forcing a flush (e.g. "200ms", "1s").
    #[serde(default = "default_debounce_max_wait", alias = "debounce_max")]
    pub debounce_max_wait: String,

    /// Maximum concurrent SSE sessions across all clients.
    #[serde(default = "default_sse_max_sessions")]
    pub sse_max_sessions: usize,

    /// Maximum subscriptions per SSE session.
    #[serde(default = "default_subscription_max_per_session")]
    pub subscription_max_per_session: usize,

    /// Row-count threshold for switching from row-level to table-level
    /// change tracking per table. Lowering reduces memory; raising reduces
    /// false-positive invalidations on write-heavy tables.
    #[serde(
        default = "default_change_tracking_row_threshold",
        alias = "adaptive_row_threshold"
    )]
    pub change_tracking_row_threshold: usize,

    /// Number of DashMap shards for the subscription manager. Higher values
    /// reduce lock contention at the cost of memory.
    #[serde(default = "default_shard_count")]
    pub shard_count: usize,

    // -- Reserved 1.0 quota fields -------------------------------------------------
    // These names are reserved so apps can't squat on them with their own meaning.
    // Forge parses them today but does not act on them; behavior lands in 1.0.x.
    /// RESERVED. Maximum concurrent SSE sessions per authenticated user.
    /// Parsed today, not yet enforced; will be honored in a 1.0.x release.
    #[serde(default)]
    pub max_sessions_per_user: Option<usize>,

    /// RESERVED. Maximum concurrent SSE sessions per source IP.
    /// Parsed today, not yet enforced; will be honored in a 1.0.x release.
    #[serde(default)]
    pub max_sessions_per_ip: Option<usize>,

    /// RESERVED. Cap on a user's total subscriptions across every active session.
    /// Parsed today, not yet enforced; will be honored in a 1.0.x release.
    #[serde(default)]
    pub max_subscriptions_per_user: Option<usize>,

    /// RESERVED. Per-query cached-result memory ceiling (bytes). Cached results
    /// exceeding this size are dropped after re-execution. Parsed today, not yet
    /// enforced; will be honored in a 1.0.x release.
    #[serde(default)]
    pub max_cached_result_bytes: Option<usize>,

    /// RESERVED. Rate limit on `POST /_api/subscribe`. Parsed today, not yet
    /// enforced; will be honored in a 1.0.x release.
    #[serde(default)]
    pub subscribe_rate_limit: Option<RateLimit>,
}

/// Per-route rate limit specification used in `[realtime].subscribe_rate_limit`
/// and reserved for future config sections that gate request rates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RateLimit {
    /// Maximum requests per `per` window.
    pub requests: u32,
    /// Window duration string (e.g. `"1m"`, `"30s"`).
    pub per: String,
    /// Bucket key. One of `"user"`, `"ip"`, `"tenant"`, `"global"`.
    /// Defaults to `"user"` when omitted.
    #[serde(default)]
    pub key: Option<String>,
}

impl RateLimit {
    /// Parse the window duration into seconds.
    pub fn per_secs(&self) -> u64 {
        parse_duration_secs(&self.per, 60)
    }

    /// Resolve the bucket key, defaulting to `User`.
    pub fn rate_limit_key(&self) -> crate::rate_limit::RateLimitKey {
        self.key
            .as_deref()
            .and_then(|k| k.parse().ok())
            .unwrap_or_default()
    }
}

fn default_max_concurrent_reexecutions() -> usize {
    64
}
fn default_resync_interval() -> String {
    "60s".to_string()
}
fn default_postgres_change_buffer_size() -> usize {
    1024
}
fn default_debounce_quiet_window() -> String {
    "50ms".to_string()
}
fn default_debounce_max_wait() -> String {
    "200ms".to_string()
}
fn default_sse_max_sessions() -> usize {
    10_000
}
fn default_subscription_max_per_session() -> usize {
    100
}
fn default_change_tracking_row_threshold() -> usize {
    200
}
fn default_shard_count() -> usize {
    64
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_reexecutions: default_max_concurrent_reexecutions(),
            resync_interval: default_resync_interval(),
            postgres_change_buffer_size: default_postgres_change_buffer_size(),
            debounce_quiet_window: default_debounce_quiet_window(),
            debounce_max_wait: default_debounce_max_wait(),
            sse_max_sessions: default_sse_max_sessions(),
            subscription_max_per_session: default_subscription_max_per_session(),
            change_tracking_row_threshold: default_change_tracking_row_threshold(),
            shard_count: default_shard_count(),
            max_sessions_per_user: None,
            max_sessions_per_ip: None,
            max_subscriptions_per_user: None,
            max_cached_result_bytes: None,
            subscribe_rate_limit: None,
        }
    }
}

impl RealtimeConfig {
    /// Resync interval in seconds, parsed from the `resync_interval` string.
    pub fn resync_interval_secs(&self) -> u64 {
        parse_duration_secs(&self.resync_interval, 60)
    }

    /// Debounce quiet window in milliseconds, parsed from the `debounce_quiet_window` string.
    pub fn debounce_quiet_ms(&self) -> u64 {
        parse_duration_millis(&self.debounce_quiet_window, 50)
    }

    /// Absolute maximum debounce wait in milliseconds, parsed from the `debounce_max_wait` string.
    pub fn debounce_max_ms(&self) -> u64 {
        parse_duration_millis(&self.debounce_max_wait, 200)
    }
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
    fn reserved_realtime_quota_fields_parse_today() {
        // Reserved field names must be parseable now so 1.0.x can light them
        // up without a config-format break. They aren't enforced yet.
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [realtime]
            max_sessions_per_user = 4
            max_sessions_per_ip = 16
            max_subscriptions_per_user = 200
            max_cached_result_bytes = 1048576
            subscribe_rate_limit = { requests = 10, per = "1m", key = "user" }
        "#;
        let config = ForgeConfig::parse_toml(toml).unwrap();
        assert_eq!(config.realtime.max_sessions_per_user, Some(4));
        assert_eq!(config.realtime.max_sessions_per_ip, Some(16));
        assert_eq!(config.realtime.max_subscriptions_per_user, Some(200));
        assert_eq!(config.realtime.max_cached_result_bytes, Some(1024 * 1024));
        let rl = config.realtime.subscribe_rate_limit.unwrap();
        assert_eq!(rl.requests, 10);
        assert_eq!(rl.per, "1m");
        assert_eq!(rl.key.as_deref(), Some("user"));
    }
}

//! Gateway configuration for the HTTP listener, CORS, TLS, and request limits.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::default_true;
use super::types::{DurationStr, SizeStr};

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
    pub request_timeout: DurationStr,

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
    pub max_body_size: SizeStr,

    /// Default per-file cap for multipart uploads (e.g. "10mb", "200mb").
    /// Applies when a mutation does not declare its own `max_size`. Set to
    /// the same value as `max_body_size` to disable the per-file guard.
    /// Defaults to "10mb".
    #[serde(default = "default_max_file_size")]
    pub max_file_size: SizeStr,

    /// TLS configuration for the gateway listener.
    #[serde(default)]
    pub tls: TlsConfig,

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

    /// Maximum number of background jobs a single mutation request may dispatch.
    /// Prevents a single mutation from enqueuing an unbounded number of jobs and
    /// exhausting the job table. Defaults to 10.
    #[serde(default = "default_max_jobs_per_request")]
    pub max_jobs_per_request: usize,

    /// Maximum serialized response size in bytes. Responses exceeding this limit
    /// are rejected before being written to the wire. Defaults to 10 MiB.
    #[serde(default = "default_max_result_size_bytes")]
    pub max_result_size_bytes: usize,

    /// Maximum JSON nesting depth for incoming request bodies. Requests with
    /// deeper nesting are rejected before deserialization to prevent stack
    /// exhaustion. Defaults to 64.
    #[serde(default = "default_max_json_depth")]
    pub max_json_depth: usize,
    // TODO(pre-1.0): per-route CSP overrides (item 40)
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
            max_multipart_fields: default_max_multipart_fields(),
            security_headers: true,
            hsts: false,
            trusted_proxies: Vec::new(),
            max_jobs_per_request: default_max_jobs_per_request(),
            max_result_size_bytes: default_max_result_size_bytes(),
            max_json_depth: default_max_json_depth(),
        }
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

fn default_request_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
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
        "/_api/signal".to_string(),
    ]
}

fn default_max_body_size() -> SizeStr {
    SizeStr::new(20 * 1024 * 1024)
}

fn default_max_file_size() -> SizeStr {
    SizeStr::new(10 * 1024 * 1024)
}

fn default_max_multipart_fields() -> usize {
    20
}

fn default_max_jobs_per_request() -> usize {
    10
}

fn default_max_result_size_bytes() -> usize {
    10 * 1024 * 1024
}

fn default_max_json_depth() -> usize {
    64
}

//! MCP (Model Context Protocol) server configuration.

use std::time::Duration;

use crate::error::{ForgeError, Result};
use serde::{Deserialize, Serialize};

use super::default_true;
use super::types::DurationStr;

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
    pub session_ttl: DurationStr,

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

impl McpConfig {
    /// Paths reserved by the gateway that MCP must not collide with.
    pub(crate) const RESERVED_PATHS: &[&str] = &[
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

    /// Validate the MCP configuration.
    pub fn validate(&self) -> Result<()> {
        if self.path.is_empty() || !self.path.starts_with('/') {
            return Err(ForgeError::config(
                "mcp.path must start with '/' (example: /mcp)",
            ));
        }
        if self.path.contains(' ') {
            return Err(ForgeError::config("mcp.path cannot contain spaces"));
        }
        if Self::RESERVED_PATHS.contains(&self.path.as_str()) {
            return Err(ForgeError::config(format!(
                "mcp.path '{}' conflicts with a reserved gateway route",
                self.path
            )));
        }
        if self.session_ttl.as_secs() == 0 {
            return Err(ForgeError::config("mcp.session_ttl must be greater than 0"));
        }
        Ok(())
    }
}

fn default_max_mcp_sessions() -> usize {
    10_000
}

fn default_max_sessions_per_user() -> usize {
    100
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
}

fn default_mcp_session_ttl() -> DurationStr {
    DurationStr::new(Duration::from_secs(3600))
}

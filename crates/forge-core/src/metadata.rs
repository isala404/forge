//! Unified handler metadata for uniform consumers (observability, admin, codegen).
//!
//! Each handler trait keeps its typed `info()` for specific fields. This module
//! provides `HandlerMetadata` as a flat, kind-tagged view covering every
//! per-handler concept, so introspection tools and registries don't need to
//! branch on concrete types.

use std::time::Duration;

/// Discriminates which kind of handler produced a [`HandlerMetadata`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HandlerKind {
    Query,
    Mutation,
    Job,
    Cron,
    Workflow,
    Daemon,
    Webhook,
    McpTool,
}

impl std::fmt::Display for HandlerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
            Self::Job => "job",
            Self::Cron => "cron",
            Self::Workflow => "workflow",
            Self::Daemon => "daemon",
            Self::Webhook => "webhook",
            Self::McpTool => "mcp_tool",
        };
        f.write_str(s)
    }
}

/// Flat, kind-tagged handler descriptor that covers every per-handler concept.
///
/// Constructed via the `From` impls below or the `metadata()` default method
/// on each handler trait. Fields absent for a given kind are `None` or `false`.
///
/// Marked `#[non_exhaustive]` so adding new fields is non-breaking.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct HandlerMetadata {
    /// Which kind of handler this represents.
    pub kind: HandlerKind,
    /// Handler name used for routing and identification.
    pub name: String,
    /// Human-readable description (queries, mutations, MCP tools).
    pub description: Option<String>,
    /// Whether this handler is accessible without authentication.
    pub is_public: bool,
    /// Role required to call this handler (`None` = any authenticated user).
    pub required_role: Option<String>,
    /// Execution timeout for the handler itself.
    pub timeout: Option<Duration>,
    /// Default timeout applied to outbound HTTP requests.
    pub http_timeout: Option<Duration>,
    /// Cache TTL in seconds (queries only).
    pub cache_ttl: Option<u64>,
    /// Rate-limit: max requests allowed in the window.
    pub rate_limit_requests: Option<u32>,
    /// Rate-limit: window length in seconds.
    pub rate_limit_per_secs: Option<u64>,
    /// Rate-limit: bucket key type ("user", "ip", "tenant", "global", or custom).
    pub rate_limit_key: Option<String>,
    /// Access-log level override (queries/mutations).
    pub log_level: Option<String>,
    /// Tables this handler reads/writes, extracted at compile time.
    pub table_dependencies: Vec<String>,
    /// Columns referenced in SELECT clauses (queries only).
    pub selected_columns: Vec<String>,
    /// Whether the mutation runs inside a database transaction.
    pub transactional: bool,
    /// Whether the query always reads from the primary replica.
    pub consistent: bool,
    /// Per-handler upload size cap in bytes (mutations only).
    pub max_upload_size_bytes: Option<usize>,
    /// Cron schedule expression (crons only).
    pub cron_schedule: Option<String>,
    /// Cron timezone (crons only).
    pub cron_timezone: Option<String>,
    /// Workflow version string (workflows only).
    pub workflow_version: Option<String>,
    /// Workflow contract signature hash (workflows only).
    pub workflow_signature: Option<String>,
    /// Whether the daemon runs under leader election (daemons only).
    pub leader_elected: Option<bool>,
    /// URL path the webhook listens on (webhooks only).
    pub webhook_path: Option<String>,
}

impl From<&crate::function::FunctionInfo> for HandlerMetadata {
    fn from(info: &crate::function::FunctionInfo) -> Self {
        let kind = match info.kind {
            crate::function::FunctionKind::Query => HandlerKind::Query,
            crate::function::FunctionKind::Mutation => HandlerKind::Mutation,
            crate::function::FunctionKind::Webhook => HandlerKind::Webhook,
        };
        Self {
            kind,
            name: info.name.to_string(),
            description: info.description.map(str::to_string),
            is_public: info.is_public,
            required_role: info.required_role.map(str::to_string),
            timeout: info.timeout,
            http_timeout: info.http_timeout,
            cache_ttl: info.cache_ttl,
            rate_limit_requests: info.rate_limit_requests,
            rate_limit_per_secs: info.rate_limit_per_secs,
            rate_limit_key: info.rate_limit_key.as_ref().map(|k| k.as_str().to_string()),
            log_level: info.log_level.map(|l| l.as_str().to_string()),
            table_dependencies: info
                .table_dependencies
                .iter()
                .map(|s| s.to_string())
                .collect(),
            selected_columns: info
                .selected_columns
                .iter()
                .map(|s| s.to_string())
                .collect(),
            transactional: info.transactional,
            consistent: info.consistent,
            max_upload_size_bytes: info.max_upload_size_bytes,
            cron_schedule: None,
            cron_timezone: None,
            workflow_version: None,
            workflow_signature: None,
            leader_elected: None,
            webhook_path: None,
        }
    }
}

impl From<&crate::job::JobInfo> for HandlerMetadata {
    fn from(info: &crate::job::JobInfo) -> Self {
        Self {
            kind: HandlerKind::Job,
            name: info.name.to_string(),
            description: info.description.map(str::to_string),
            is_public: info.is_public,
            required_role: info.required_role.map(str::to_string),
            timeout: Some(info.timeout),
            http_timeout: info.http_timeout,
            cache_ttl: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: Vec::new(),
            selected_columns: Vec::new(),
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            cron_schedule: None,
            cron_timezone: None,
            workflow_version: None,
            workflow_signature: None,
            leader_elected: None,
            webhook_path: None,
        }
    }
}

impl From<&crate::cron::CronInfo> for HandlerMetadata {
    fn from(info: &crate::cron::CronInfo) -> Self {
        Self {
            kind: HandlerKind::Cron,
            name: info.name.to_string(),
            description: None,
            is_public: false,
            required_role: None,
            timeout: Some(info.timeout),
            http_timeout: info.http_timeout,
            cache_ttl: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: Vec::new(),
            selected_columns: Vec::new(),
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            cron_schedule: Some(info.schedule.expression().to_string()),
            cron_timezone: Some(info.timezone.to_string()),
            workflow_version: None,
            workflow_signature: None,
            leader_elected: None,
            webhook_path: None,
        }
    }
}

impl From<&crate::workflow::WorkflowInfo> for HandlerMetadata {
    fn from(info: &crate::workflow::WorkflowInfo) -> Self {
        Self {
            kind: HandlerKind::Workflow,
            name: info.name.to_string(),
            description: None,
            is_public: info.is_public,
            required_role: info.required_role.map(str::to_string),
            timeout: Some(info.timeout),
            http_timeout: info.http_timeout,
            cache_ttl: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: Vec::new(),
            selected_columns: Vec::new(),
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            cron_schedule: None,
            cron_timezone: None,
            workflow_version: Some(info.version.to_string()),
            workflow_signature: Some(info.signature.to_string()),
            leader_elected: None,
            webhook_path: None,
        }
    }
}

impl From<&crate::daemon::DaemonInfo> for HandlerMetadata {
    fn from(info: &crate::daemon::DaemonInfo) -> Self {
        Self {
            kind: HandlerKind::Daemon,
            name: info.name.to_string(),
            description: None,
            is_public: false,
            required_role: None,
            timeout: None,
            http_timeout: info.http_timeout,
            cache_ttl: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: Vec::new(),
            selected_columns: Vec::new(),
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            cron_schedule: None,
            cron_timezone: None,
            workflow_version: None,
            workflow_signature: None,
            leader_elected: Some(info.leader_elected),
            webhook_path: None,
        }
    }
}

impl From<&crate::webhook::WebhookInfo> for HandlerMetadata {
    fn from(info: &crate::webhook::WebhookInfo) -> Self {
        Self {
            kind: HandlerKind::Webhook,
            name: info.name.to_string(),
            description: info.description.map(str::to_string),
            is_public: true, // webhooks bypass auth by design
            required_role: None,
            timeout: Some(info.timeout),
            http_timeout: info.http_timeout,
            cache_ttl: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: Vec::new(),
            selected_columns: Vec::new(),
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            cron_schedule: None,
            cron_timezone: None,
            workflow_version: None,
            workflow_signature: None,
            leader_elected: None,
            webhook_path: Some(info.path.to_string()),
        }
    }
}

impl From<&crate::mcp::McpToolInfo> for HandlerMetadata {
    fn from(info: &crate::mcp::McpToolInfo) -> Self {
        Self {
            kind: HandlerKind::McpTool,
            name: info.name.to_string(),
            description: info.description.map(str::to_string),
            is_public: info.is_public,
            required_role: info.required_role.map(str::to_string),
            timeout: info.timeout,
            http_timeout: None,
            cache_ttl: None,
            rate_limit_requests: info.rate_limit_requests,
            rate_limit_per_secs: info.rate_limit_per_secs,
            rate_limit_key: info.rate_limit_key.map(str::to_string),
            log_level: None,
            table_dependencies: Vec::new(),
            selected_columns: Vec::new(),
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            cron_schedule: None,
            cron_timezone: None,
            workflow_version: None,
            workflow_signature: None,
            leader_elected: None,
            webhook_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{FunctionInfo, FunctionKind};

    #[test]
    fn test_handler_kind_display() {
        assert_eq!(HandlerKind::Query.to_string(), "query");
        assert_eq!(HandlerKind::Mutation.to_string(), "mutation");
        assert_eq!(HandlerKind::Job.to_string(), "job");
        assert_eq!(HandlerKind::Cron.to_string(), "cron");
        assert_eq!(HandlerKind::Workflow.to_string(), "workflow");
        assert_eq!(HandlerKind::Daemon.to_string(), "daemon");
        assert_eq!(HandlerKind::Webhook.to_string(), "webhook");
        assert_eq!(HandlerKind::McpTool.to_string(), "mcp_tool");
    }

    #[test]
    fn test_from_function_info_query() {
        let info = FunctionInfo {
            name: "get_user",
            description: Some("Get a user"),
            kind: FunctionKind::Query,
            required_role: None,
            is_public: false,
            cache_ttl: Some(60),
            timeout: Some(Duration::from_secs(10)),
            http_timeout: None,
            rate_limit_requests: Some(100),
            rate_limit_per_secs: Some(60),
            rate_limit_key: Some(crate::rate_limit::RateLimitKey::User),
            log_level: None,
            table_dependencies: &["users"],
            selected_columns: &["id", "name"],
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
        };

        let meta = HandlerMetadata::from(&info);
        assert_eq!(meta.kind, HandlerKind::Query);
        assert_eq!(meta.name, "get_user");
        assert_eq!(meta.cache_ttl, Some(60));
        assert_eq!(meta.table_dependencies, vec!["users"]);
        assert_eq!(meta.selected_columns, vec!["id", "name"]);
        assert_eq!(meta.rate_limit_key.as_deref(), Some("user"));
    }

    #[test]
    fn test_from_function_info_mutation() {
        let info = FunctionInfo {
            name: "create_user",
            description: None,
            kind: FunctionKind::Mutation,
            required_role: Some("admin"),
            is_public: false,
            cache_ttl: None,
            timeout: Some(Duration::from_secs(30)),
            http_timeout: Some(Duration::from_secs(5)),
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: &["users"],
            selected_columns: &[],
            transactional: true,
            consistent: false,
            max_upload_size_bytes: None,
        };

        let meta = HandlerMetadata::from(&info);
        assert_eq!(meta.kind, HandlerKind::Mutation);
        assert!(meta.transactional);
        assert_eq!(meta.required_role.as_deref(), Some("admin"));
    }

    #[test]
    fn test_from_job_info() {
        use std::time::Duration;
        let info = crate::job::JobInfo {
            name: "send_email",
            timeout: Duration::from_secs(120),
            is_public: false,
            required_role: None,
            ..crate::job::JobInfo::default()
        };

        let meta = HandlerMetadata::from(&info);
        assert_eq!(meta.kind, HandlerKind::Job);
        assert_eq!(meta.name, "send_email");
        assert_eq!(meta.timeout, Some(Duration::from_secs(120)));
    }

    #[test]
    fn test_from_daemon_info() {
        let info = crate::daemon::DaemonInfo {
            name: "cleanup",
            leader_elected: false,
            ..crate::daemon::DaemonInfo::default()
        };

        let meta = HandlerMetadata::from(&info);
        assert_eq!(meta.kind, HandlerKind::Daemon);
        assert_eq!(meta.leader_elected, Some(false));
    }
}

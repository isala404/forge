/// Internal sealing marker preventing manual handler-trait implementations.
#[doc(hidden)]
pub mod __sealed {
    pub trait Sealed {}
}

pub mod auth;
pub mod cluster;
pub mod config;
pub mod context;
pub mod cron;
pub mod daemon;
pub mod env;
pub mod error;
pub mod function;
pub mod http;
pub mod job;
pub mod mcp;
pub mod oauth;
pub mod pagination;
pub mod rate_limit;
pub mod realtime;
pub mod schema;
pub mod signals;
pub mod tenant;
pub mod types;
pub mod util;
pub mod webhook;
pub mod workflow;

pub mod testing;

pub use auth::{
    Claims, ClaimsBuilder, DefaultRoleResolver, RoleResolver, SharedRoleResolver, TokenPair,
    default_role_resolver,
};
pub use cluster::{ClusterInfo, LeaderInfo, LeaderRole, NodeId, NodeInfo, NodeRole, NodeStatus};
pub use config::{ForgeConfig, McpConfig, SignalsConfig};
pub use context::{AuthenticatedContext, HandlerContext};
pub use cron::{CronContext, CronInfo, CronSchedule, ForgeCron};
pub use daemon::{DaemonContext, DaemonInfo, DaemonStatus, ForgeDaemon};
pub use env::{EnvAccess, EnvProvider, MockEnvProvider, RealEnvProvider};
pub use error::{ForgeError, Result};
pub use function::{
    AuthContext, AuthTokenTtl, DbConn, ForgeConn, ForgeDb, ForgeMutation, ForgeQuery, FunctionInfo,
    FunctionKind, JobDispatch, KvHandle, LogLevel, MutationContext, QueryContext, RequestMetadata,
    TokenIssuer, WorkflowDispatch,
};
pub use http::{
    CircuitBreakerClient, CircuitBreakerConfig, CircuitBreakerError, CircuitBreakerOpen,
    CircuitState, CircuitStatus, HttpClient, HttpRequestBuilder,
};
pub use job::{ForgeJob, JobContext, JobInfo, JobPriority, JobStatus, RetryConfig};
pub use mcp::{
    ForgeMcpTool, McpContent, McpContentBlock, McpToolAnnotations, McpToolContext, McpToolIcon,
    McpToolInfo, McpToolResult,
};
pub use pagination::{Cursor, Page, PageInfo};
pub use rate_limit::{
    ParseRateLimitKeyError, RateLimitConfig, RateLimitHeaders, RateLimitKey, RateLimitResult,
};
pub use realtime::{
    AuthScope, Change, ChangeOperation, Delta, QueryGroup, QueryGroupId, ReadSet, SessionId,
    SessionInfo, SessionStatus, Subscriber, SubscriberId, SubscriptionId, SubscriptionState,
    TrackingMode,
};
pub use schema::{FieldDef, ModelMeta, SchemaRegistry, TableDef};
pub use schemars;
pub use tenant::{TenantContext, TenantIsolationMode};
pub use types::{Instant, LocalDate, LocalTime, Upload};
pub use webhook::{
    ForgeWebhook, IdempotencyConfig, IdempotencySource, SignatureAlgorithm, SignatureConfig,
    WebhookContext, WebhookInfo, WebhookResult, WebhookSignature,
};
pub use workflow::{
    ForgeWorkflow, SuspendReason, WorkflowContext, WorkflowEvent, WorkflowEventSender,
    WorkflowInfo, WorkflowStatus,
};

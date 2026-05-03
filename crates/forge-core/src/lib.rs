//! Core types, traits, and contexts for the Forge framework.
//!
//! This crate defines all handler traits ([`function::ForgeQuery`], [`function::ForgeMutation`],
//! [`job::ForgeJob`], [`cron::ForgeCron`], [`workflow::ForgeWorkflow`], [`daemon::ForgeDaemon`],
//! [`webhook::ForgeWebhook`], [`mcp::ForgeMcpTool`]), execution contexts, error types,
//! configuration, and testing infrastructure.
//!
//! Library code depends on `forge-core` for types. The runtime (`forge-runtime`) implements
//! the execution engine. Proc macros (`forge-macros`) generate trait implementations.

/// Internal sealing marker. Not part of the public API.
///
/// Handler traits (`ForgeQuery`, `ForgeMutation`, `ForgeJob`, `ForgeCron`,
/// `ForgeWorkflow`, `ForgeDaemon`, `ForgeWebhook`, `ForgeMcpTool`) require
/// implementors to also implement [`__sealed::Sealed`]. The `forge-macros`
/// proc-macros emit this impl automatically; user code should not implement
/// any handler trait by hand. Manual `impl Sealed` opts into framework
/// internals that may break in any release.
#[doc(hidden)]
pub mod __sealed {
    /// Marker trait that gates manual handler-trait implementations. Hidden
    /// from rustdoc because it is an implementation detail; do not implement
    /// it directly.
    pub trait Sealed {}
}

pub mod auth;
pub mod cluster;
pub mod config;
pub mod context;
pub mod cron;
pub mod daemon;
pub mod db;
pub mod env;
pub mod error;
pub mod function;
pub mod http;
pub mod job;
pub mod mcp;
pub mod metadata;
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

// Testing utilities
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
pub use db::ForgePool;
pub use env::{EnvAccess, EnvProvider, MockEnvProvider, RealEnvProvider};
pub use error::{ForgeError, Result};
pub use function::{
    AuthContext, AuthTokenTtl, DbConn, ForgeConn, ForgeDb, ForgeMutation, ForgeQuery, FunctionInfo,
    FunctionKind, JobDispatch, JobInfoLookup, LogLevel, MutationContext, OutboxBuffer, PendingJob,
    PendingWorkflow, QueryContext, RequestMetadata, TokenIssuer, WorkflowDispatch,
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
pub use metadata::{HandlerKind, HandlerMetadata};
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
pub use tenant::{HasTenant, TenantContext, TenantIsolationMode};
pub use types::{Instant, LocalDate, LocalTime, Upload};
pub use webhook::{
    ForgeWebhook, IdempotencyConfig, IdempotencySource, SignatureAlgorithm, SignatureConfig,
    WebhookContext, WebhookInfo, WebhookResult, WebhookSignature,
};
pub use workflow::{
    ForgeWorkflow, ParallelBuilder, ParallelResults, SuspendReason, WorkflowContext, WorkflowEvent,
    WorkflowEventSender, WorkflowInfo, WorkflowStatus,
};

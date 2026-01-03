pub mod auth;
pub mod cluster;
pub mod config;
pub mod cron;
pub mod error;
pub mod function;
pub mod job;
pub mod observability;
pub mod rate_limit;
pub mod realtime;
pub mod schema;
pub mod tenant;
pub mod workflow;

// Testing utilities - available when the "testing" feature is enabled or in test mode
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use auth::{Claims, ClaimsBuilder};
pub use cluster::{ClusterInfo, LeaderInfo, LeaderRole, NodeId, NodeInfo, NodeRole, NodeStatus};
pub use config::ForgeConfig;
pub use cron::{CronContext, CronInfo, CronSchedule, ForgeCron};
pub use error::{ForgeError, Result};
pub use function::{
    ActionContext, AuthContext, ForgeAction, ForgeMutation, ForgeQuery, FunctionInfo, FunctionKind,
    JobDispatch, MutationContext, QueryContext, RequestMetadata, WorkflowDispatch,
};
pub use job::{ForgeJob, JobContext, JobInfo, JobPriority, JobStatus, RetryConfig};
pub use observability::{
    Alert, AlertCondition, AlertSeverity, AlertState, AlertStatus, LogEntry, LogLevel, Metric,
    MetricKind, MetricLabels, MetricValue, Span, SpanContext, SpanKind, SpanStatus, TraceId,
};
pub use rate_limit::{RateLimitConfig, RateLimitHeaders, RateLimitKey, RateLimitResult};
pub use realtime::{
    Change, ChangeOperation, Delta, ReadSet, SessionId, SessionInfo, SessionStatus, SubscriptionId,
    SubscriptionInfo, SubscriptionState, TrackingMode,
};
pub use schema::{FieldDef, ModelMeta, SchemaRegistry, TableDef};
pub use tenant::{HasTenant, TenantContext, TenantIsolationMode};
pub use workflow::{
    ForgeWorkflow, ParallelBuilder, ParallelResults, SuspendReason, WorkflowContext, WorkflowEvent,
    WorkflowEventSender, WorkflowInfo, WorkflowStatus,
};

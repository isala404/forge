//! Forge runtime engine.
//!
//! Implements the Axum gateway, job worker, workflow executor, cron scheduler,
//! daemon runner, reactivity system, cluster coordination, rate limiting,
//! observability, and product analytics (signals).
//!
//! Subsystems are feature-gated. Always-on infrastructure: `db`, `cluster`,
//! `migrations`, `function` (dispatch core), `rate_limit`, `observability`
//! (with no-op stubs when `otel` is off), `testing`.
//!
//! Opt-in features:
//! - `gateway` bundles HTTP server + SSE realtime + MCP + OAuth + webhooks +
//!   signals (they share the axum/tower stack and split poorly)
//! - `jobs`, `workflows`, `cron`, `daemons` are independent of `gateway`
//! - `geoip` adds bundled IP-to-country resolution (heavy build-time download)
//! - `otel` adds OpenTelemetry exporters (heavy crate deps)

pub use sqlx;

// Always-on infrastructure
pub mod cluster;
pub mod function;
pub mod kv;
pub mod migrations;
pub mod observability;
pub mod pg;
pub mod rate_limit;
pub(crate) mod stable_hash;
pub mod testing;

// Optional subsystems
#[cfg(feature = "cron")]
pub mod cron;
#[cfg(feature = "daemons")]
pub mod daemon;
#[cfg(feature = "gateway")]
pub mod gateway;
#[cfg(feature = "jobs")]
pub mod jobs;
#[cfg(feature = "gateway")]
pub mod mcp;
#[cfg(feature = "gateway")]
pub mod realtime;
#[cfg(feature = "gateway")]
pub mod webhook;
#[cfg(feature = "workflows")]
pub mod workflow;

// Signals: real implementation lives behind `gateway` (the only place signals
// are actually ingested). When `gateway` is off, we expose a stub module so
// always-on modules (rate_limit, jobs, daemon, function) can call
// `crate::signals::emit_*` without cfg gates everywhere.
#[cfg(feature = "gateway")]
pub mod signals;
#[cfg(not(feature = "gateway"))]
#[path = "signals_stub.rs"]
pub mod signals;

// --- Re-exports follow the same gating ---

pub use cluster::{
    GracefulShutdown, HeartbeatConfig, HeartbeatLoop, InFlightGuard, NodeCounts, NodeRegistry,
    ShutdownConfig,
};
pub use function::{FunctionRegistry, FunctionRouter, RouteResult};
pub use kv::KvStore;
pub use observability::{
    TelemetryConfig, TelemetryError, build_env_filter, init_telemetry, shutdown_telemetry,
};
pub use pg::{
    AppliedMigration, Database, DriftStatus, Migration, MigrationRunner, MigrationStatus,
    load_migrations_from_dir,
};
pub use pg::{LeaderConfig, LeaderElection, LeaderGuard};
pub use rate_limit::{HybridRateLimiter, StrictRateLimiter};

#[cfg(feature = "cron")]
pub use cron::{CronEntry, CronRecord, CronRegistry, CronRunner, CronStatus};
#[cfg(feature = "daemons")]
pub use daemon::{DaemonEntry, DaemonRegistry, DaemonRunner, DaemonRunnerConfig};
#[cfg(feature = "gateway")]
pub use gateway::{
    AuthMiddleware, GatewayConfig, GatewayServer, RpcError, RpcHandler, RpcRequest, RpcResponse,
    TracingMiddleware,
};
#[cfg(feature = "jobs")]
pub use jobs::{
    JobDispatcher, JobExecutor, JobQueue, JobRecord, JobRegistry, Worker, WorkerConfig,
};
#[cfg(feature = "gateway")]
pub use mcp::{McpToolEntry, McpToolRegistry};
#[cfg(feature = "gateway")]
pub use realtime::{
    ChangeListener, InvalidationEngine, RealtimeConfig, RealtimeMessage, SessionManager,
    SessionServer, SubscriptionManager,
};
#[cfg(feature = "gateway")]
pub use webhook::{WebhookEntry, WebhookRegistry, WebhookState, webhook_handler};
#[cfg(feature = "workflows")]
pub use workflow::{
    DrainEntry, EventStore, WorkflowEntry, WorkflowExecutor, WorkflowReadiness, WorkflowRecord,
    WorkflowRegistry, WorkflowScheduler, WorkflowSchedulerConfig, WorkflowStepRecord,
};

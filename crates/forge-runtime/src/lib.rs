//! Forge runtime engine.

pub use sqlx;

// Always-on infrastructure
pub mod cluster;
pub mod function;
pub mod kv;
pub mod observability;
pub mod pg;
pub mod rate_limit;
pub(crate) mod stable_hash;

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

// No-op stubs when `gateway` is off so callers don't need cfg gates.
#[cfg(feature = "gateway")]
pub mod signals;
#[cfg(not(feature = "gateway"))]
pub mod signals {
    use forge_core::signals::SignalEvent;

    #[inline]
    pub fn emit_raw(_event: SignalEvent) {}

    #[inline]
    pub fn emit_server_execution(
        _name: &str,
        _kind: &str,
        _duration_ms: i32,
        _success: bool,
        _error_message: Option<String>,
    ) {
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn emit_diagnostic(
        _event_name: &str,
        _properties: serde_json::Value,
        _client_ip: Option<String>,
        _user_agent: Option<String>,
        _visitor_id: Option<String>,
        _user_id: Option<uuid::Uuid>,
        _is_bot: bool,
    ) {
    }

    #[inline]
    pub fn install_global(_: Option<()>) {}

    pub mod bot {
        #[inline]
        pub fn is_bot(_user_agent: Option<&str>) -> bool {
            false
        }
    }

    pub mod visitor {
        pub fn generate_visitor_id(
            _client_ip: Option<&str>,
            _user_agent: Option<&str>,
            _server_secret: &str,
        ) -> String {
            String::new()
        }
    }
}

pub use cluster::{GracefulShutdown, HeartbeatConfig, HeartbeatLoop, NodeRegistry, ShutdownConfig};
pub use function::{FunctionRegistry, RouteOutcome};
pub use kv::KvStore;
pub use observability::{TelemetryConfig, init_telemetry, shutdown_telemetry};
pub use pg::{
    AppliedMigration, Database, DriftStatus, Migration, MigrationRunner, MigrationStatus,
    load_migrations_from_dir,
};
pub use pg::{LeaderConfig, LeaderElection, PgNotifyBus};
pub use rate_limit::{HybridRateLimiter, StrictRateLimiter};

#[cfg(feature = "cron")]
pub use cron::{CronRegistry, CronRunner};
#[cfg(feature = "daemons")]
pub use daemon::{DaemonRegistry, DaemonRunner, DaemonRunnerConfig};
#[cfg(feature = "gateway")]
pub use gateway::{GatewayConfig, GatewayServer};
#[cfg(feature = "jobs")]
pub use jobs::{JobDispatcher, JobQueue, JobRegistry, Worker, WorkerConfig};
#[cfg(feature = "gateway")]
pub use mcp::McpToolRegistry;
#[cfg(feature = "gateway")]
pub use realtime::RealtimeConfig;
#[cfg(feature = "gateway")]
pub use webhook::{WebhookRegistry, WebhookState, webhook_handler};
#[cfg(feature = "workflows")]
pub use workflow::{
    EventStore, WorkflowExecutor, WorkflowRegistry, WorkflowScheduler, WorkflowSchedulerConfig,
};

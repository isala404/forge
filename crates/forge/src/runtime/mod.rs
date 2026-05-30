//! FORGE runtime — single binary that runs the gateway, workers, scheduler, and cluster.

mod builder;
pub use builder::ForgeBuilder;

#[cfg(feature = "gateway")]
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
#[cfg(feature = "gateway")]
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

#[cfg(feature = "gateway")]
use axum::Router;
#[cfg(feature = "gateway")]
use axum::body::Body;
#[cfg(feature = "gateway")]
use axum::http::Request;
#[cfg(feature = "gateway")]
use axum::response::Response;
use tokio::sync::broadcast;

use forge_core::cluster::{LeaderRole, NodeId, NodeInfo, NodeRole, NodeStatus};
use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_runtime::pg::migration::{Migration, MigrationRunner, load_migrations_from_dir};

#[cfg(feature = "gateway")]
use forge_core::mcp::ForgeMcpTool;
use forge_runtime::cluster::{
    GracefulShutdown, HeartbeatConfig, HeartbeatLoop, NodeRegistry, ShutdownConfig,
};
#[cfg(feature = "cron")]
use forge_runtime::cron::{CronRegistry, CronRunner, CronRunnerConfig};
#[cfg(feature = "daemons")]
use forge_runtime::daemon::{DaemonRegistry, DaemonRunner};
use forge_runtime::function::FunctionRegistry;
use forge_runtime::pg::Database;
use forge_runtime::pg::{LeaderConfig, LeaderElection, PgNotifyBus};
// CircuitBreakerClient wraps reqwest; used by cron/daemon/workflow for
// outbound HTTP. (Gateway uses its own reqwest path.)
#[cfg(any(feature = "cron", feature = "daemons", feature = "workflows"))]
use forge_core::CircuitBreakerClient;
#[cfg(feature = "gateway")]
use forge_runtime::gateway::{
    AuthConfig, GatewayConfig as RuntimeGatewayConfig, GatewayServer, PeerAddr, TlsListenConfig,
    bind_listener,
};
#[cfg(feature = "jobs")]
use forge_runtime::jobs::{JobDispatcher, JobQueue, JobRegistry, Worker, WorkerConfig};
#[cfg(feature = "gateway")]
use forge_runtime::mcp::McpToolRegistry;
#[cfg(feature = "gateway")]
use forge_runtime::realtime::{
    InvalidationConfig, ListenerConfig, ReactorConfig, RealtimeConfig as RuntimeRealtimeConfig,
};
#[cfg(feature = "gateway")]
use forge_runtime::webhook::{WebhookRegistry, WebhookState, webhook_handler};
#[cfg(feature = "workflows")]
use forge_runtime::workflow::{
    EventStore, WorkflowExecutor, WorkflowRegistry, WorkflowScheduler, WorkflowSchedulerConfig,
};
#[cfg(feature = "workflows")]
use tokio_util::sync::CancellationToken;

use builder::{config_role_to_node_role, get_hostname};

/// Type alias for frontend handler function.
#[cfg(feature = "gateway")]
pub type FrontendHandler = fn(Request<Body>) -> Pin<Box<dyn Future<Output = Response> + Send>>;

/// Common imports for Forge applications.
///
/// **Stability contract:** all `pub use` items here are part of Forge's stable
/// surface. Removing or renaming an item is a breaking change.
///
/// **Upstream crates:** `axum` is re-exported so `custom_routes` factories
/// compile against the same version the runtime uses. A breaking axum change
/// requires a Forge minor or major bump.
pub mod prelude {
    pub use chrono::{DateTime, Utc};
    pub use uuid::Uuid;

    pub use serde::{Deserialize, Serialize};
    pub use serde_json;
    pub use serde_json::Value;

    pub type Timestamp = DateTime<Utc>;

    pub use forge_core::auth::TokenPair;
    pub use forge_core::config::ForgeConfig;
    pub use forge_core::cron::{CronContext, ForgeCron};
    pub use forge_core::daemon::{DaemonContext, ForgeDaemon};
    // EnvAccess adds `ctx.env(...)` / `ctx.env_require(...)` — kept in the
    // glob so handlers don't need an explicit import.
    pub use forge_core::env::EnvAccess;
    pub use forge_core::error::{ForgeError, Result};
    pub use forge_core::function::{
        AuthContext, DbConn, ForgeMutation, ForgeQuery, MutationContext, QueryContext,
    };
    pub use forge_core::job::{ForgeJob, JobContext, JobPriority};
    pub use forge_core::mcp::{ForgeMcpTool, McpToolContext};
    pub use forge_core::realtime::Delta;
    pub use forge_core::schemars::JsonSchema;
    pub use forge_core::types::Upload;
    pub use forge_core::webhook::{ForgeWebhook, WebhookContext, WebhookResult, WebhookSignature};
    pub use forge_core::workflow::{ForgeWorkflow, WorkflowContext};

    // Same axum version the runtime uses; avoids type mismatches in custom_routes factories.
    #[cfg(feature = "gateway")]
    pub use axum;

    pub use crate::{Forge, ForgeBuilder};

    pub use forge_core::testing::{
        TestCronContext, TestDaemonContext, TestJobContext, TestMcpToolContext,
        TestMutationContext, TestQueryContext, TestWebhookContext, TestWorkflowContext,
    };
}

/// The main FORGE runtime.
pub struct Forge {
    pub(super) config: ForgeConfig,
    pub(super) db: Option<Database>,
    pub(super) node_id: NodeId,
    pub(super) function_registry: FunctionRegistry,
    #[cfg(feature = "gateway")]
    pub(super) mcp_registry: McpToolRegistry,
    #[cfg(feature = "jobs")]
    pub(super) job_registry: JobRegistry,
    #[cfg(feature = "cron")]
    pub(super) cron_registry: Arc<CronRegistry>,
    #[cfg(feature = "workflows")]
    pub(super) workflow_registry: WorkflowRegistry,
    #[cfg(feature = "daemons")]
    pub(super) daemon_registry: Arc<DaemonRegistry>,
    #[cfg(feature = "gateway")]
    pub(super) webhook_registry: Arc<WebhookRegistry>,
    pub(super) shutdown_tx: broadcast::Sender<()>,
    pub(super) migrations_dir: PathBuf,
    pub(super) extra_migrations: Vec<Migration>,
    #[cfg(feature = "gateway")]
    pub(super) frontend_handler: Option<FrontendHandler>,
    #[cfg(feature = "gateway")]
    pub(super) custom_routes_factory: Option<Box<dyn FnOnce(sqlx::PgPool) -> Router + Send + Sync>>,
    #[cfg(feature = "gateway")]
    pub(super) role_resolver: Option<forge_core::SharedRoleResolver>,
}

impl Forge {
    pub fn builder() -> ForgeBuilder {
        ForgeBuilder::new()
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn config(&self) -> &ForgeConfig {
        &self.config
    }

    pub fn function_registry(&self) -> &FunctionRegistry {
        &self.function_registry
    }

    pub fn function_registry_mut(&mut self) -> &mut FunctionRegistry {
        &mut self.function_registry
    }

    #[cfg(feature = "gateway")]
    pub fn mcp_registry_mut(&mut self) -> &mut McpToolRegistry {
        &mut self.mcp_registry
    }

    #[cfg(feature = "gateway")]
    pub fn register_mcp_tool<T: ForgeMcpTool>(&mut self) -> &mut Self {
        self.mcp_registry.register::<T>();
        self
    }

    #[cfg(feature = "jobs")]
    pub fn job_registry(&self) -> &JobRegistry {
        &self.job_registry
    }

    #[cfg(feature = "jobs")]
    pub fn job_registry_mut(&mut self) -> &mut JobRegistry {
        &mut self.job_registry
    }

    #[cfg(feature = "cron")]
    pub fn cron_registry(&self) -> Arc<CronRegistry> {
        self.cron_registry.clone()
    }

    #[cfg(feature = "workflows")]
    pub fn workflow_registry(&self) -> &WorkflowRegistry {
        &self.workflow_registry
    }

    #[cfg(feature = "workflows")]
    pub fn workflow_registry_mut(&mut self) -> &mut WorkflowRegistry {
        &mut self.workflow_registry
    }

    #[cfg(feature = "daemons")]
    pub fn daemon_registry(&self) -> Arc<DaemonRegistry> {
        self.daemon_registry.clone()
    }

    #[cfg(feature = "gateway")]
    pub fn webhook_registry(&self) -> Arc<WebhookRegistry> {
        self.webhook_registry.clone()
    }

    /// Persist all workflow definitions. Fails startup if a signature conflicts
    /// under the same name+version (contract changed without a version bump).
    #[cfg(feature = "workflows")]
    async fn persist_workflow_definitions(&self, pool: &sqlx::PgPool) -> Result<()> {
        self.workflow_registry.persist_definitions(pool).await
    }

    /// Install the telemetry/tracing subscriber from the configured observability
    /// settings. Best-effort: a failure is reported but does not abort startup
    /// (it falls back to `eprintln!` since tracing macros would be dropped).
    fn init_telemetry_subsystem(&self) {
        let telemetry_config = forge_runtime::TelemetryConfig::from_observability_config(
            &self.config.observability,
            &self.config.project.name,
            &self.config.project.version,
        );
        let telemetry_result = forge_runtime::init_telemetry(
            &telemetry_config,
            &self.config.project.name,
            &self.config.observability.log_level,
        );
        match &telemetry_result {
            Ok(true) | Ok(false) => {
                tracing::debug!(
                    endpoint = %telemetry_config.otlp_endpoint,
                    traces = telemetry_config.enable_traces,
                    metrics = telemetry_config.enable_metrics,
                    logs = telemetry_config.enable_logs,
                    sampling = telemetry_config.sampling_ratio,
                    "Telemetry initialized"
                );
            }
            Err(e) => eprintln!("forge: failed to initialize telemetry: {e}"),
        }
    }

    /// Run user migrations, then persist workflow definitions if any are registered.
    async fn apply_migrations(&self, pool: &sqlx::PgPool) -> Result<()> {
        let runner = MigrationRunner::new(pool.clone());

        let mut user_migrations = load_migrations_from_dir(&self.migrations_dir)?;
        user_migrations.extend(self.extra_migrations.clone());

        runner.run(user_migrations).await?;
        tracing::debug!("Migrations applied");

        #[cfg(feature = "workflows")]
        if !self.workflow_registry.is_empty() {
            self.persist_workflow_definitions(pool).await?;
        }

        Ok(())
    }

    /// Start the runtime. Blocks until a ctrl-c or `Forge::shutdown()` is called.
    pub async fn run(mut self) -> Result<()> {
        self.init_telemetry_subsystem();

        tracing::debug!("Connecting to database");

        let db =
            Database::from_config_with_service(&self.config.database, &self.config.project.name)
                .await?;
        let pool = db.primary().clone();
        // Health monitor self-terminates on shutdown_tx; drop the handle.
        let _ = db.start_health_monitor(self.shutdown_tx.subscribe());
        self.db = Some(db);

        tracing::debug!("Database connected");

        self.apply_migrations(&pool).await?;

        let hostname = get_hostname();

        // HOST env var overrides bind address; PORT env var overrides config port.
        let ip_address: IpAddr = std::env::var("HOST")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));

        // PORT env var overrides the configured port. Resolve it into a local
        // rather than mutating the config in place — `run()` should not leave
        // the owned config in a different state than it was constructed with.
        let effective_port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(self.config.gateway.port);

        let roles: Vec<NodeRole> = self
            .config
            .node
            .roles
            .iter()
            .map(config_role_to_node_role)
            .collect();

        let node_info = NodeInfo::new_local(
            hostname,
            ip_address,
            effective_port,
            self.config.gateway.grpc_port,
            roles.clone(),
            self.config.node.worker_capabilities.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
        );

        let node_id = node_info.id;
        self.node_id = node_id;

        let node_registry = Arc::new(NodeRegistry::new(pool.clone(), node_info));

        if let Err(e) = node_registry.register().await {
            tracing::debug!("Failed to register node (tables may not exist): {}", e);
        }

        if let Err(e) = node_registry.set_status(NodeStatus::Active).await {
            tracing::debug!("Failed to set node status: {}", e);
        }

        // Construct the shared PG NOTIFY bus up front so the leader election
        // can subscribe to `forge_leader_released` and react instantly to a
        // sibling's voluntary release instead of waiting for the next tick.
        let notify_bus = Arc::new(PgNotifyBus::new(
            pool.clone(),
            &[
                "forge_changes",
                "forge_jobs_available",
                "forge_workflow_wakeup",
                forge_runtime::pg::LEADER_RELEASED_CHANNEL,
            ],
        ));

        let leader_election = if roles.contains(&NodeRole::Scheduler) {
            let election = Arc::new(
                LeaderElection::new(
                    pool.clone(),
                    node_id,
                    LeaderRole::Scheduler,
                    LeaderConfig::default(),
                )
                .with_notify_bus(notify_bus.clone()),
            );

            // Try to become leader
            if let Err(e) = election.try_become_leader().await {
                tracing::debug!("Failed to acquire leadership: {}", e);
            }

            Some(election)
        } else {
            None
        };

        let shutdown = Arc::new(GracefulShutdown::new(
            node_registry.clone(),
            leader_election.clone(),
            ShutdownConfig::default(),
        ));

        #[cfg(any(feature = "cron", feature = "daemons", feature = "workflows"))]
        let http_client = CircuitBreakerClient::with_ssrf_protection();

        let mut handles = Vec::new();
        // Leader handles: cron, daemon, workflow scheduler. Must finish before
        // releasing the advisory lock so no sibling starts duplicate work mid-tick.
        let mut leader_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        {
            let heartbeat_pool = pool.clone();
            let heartbeat_node_id = node_id;
            let config = HeartbeatConfig::from_cluster_config(&self.config.cluster);
            handles.push(tokio::spawn(async move {
                match HeartbeatLoop::new(heartbeat_pool, heartbeat_node_id, config).await {
                    Ok(heartbeat) => heartbeat.run().await,
                    Err(e) => tracing::error!(error = %e, "Failed to start heartbeat loop"),
                }
            }));
        }

        if let Some(ref election) = leader_election {
            let election = election.clone();
            handles.push(tokio::spawn(async move {
                election.run().await;
            }));
        }

        #[cfg(feature = "cron")]
        {
            forge_runtime::cron::register_cron_bridges(&self.cron_registry, &mut self.job_registry);
        }

        #[cfg(feature = "jobs")]
        let job_queue = JobQueue::new(pool.clone());

        // Gate the direct bus spawn on the absence of a gateway role: the reactor
        // already calls bus.run() for gateway nodes, and two spawns on the same
        // PgNotifyBus instance would race.
        #[cfg(feature = "gateway")]
        let notify_bus_needs_direct_spawn = !roles.contains(&NodeRole::Gateway);
        #[cfg(not(feature = "gateway"))]
        let notify_bus_needs_direct_spawn = true;
        if notify_bus_needs_direct_spawn {
            let (bus_shutdown_tx, bus_shutdown_rx) = tokio::sync::watch::channel(false);
            let bus_for_task = notify_bus.clone();
            handles.push(tokio::spawn(async move {
                bus_for_task.run(bus_shutdown_rx).await;
            }));
            let mut bus_broadcast_rx = self.shutdown_tx.subscribe();
            tokio::spawn(async move {
                let _ = bus_broadcast_rx.recv().await;
                let _ = bus_shutdown_tx.send(true);
            });
        }

        let kv_handle: Arc<dyn forge_core::function::KvHandle> =
            Arc::new(forge_runtime::KvStore::new(pool.clone(), "handlers"));

        // Must register the workflow bridge BEFORE spawning workers: JobRegistry
        // is cloned by value per worker, so late registration is invisible and
        // `$workflow_resume` jobs would fail with "unknown job type".
        #[cfg(feature = "workflows")]
        let workflow_bridge_executor = Arc::new(
            WorkflowExecutor::new(
                Arc::new(self.workflow_registry.clone()),
                pool.clone(),
                job_queue.clone(),
                http_client.clone(),
            )
            .with_kv(Arc::clone(&kv_handle)),
        );
        #[cfg(feature = "workflows")]
        {
            forge_runtime::workflow::register_workflow_bridge(
                workflow_bridge_executor.clone(),
                &mut self.job_registry,
            );
        }

        // Dispatcher must be constructed before workers so it can be threaded
        // into each JobExecutor. Without it, `ctx.start_workflow(...)` writes
        // blank version/signature columns and immediately blocks on resume.
        #[cfg(feature = "jobs")]
        let job_dispatcher = {
            let job_queue_for_dispatch = JobQueue::new(pool.clone());
            Arc::new(JobDispatcher::new(
                job_queue_for_dispatch,
                self.job_registry.clone(),
            ))
        };

        // One Worker per queue so `default` traffic can't starve `workflows` or `cron`.
        // Only the `default` queue's worker claims untagged (NULL capability) jobs.
        #[cfg(feature = "jobs")]
        if roles.contains(&NodeRole::Worker) {
            let mut node_capabilities: Vec<String> = self.config.node.worker_capabilities.clone();
            for queue_name in self.config.worker.queues.keys() {
                if !node_capabilities.iter().any(|c| c == queue_name) {
                    node_capabilities.push(queue_name.clone());
                }
            }

            for (queue_name, queue_cfg) in &self.config.worker.queues {
                if queue_cfg.workers == 0 {
                    continue;
                }
                let worker_id = Uuid::new_v4();
                let claim_untagged = queue_name == forge_core::config::DEFAULT_QUEUE;
                let worker_config = WorkerConfig {
                    id: Some(worker_id),
                    capabilities: vec![queue_name.clone()],
                    claim_untagged,
                    max_concurrent: queue_cfg.workers,
                    poll_interval: *self.config.worker.poll_interval,
                    ..Default::default()
                };

                let worker_base = Worker::new(
                    worker_config,
                    job_queue.clone(),
                    self.job_registry.clone(),
                    pool.clone(),
                    notify_bus.clone(),
                )
                .with_kv(Arc::clone(&kv_handle))
                .with_job_dispatch(job_dispatcher.clone());

                #[cfg(feature = "workflows")]
                let mut worker =
                    worker_base.with_workflow_dispatch(workflow_bridge_executor.clone());
                #[cfg(not(feature = "workflows"))]
                let mut worker = worker_base;

                let queue_label = queue_name.clone();
                handles.push(tokio::spawn(async move {
                    if let Err(e) = worker.run().await {
                        tracing::error!(queue = %queue_label, "Worker error: {}", e);
                    }
                }));

                tracing::debug!(
                    queue = %queue_name,
                    workers = queue_cfg.workers,
                    "Job worker pool started",
                );
            }

            // pool_size >= sum(workers) + 6 (persistent conns: change listener,
            // leader lock, heartbeat, health monitor, migration lock, headroom).
            // Gateway handlers draw from the same pool but hold connections briefly.
            let total_worker_concurrency: usize =
                self.config.worker.queues.values().map(|q| q.workers).sum();
            const PERSISTENT_CONN_OVERHEAD: usize = 6;
            let min_recommended = total_worker_concurrency + PERSISTENT_CONN_OVERHEAD;
            if (self.config.database.pool_size as usize) < min_recommended {
                tracing::warn!(
                    pool_size = self.config.database.pool_size,
                    total_worker_concurrency,
                    min_recommended,
                    "database.pool_size ({}) is below the recommended minimum ({}) for the \
                     configured worker concurrency. \
                     Formula: sum(workers per queue) + 6 = {} + 6 = {}. \
                     Increase database.pool_size to avoid connection exhaustion under load.",
                    self.config.database.pool_size,
                    min_recommended,
                    total_worker_concurrency,
                    min_recommended,
                );
            }
        }

        #[cfg(feature = "jobs")]
        if roles.contains(&NodeRole::Worker) {
            let kv_pool = pool.clone();
            let mut kv_shutdown = self.shutdown_tx.subscribe();
            let kv_leader = leader_election.clone();
            handles.push(tokio::spawn(async move {
                let kv = forge_runtime::KvStore::new(kv_pool.clone(), "app");
                let rate_limiter = forge_runtime::StrictRateLimiter::new(kv_pool);
                loop {
                    tokio::select! {
                        _ = kv_shutdown.recv() => break,
                        _ = tokio::time::sleep(Duration::from_secs(300)) => {}
                    }
                    let is_leader = kv_leader.as_ref().map(|e| e.is_leader()).unwrap_or(true);
                    if !is_leader {
                        continue;
                    }
                    match kv.cleanup_expired().await {
                        Ok(n) if n > 0 => tracing::debug!(count = n, "KV TTL cleanup"),
                        Err(e) => tracing::warn!(error = %e, "KV TTL cleanup failed"),
                        _ => {}
                    }
                    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
                    match rate_limiter.cleanup(cutoff).await {
                        Ok(n) if n > 0 => tracing::debug!(count = n, "Rate limit bucket cleanup"),
                        Err(e) => tracing::warn!(error = %e, "Rate limit cleanup failed"),
                        _ => {}
                    }
                }
            }));
        }

        #[cfg(feature = "cron")]
        let cron_runner_handle: Option<Arc<CronRunner>> = if roles.contains(&NodeRole::Scheduler) {
            let cron_registry = self.cron_registry.clone();
            let cron_pool = pool.clone();
            let cron_leader_election = leader_election.clone();

            let cron_config = CronRunnerConfig {
                poll_interval: *self.config.cron.poll_interval,
                node_id: node_id.as_uuid(),
                is_leader: cron_leader_election.is_none(),
                leader_election: cron_leader_election,
                run_stale_threshold: Duration::from_secs(15 * 60),
                ..Default::default()
            };

            let cron_runner = Arc::new(CronRunner::new(
                cron_registry,
                cron_pool,
                job_queue.clone(),
                cron_config,
            ));
            let cron_runner_clone = cron_runner.clone();

            leader_handles.push(tokio::spawn(async move {
                if let Err(e) = cron_runner_clone.run().await {
                    tracing::error!("Cron runner error: {}", e);
                }
            }));

            tracing::debug!("Cron scheduler started");
            Some(cron_runner)
        } else {
            None
        };

        #[cfg(feature = "workflows")]
        let workflow_shutdown_token = CancellationToken::new();
        #[cfg(feature = "workflows")]
        if roles.contains(&NodeRole::Scheduler) {
            let event_store = Arc::new(EventStore::new(pool.clone()));
            let scheduler = WorkflowScheduler::new(
                pool.clone(),
                job_queue.clone(),
                event_store,
                WorkflowSchedulerConfig {
                    poll_interval: *self.config.workflow.poll_interval,
                    leader_election: leader_election.clone(),
                    ..WorkflowSchedulerConfig::default()
                },
                notify_bus.clone(),
            );

            let shutdown_token = workflow_shutdown_token.clone();
            leader_handles.push(tokio::spawn(async move {
                scheduler.run(shutdown_token).await;
            }));

            tracing::debug!("Workflow scheduler started");
        }

        #[cfg(feature = "workflows")]
        let workflow_executor = workflow_bridge_executor;

        #[cfg(feature = "daemons")]
        if roles.contains(&NodeRole::Scheduler) && !self.daemon_registry.is_empty() {
            let daemon_registry = self.daemon_registry.clone();
            let daemon_pool = pool.clone();
            let daemon_http = http_client.clone();
            let daemon_shutdown_rx = self.shutdown_tx.subscribe();

            let daemon_runner = DaemonRunner::new(
                daemon_registry,
                daemon_pool,
                daemon_http,
                node_id.as_uuid(),
                daemon_shutdown_rx,
            )
            .with_config(forge_runtime::daemon::DaemonRunnerConfig {
                health_check_interval: *self.config.daemon.health_check_interval,
                heartbeat_interval: *self.config.daemon.heartbeat_interval,
            });
            #[cfg(feature = "jobs")]
            let daemon_runner = daemon_runner.with_job_dispatch(job_dispatcher.clone());
            #[cfg(feature = "workflows")]
            let daemon_runner = daemon_runner.with_workflow_dispatch(workflow_executor.clone());
            let daemon_runner = daemon_runner.with_kv(Arc::clone(&kv_handle));

            leader_handles.push(tokio::spawn(async move {
                if let Err(e) = daemon_runner.run().await {
                    tracing::error!("Daemon runner error: {}", e);
                }
            }));

            tracing::debug!("Daemon runner started");
        }

        #[cfg(feature = "gateway")]
        let mut reactor_handle = None;

        #[cfg(feature = "gateway")]
        if roles.contains(&NodeRole::Gateway) {
            // `from_core` enforces the both-or-neither contract here too, so
            // a programmatically constructed `ForgeConfig` that bypasses
            // `validate()` still can't slip a half-set TLS config through.
            let tls: Option<TlsListenConfig> =
                TlsListenConfig::from_core(&self.config.gateway.tls)?;

            // Fail early if handlers require auth but no usable credentials are configured.
            // The registry is populated at this point so we can inspect every handler.
            let any_requires_auth = self
                .function_registry
                .queries()
                .any(|(_, info)| !info.is_public || info.required_role.is_some())
                || self
                    .function_registry
                    .mutations()
                    .any(|(_, info)| !info.is_public || info.required_role.is_some());

            if any_requires_auth && !self.config.auth.is_configured() {
                return Err(ForgeError::config(
                    "One or more handlers require authentication (private scope or require_role) \
                     but auth is not configured. Set auth.jwt_secret (≥32 bytes) for HMAC or \
                     auth.jwks_url for external identity providers.",
                ));
            }

            // CORS wildcard is only allowed in development. Block startup
            // unless FORGE_ENV is explicitly set to "development".
            if self.config.gateway.cors_enabled
                && self.config.gateway.cors_origins.iter().any(|o| o == "*")
            {
                let forge_env = std::env::var("FORGE_ENV").ok();
                let is_dev = forge_env
                    .as_deref()
                    .is_some_and(|v| v.eq_ignore_ascii_case("development"));
                if !is_dev {
                    let production_indicators = [
                        ("FORGE_ENV", std::env::var("FORGE_ENV").ok()),
                        ("NODE_ENV", std::env::var("NODE_ENV").ok()),
                        (
                            "RAILWAY_ENVIRONMENT",
                            std::env::var("RAILWAY_ENVIRONMENT").ok(),
                        ),
                        ("K_SERVICE", std::env::var("K_SERVICE").ok()),
                        ("FLY_APP_NAME", std::env::var("FLY_APP_NAME").ok()),
                        (
                            "KUBERNETES_SERVICE_HOST",
                            std::env::var("KUBERNETES_SERVICE_HOST").ok(),
                        ),
                        ("AWS_EXECUTION_ENV", std::env::var("AWS_EXECUTION_ENV").ok()),
                    ];
                    let hint = production_indicators
                        .iter()
                        .find_map(|(name, val)| {
                            val.as_ref().map(|v| format!(" ({name}={v} detected)"))
                        })
                        .unwrap_or_default();
                    return Err(ForgeError::config(format!(
                        "gateway.cors_origins = [\"*\"] is only allowed when FORGE_ENV=development{hint}. \
                         Set explicit origins (e.g. cors_origins = [\"https://yourdomain.com\"])."
                    )));
                }
            }

            let gateway_config = RuntimeGatewayConfig {
                port: effective_port,
                max_connections: self.config.gateway.max_connections,
                sse_max_sessions: self.config.realtime.sse_max_sessions,
                request_timeout_secs: self.config.gateway.request_timeout.as_secs(),
                cors_enabled: self.config.gateway.cors_enabled,
                cors_origins: self.config.gateway.cors_origins.clone(),
                auth: AuthConfig::from_forge_config(&self.config.auth)
                    .map_err(|e| ForgeError::config(e.to_string()))?,
                mcp: self.config.mcp.clone(),
                quiet_paths: self.config.gateway.quiet_paths.clone(),
                max_body_size_bytes: self.config.gateway.max_body_size.as_bytes(),
                max_json_body_bytes: self.config.gateway.max_json_body_size.as_bytes(),
                max_file_size_bytes: self.config.gateway.max_file_size.as_bytes(),
                token_ttl: forge_core::AuthTokenTtl::new(
                    self.config.auth.access_token_ttl_secs(),
                    self.config.auth.refresh_token_ttl_days(),
                ),
                project_name: self.config.project.name.clone(),
                tls,
                reactor_config: {
                    let rt = &self.config.realtime;
                    ReactorConfig {
                        listener: ListenerConfig {
                            buffer_size: rt.postgres_change_buffer_size,
                            ..ListenerConfig::default()
                        },
                        invalidation: InvalidationConfig {
                            debounce_ms: rt.debounce_quiet_window.as_millis(),
                            max_debounce_ms: rt.debounce_max_wait.as_millis(),
                            ..InvalidationConfig::default()
                        },
                        realtime: RuntimeRealtimeConfig {
                            max_subscriptions_per_session: rt.subscription_max_per_session,
                        },
                        max_concurrent_reexecutions: rt.max_concurrent_reexecutions,
                        resync_interval_secs: rt.resync_interval.as_secs(),
                        shard_count: rt.shard_count,
                        ..ReactorConfig::default()
                    }
                },
                max_multipart_fields: self.config.gateway.max_multipart_fields,
                max_sessions_per_user: self.config.realtime.max_sessions_per_user,
                max_sessions_per_ip: self.config.realtime.max_sessions_per_ip,
                max_subscriptions_per_user: self.config.realtime.max_subscriptions_per_user,
                security_headers: self.config.gateway.security_headers,
                hsts: self.config.gateway.hsts,
                trusted_proxies: self
                    .config
                    .gateway
                    .trusted_proxies
                    .iter()
                    .filter_map(|s| {
                        s.parse::<ipnet::IpNet>()
                            .or_else(|_| s.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
                            .ok()
                    })
                    .collect(),
                max_jobs_per_request: self.config.gateway.max_jobs_per_request,
                max_result_size_bytes: self.config.gateway.max_result_size_bytes,
                max_json_depth: self.config.gateway.max_json_depth,
            };

            let db_ref = self
                .db
                .clone()
                .ok_or_else(|| ForgeError::internal("Database not initialized"))?;

            let gateway = GatewayServer::new(
                gateway_config,
                self.function_registry.clone(),
                db_ref.clone(),
                notify_bus.clone(),
            )
            .with_node_id(self.node_id);
            #[cfg(feature = "jobs")]
            let gateway = gateway.with_job_dispatcher(job_dispatcher.clone());
            #[cfg(feature = "workflows")]
            let gateway = gateway.with_workflow_dispatcher(workflow_executor.clone());
            let gateway = gateway.with_kv(Arc::clone(&kv_handle));
            let mut gateway = gateway.with_mcp_registry(self.mcp_registry.clone());

            if matches!(
                self.config.rate_limit.mode,
                forge_core::config::RateLimitMode::Hybrid
            ) {
                // System table query runs at startup, not in offline-checked code path.
                #[allow(clippy::disallowed_methods)]
                let active_nodes: Option<i64> =
                    sqlx::query_scalar("SELECT COUNT(*) FROM forge_nodes WHERE status = 'active'")
                        .fetch_one(db_ref.primary())
                        .await
                        .ok();
                if active_nodes.is_some_and(|n| n > 1) {
                    let n = active_nodes.unwrap_or(0);
                    tracing::warn!(
                        active_nodes = n,
                        "rate_limit.mode is 'hybrid' but {n} active nodes detected. \
                         Per-user/per-IP limits are local-only and effectively multiply by the \
                         node count. Set rate_limit.mode = \"strict\" for cluster deployments."
                    );
                }
            }

            let rate_limiter: std::sync::Arc<dyn forge_core::rate_limit::RateLimiterBackend> =
                match self.config.rate_limit.mode {
                    forge_core::config::RateLimitMode::Strict => std::sync::Arc::new(
                        forge_runtime::StrictRateLimiter::new(db_ref.primary().clone()),
                    ),
                    forge_core::config::RateLimitMode::Hybrid => {
                        std::sync::Arc::new(forge_runtime::HybridRateLimiter::with_max_buckets(
                            db_ref.primary().clone(),
                            self.config.rate_limit.max_local_buckets,
                        ))
                    }
                };
            gateway = gateway.with_rate_limiter(rate_limiter);
            if let Some(resolver) = self.role_resolver.take() {
                gateway = gateway.with_role_resolver(resolver);
            }
            if self.config.signals.enabled {
                let signals_pool = std::sync::Arc::new(db_ref.primary().clone());
                let collector = forge_runtime::signals::SignalsCollector::spawn(
                    signals_pool.clone(),
                    self.config.signals.batch_size,
                    *self.config.signals.flush_interval,
                    self.config.signals.channel_capacity,
                );
                // Explicit MMDB path means the operator wants city-level data.
                // Fail fast rather than silently downgrading to the embedded DB.
                let geoip = match &self.config.signals.geoip_db_path {
                    Some(path) => {
                        let resolver = forge_runtime::signals::geoip::GeoIpResolver::from_mmdb(
                            std::path::Path::new(path),
                        )?;
                        tracing::info!(path, "GeoIP: MaxMind MMDB loaded (city-level)");
                        resolver
                    }
                    None => forge_runtime::signals::geoip::GeoIpResolver::new(),
                };
                gateway = gateway
                    .with_signals_collector(collector)
                    .with_signals_anonymize_ip(self.config.signals.anonymize_ip)
                    .with_signals_geoip(geoip)
                    .with_signals_rate_limit_per_minute(self.config.signals.rate_limit_per_minute);

                forge_runtime::signals::session::spawn_session_reaper(
                    signals_pool.clone(),
                    (self.config.signals.session_timeout.as_secs() / 60) as u32,
                );

                forge_runtime::signals::partition::ensure_partitions(&signals_pool).await;

                {
                    let partition_pool = signals_pool.clone();
                    let retention_days = self.config.signals.retention_days;
                    let partition_leader = leader_election.clone();
                    let mut partition_shutdown = self.shutdown_tx.subscribe();
                    handles.push(tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                _ = partition_shutdown.recv() => break,
                                _ = tokio::time::sleep(Duration::from_secs(21_600)) => {}
                            }
                            let is_leader = partition_leader
                                .as_ref()
                                .map(|e| e.is_leader())
                                .unwrap_or(true);
                            if is_leader {
                                forge_runtime::signals::partition::ensure_partitions(
                                    &partition_pool,
                                )
                                .await;
                                forge_runtime::signals::partition::drop_old_partitions(
                                    &partition_pool,
                                    retention_days,
                                )
                                .await;
                                forge_runtime::signals::partition::check_default_partition(
                                    &partition_pool,
                                )
                                .await;
                            }
                        }
                    }));
                }

                tracing::info!("Signals enabled (analytics + diagnostics)");
            }

            // Webhooks ride the gateway middleware stack via `with_custom_routes`.
            // The previous separate router had a hand-rolled CORS stack that
            // deadlocked post-preflight browser POSTs and omitted the
            // `x-webhook-timestamp` header.
            let mut custom_routes: Option<Router> = self
                .custom_routes_factory
                .take()
                .map(|factory| factory(pool.clone()));

            if !self.webhook_registry.is_empty() {
                use axum::extract::DefaultBodyLimit;
                use axum::routing::post;

                let webhook_state = WebhookState::new(self.webhook_registry.clone(), pool.clone());
                #[cfg(feature = "jobs")]
                let webhook_state = webhook_state.with_job_dispatcher(job_dispatcher.clone());
                #[cfg(feature = "workflows")]
                let webhook_state =
                    webhook_state.with_workflow_dispatcher(workflow_executor.clone());
                let webhook_state = webhook_state.with_kv(Arc::clone(&kv_handle));
                let webhook_state = Arc::new(webhook_state);

                let webhook_routes = Router::new()
                    .route(
                        "/webhooks/{*path}",
                        post(webhook_handler).with_state(webhook_state),
                    )
                    .layer(DefaultBodyLimit::max(1024 * 1024));

                custom_routes = Some(match custom_routes {
                    Some(existing) => existing.merge(webhook_routes),
                    None => webhook_routes,
                });

                tracing::debug!(
                    webhooks = ?self.webhook_registry.paths().collect::<Vec<_>>(),
                    "Webhook routes registered"
                );
            }

            if let Some(routes) = custom_routes {
                gateway = gateway.with_custom_routes(routes);
                tracing::debug!("Custom and webhook routes merged into gateway middleware stack");
            }

            let reactor = gateway.reactor();
            if let Err(e) = reactor.start().await {
                tracing::error!("Failed to start reactor: {}", e);
            } else {
                tracing::debug!("Reactor started");
                reactor_handle = Some(reactor);
            }

            let api_router = gateway.router();
            let mut router = Router::new().nest("/_api", api_router);

            if self.config.mcp.enabled {
                use axum::routing::get;

                // Return a parseable JSON 404 when OAuth is not configured so
                // MCP clients get a clear signal rather than an HTML error page.
                async fn oauth_not_supported() -> impl axum::response::IntoResponse {
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        axum::Json(serde_json::json!({
                            "error": "oauth_not_supported",
                            "error_description": "This server does not support OAuth. Connect without authentication."
                        })),
                    )
                }

                #[cfg(feature = "mcp-oauth")]
                if let Some((oauth_api_router, oauth_state)) = gateway.oauth_router() {
                    router = router.nest("/_api", oauth_api_router);
                    router = router
                        .route(
                            "/.well-known/oauth-authorization-server",
                            get(forge_runtime::gateway::oauth::well_known_oauth_metadata)
                                .with_state(oauth_state.clone()),
                        )
                        .route(
                            "/.well-known/oauth-protected-resource",
                            get(forge_runtime::gateway::oauth::well_known_resource_metadata)
                                .with_state(oauth_state),
                        );

                    tracing::info!("OAuth 2.1 endpoints enabled for MCP");
                } else {
                    router = router
                        .route(
                            "/.well-known/oauth-authorization-server",
                            get(oauth_not_supported),
                        )
                        .route(
                            "/.well-known/oauth-protected-resource",
                            get(oauth_not_supported),
                        );
                }

                #[cfg(not(feature = "mcp-oauth"))]
                {
                    router = router
                        .route(
                            "/.well-known/oauth-authorization-server",
                            get(oauth_not_supported),
                        )
                        .route(
                            "/.well-known/oauth-protected-resource",
                            get(oauth_not_supported),
                        );
                }
            }

            if let Some(handler) = self.frontend_handler {
                use axum::routing::get;
                router = router.fallback(get(handler));
                tracing::debug!("Frontend handler enabled");
            }

            let addr = gateway.addr();
            let tls = gateway.tls().cloned();
            // Graceful shutdown drains in-flight requests before we release the
            // advisory lock, which also drains the mutation outbox (each flush
            // is part of the request transaction).
            let mut gateway_shutdown_rx = shutdown.subscribe();

            handles.push(tokio::spawn(async move {
                tracing::debug!(addr = %addr, "Gateway server binding");
                let listener = match bind_listener(addr, tls.as_ref()).await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to bind gateway listener");
                        return;
                    }
                };
                // Serve with per-connection peer address so downstream
                // middleware can resolve the real client IP. Without this the
                // router's default `into_make_service` omits `ConnectInfo`,
                // leaving every client IP unresolved — which collapses per-IP
                // rate-limit buckets, blanks signal visitor IDs, and breaks the
                // IP-bound SSE auth ticket. Mirrors `GatewayServer::run`.
                let serve = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<PeerAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = gateway_shutdown_rx.wait_for(|v| *v).await;
                    tracing::debug!("Gateway draining in-flight requests");
                });
                if let Err(e) = serve.await {
                    tracing::error!("Gateway server error: {}", e);
                }
            }));
        }

        #[cfg(feature = "jobs")]
        let jobs_count = self.job_registry.len();
        #[cfg(not(feature = "jobs"))]
        let jobs_count: usize = 0;
        #[cfg(feature = "cron")]
        let crons_count = self.cron_registry.len();
        #[cfg(not(feature = "cron"))]
        let crons_count: usize = 0;
        #[cfg(feature = "workflows")]
        let workflows_count = self.workflow_registry.len();
        #[cfg(not(feature = "workflows"))]
        let workflows_count: usize = 0;
        #[cfg(feature = "daemons")]
        let daemons_count = self.daemon_registry.len();
        #[cfg(not(feature = "daemons"))]
        let daemons_count: usize = 0;
        #[cfg(feature = "gateway")]
        let webhooks_count = self.webhook_registry.len();
        #[cfg(not(feature = "gateway"))]
        let webhooks_count: usize = 0;
        #[cfg(feature = "gateway")]
        let mcp_tools_count = self.mcp_registry.len();
        #[cfg(not(feature = "gateway"))]
        let mcp_tools_count: usize = 0;

        tracing::info!(
            queries = self.function_registry.queries().count(),
            mutations = self.function_registry.mutations().count(),
            jobs = jobs_count,
            crons = crons_count,
            workflows = workflows_count,
            daemons = daemons_count,
            webhooks = webhooks_count,
            mcp_tools = mcp_tools_count,
            "Functions registered"
        );

        {
            let pool = pool.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    forge_runtime::observability::record_pool_metrics(&pool);
                }
            });
        }

        let role_names: Vec<&str> = roles.iter().map(|r| r.as_str()).collect();
        let capabilities = &self.config.node.worker_capabilities;
        tracing::info!(
            node_id = %node_id,
            project = %self.config.project.name,
            version = env!("CARGO_PKG_VERSION"),
            roles = ?role_names,
            worker_capabilities = ?capabilities,
            port = effective_port,
            db_pool_size = self.config.database.pool_size,
            cluster_discovery = ?self.config.cluster.discovery,
            observability = self.config.observability.enabled,
            mcp = self.config.mcp.enabled,
            "Forge started"
        );

        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::debug!("Received ctrl-c");
            }
            _ = shutdown_rx.recv() => {
                tracing::debug!("Received shutdown notification");
            }
        }

        tracing::debug!("Graceful shutdown starting");

        // No-op if already triggered via `Forge::shutdown()`.
        let _ = self.shutdown_tx.send(());

        // Cancel leader tasks before joining so they wind down concurrently.
        #[cfg(feature = "workflows")]
        workflow_shutdown_token.cancel();

        #[cfg(feature = "cron")]
        if let Some(ref runner) = cron_runner_handle {
            runner.stop().await;
        }

        // Drain leader tasks before releasing the advisory lock to prevent a
        // sibling from starting duplicate work while we are mid-tick.
        tracing::debug!("Waiting for leader-held tasks to drain");
        for handle in leader_handles {
            let _ = handle.await;
        }
        tracing::debug!("Leader-held tasks drained");

        // Advisory lock released here — only after leader tasks have fully stopped.
        if let Err(e) = shutdown.shutdown().await {
            tracing::warn!(error = %e, "Shutdown error");
        }

        if let Some(ref election) = leader_election {
            election.stop();
        }

        #[cfg(feature = "gateway")]
        if let Some(ref reactor) = reactor_handle {
            reactor.stop();
        }

        if let Some(ref db) = self.db {
            db.close().await;
        }

        forge_runtime::shutdown_telemetry();
        tracing::info!("Forge stopped");
        Ok(())
    }

    /// Request shutdown.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    use forge_core::config::NodeRole as ConfigNodeRole;
    use forge_core::mcp::{McpToolAnnotations, McpToolInfo};

    struct TestMcpTool;

    impl forge_core::__sealed::Sealed for TestMcpTool {}

    impl ForgeMcpTool for TestMcpTool {
        type Args = serde_json::Value;
        type Output = serde_json::Value;

        fn info() -> McpToolInfo {
            McpToolInfo {
                name: "test.mcp.tool",
                title: None,
                description: None,
                required_role: None,
                is_public: false,
                timeout: None,
                rate_limit_requests: None,
                rate_limit_per_secs: None,
                rate_limit_key: None,
                annotations: McpToolAnnotations::default(),
                icons: &[],
            }
        }

        fn execute(
            _ctx: &forge_core::McpToolContext,
            _args: Self::Args,
        ) -> Pin<Box<dyn Future<Output = forge_core::Result<Self::Output>> + Send + '_>> {
            Box::pin(async { Ok(serde_json::json!({ "ok": true })) })
        }
    }

    #[test]
    fn test_forge_builder_new() {
        let builder = ForgeBuilder::new();
        assert!(builder.config.is_none());
    }

    #[test]
    fn test_forge_builder_requires_config() {
        let builder = ForgeBuilder::new();
        let result = builder.build();
        assert!(result.is_err());
    }

    #[test]
    fn test_forge_builder_with_config() {
        let config = ForgeConfig::default_with_database_url("postgres://localhost/test");
        let result = ForgeBuilder::new().config(config).build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_forge_builder_register_mcp_tool() {
        let builder = ForgeBuilder::new().register_mcp_tool::<TestMcpTool>();
        assert_eq!(builder.mcp_registry.len(), 1);
    }

    #[test]
    fn test_config_role_conversion() {
        use builder::config_role_to_node_role;
        assert_eq!(
            config_role_to_node_role(&ConfigNodeRole::Gateway),
            NodeRole::Gateway
        );
        assert_eq!(
            config_role_to_node_role(&ConfigNodeRole::Worker),
            NodeRole::Worker
        );
        assert_eq!(
            config_role_to_node_role(&ConfigNodeRole::Scheduler),
            NodeRole::Scheduler
        );
        assert_eq!(
            config_role_to_node_role(&ConfigNodeRole::Function),
            NodeRole::Function
        );
    }
}

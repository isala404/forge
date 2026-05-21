//! FORGE - The Rust Full-Stack Framework
//!
//! Single binary runtime that provides:
//! - HTTP Gateway with RPC endpoints
//! - SSE server for real-time subscriptions
//! - Background job workers
//! - Cron scheduler
//! - Workflow engine
//! - Cluster coordination

mod builder;
pub use builder::ForgeBuilder;

#[cfg(feature = "gateway")]
use std::future::Future;
use std::net::IpAddr;
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
    AuthConfig, GatewayConfig as RuntimeGatewayConfig, GatewayServer, TlsListenConfig,
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
/// Glob-importing this module covers everyday handler code:
///
/// ```ignore
/// use forge::prelude::*;
///
/// #[forge::query(public)]
/// async fn ping(_ctx: &QueryContext) -> Result<String> {
///     Ok("pong".to_string())
/// }
/// ```
///
/// **Stability contract:** the `pub use` items here become part of the
/// framework's stable surface. Removing or renaming an item is a breaking
/// change. Items intentionally absent (e.g. `SchemaRegistry`, `FieldDef`)
/// are reachable via fully-qualified paths in `forge_core` for the rare
/// case that needs them; macro-generated code uses those paths directly.
///
/// **Upstream crates:** re-exporting `axum` (the `custom_routes` factory
/// signature returns `axum::Router`) and `schemars` (via `JsonSchema` for
/// the `#[model]` and `#[mcp_tool]` macros) commits Forge to upgrading
/// these in lockstep with our minor releases. A breaking upstream change
/// would mean a Forge minor or major bump.
pub mod prelude {
    // Common types — stable upstream re-exports.
    pub use chrono::{DateTime, Utc};
    pub use uuid::Uuid;

    // Serde re-exports for user code (load-bearing for serde_json! macro etc.).
    pub use serde::{Deserialize, Serialize};
    pub use serde_json;
    pub use serde_json::Value;

    /// Timestamp type alias for convenience.
    pub type Timestamp = DateTime<Utc>;

    // Core types
    pub use forge_core::auth::TokenPair;
    pub use forge_core::config::ForgeConfig;
    pub use forge_core::cron::{CronContext, ForgeCron};
    pub use forge_core::daemon::{DaemonContext, ForgeDaemon};
    // EnvAccess is a trait that adds `ctx.env(...)` / `ctx.env_require(...)`
    // methods — keeping it in the glob avoids forcing every handler to import
    // it explicitly.
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

    // Same axum version the runtime uses, avoids type mismatches in custom_routes.
    // Only available when the `gateway` feature is enabled.
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
    /// Path to user migrations directory (default: ./migrations).
    pub(super) migrations_dir: PathBuf,
    /// Additional migrations provided programmatically.
    pub(super) extra_migrations: Vec<Migration>,
    /// Optional frontend handler for embedded SPA.
    #[cfg(feature = "gateway")]
    pub(super) frontend_handler: Option<FrontendHandler>,
    /// Factory that produces custom axum routes once the pool is available.
    /// The returned router is merged into the gateway's `/_api` router, so
    /// the full middleware stack (auth, CORS, tracing, concurrency, timeouts)
    /// applies automatically.
    #[cfg(feature = "gateway")]
    pub(super) custom_routes_factory: Option<Box<dyn FnOnce(sqlx::PgPool) -> Router + Send + Sync>>,
    /// Optional pluggable role resolver for RBAC.
    #[cfg(feature = "gateway")]
    pub(super) role_resolver: Option<forge_core::SharedRoleResolver>,
}

impl Forge {
    /// Create a new builder for configuring FORGE.
    pub fn builder() -> ForgeBuilder {
        ForgeBuilder::new()
    }

    /// Get the node ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the configuration.
    pub fn config(&self) -> &ForgeConfig {
        &self.config
    }

    /// Get the function registry.
    pub fn function_registry(&self) -> &FunctionRegistry {
        &self.function_registry
    }

    /// Get the function registry mutably.
    pub fn function_registry_mut(&mut self) -> &mut FunctionRegistry {
        &mut self.function_registry
    }

    /// Get the MCP tool registry mutably.
    #[cfg(feature = "gateway")]
    pub fn mcp_registry_mut(&mut self) -> &mut McpToolRegistry {
        &mut self.mcp_registry
    }

    /// Register an MCP tool without manually accessing the registry.
    #[cfg(feature = "gateway")]
    pub fn register_mcp_tool<T: ForgeMcpTool>(&mut self) -> &mut Self {
        self.mcp_registry.register::<T>();
        self
    }

    /// Get the job registry.
    #[cfg(feature = "jobs")]
    pub fn job_registry(&self) -> &JobRegistry {
        &self.job_registry
    }

    /// Get the job registry mutably.
    #[cfg(feature = "jobs")]
    pub fn job_registry_mut(&mut self) -> &mut JobRegistry {
        &mut self.job_registry
    }

    /// Get the cron registry.
    #[cfg(feature = "cron")]
    pub fn cron_registry(&self) -> Arc<CronRegistry> {
        self.cron_registry.clone()
    }

    /// Get the workflow registry.
    #[cfg(feature = "workflows")]
    pub fn workflow_registry(&self) -> &WorkflowRegistry {
        &self.workflow_registry
    }

    /// Get the workflow registry mutably.
    #[cfg(feature = "workflows")]
    pub fn workflow_registry_mut(&mut self) -> &mut WorkflowRegistry {
        &mut self.workflow_registry
    }

    /// Get the daemon registry.
    #[cfg(feature = "daemons")]
    pub fn daemon_registry(&self) -> Arc<DaemonRegistry> {
        self.daemon_registry.clone()
    }

    /// Get the webhook registry.
    #[cfg(feature = "gateway")]
    pub fn webhook_registry(&self) -> Arc<WebhookRegistry> {
        self.webhook_registry.clone()
    }

    /// Persist all registered workflow definitions to the database.
    /// Fails startup if a definition's signature conflicts with a previously
    /// registered one under the same name+version.
    #[cfg(feature = "workflows")]
    async fn persist_workflow_definitions(&self, pool: &sqlx::PgPool) -> Result<()> {
        for info in self.workflow_registry.definitions() {
            let status = info.status.as_str();

            // Try to insert. If row exists, check signature matches.
            let existing = sqlx::query!(
                r#"
                SELECT workflow_signature FROM forge_workflow_definitions
                WHERE workflow_name = $1 AND workflow_version = $2
                "#,
                info.name,
                info.version,
            )
            .fetch_optional(pool)
            .await
            .map_err(ForgeError::Database)?;

            if let Some(row) = existing {
                if row.workflow_signature != info.signature {
                    return Err(ForgeError::config(format!(
                        "Workflow '{}' version '{}' has a different signature than previously registered. \
                         Persisted contract changed under the same version. \
                         Expected signature: {}, got: {}. \
                         Create a new version instead of modifying the existing one.",
                        info.name, info.version, row.workflow_signature, info.signature
                    )));
                }
                // Update status if changed
                sqlx::query!(
                    "UPDATE forge_workflow_definitions SET status = $3 WHERE workflow_name = $1 AND workflow_version = $2",
                    info.name,
                    info.version,
                    status,
                )
                .execute(pool)
                .await
                .map_err(ForgeError::Database)?;
            } else {
                sqlx::query!(
                    r#"
                    INSERT INTO forge_workflow_definitions (workflow_name, workflow_version, workflow_signature, status)
                    VALUES ($1, $2, $3, $4)
                    "#,
                    info.name,
                    info.version,
                    info.signature,
                    status,
                )
                .execute(pool)
                .await
                .map_err(ForgeError::Database)?;
            }

            tracing::debug!(
                workflow = info.name,
                version = info.version,
                signature = info.signature,
                status = status,
                "Workflow definition registered"
            );
        }

        Ok(())
    }

    /// Run the FORGE server.
    pub async fn run(mut self) -> Result<()> {
        // Users shouldn't need tracing_subscriber boilerplate to see logs
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
            // init_telemetry failed before a subscriber could be installed, so
            // tracing macros would be silently dropped. eprintln! is the fallback.
            Err(e) => eprintln!("forge: failed to initialize telemetry: {e}"),
        }

        tracing::debug!("Connecting to database");

        // Connect to database
        let db =
            Database::from_config_with_service(&self.config.database, &self.config.project.name)
                .await?;
        let pool = db.primary().clone();
        // Health monitor self-terminates on shutdown_tx, so we don't need to
        // hold the JoinHandle. Drop it and let the broadcast signal stop it.
        let _ = db.start_health_monitor(self.shutdown_tx.subscribe());
        self.db = Some(db);

        tracing::debug!("Database connected");

        // Run migrations with mesh-safe locking
        // This acquires an advisory lock, so only one node runs migrations at a time
        let runner = MigrationRunner::new(pool.clone());

        // Load user migrations from directory + any programmatic ones
        let mut user_migrations = load_migrations_from_dir(&self.migrations_dir)?;
        user_migrations.extend(self.extra_migrations.clone());

        runner.run(user_migrations).await?;
        tracing::debug!("Migrations applied");

        // Persist workflow definitions and validate signatures
        #[cfg(feature = "workflows")]
        if !self.workflow_registry.is_empty() {
            self.persist_workflow_definitions(&pool).await?;
        }

        // Get local node info
        let hostname = get_hostname();

        // Support HOST env var (default 0.0.0.0), PORT env var (overrides config)
        let ip_address: IpAddr = std::env::var("HOST")
            .unwrap_or_else(|_| "0.0.0.0".to_string())
            .parse()
            .unwrap_or_else(|_| "0.0.0.0".parse().expect("valid IP literal"));

        if let Ok(port_str) = std::env::var("PORT")
            && let Ok(port) = port_str.parse::<u16>()
        {
            self.config.gateway.port = port;
        }

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
            self.config.gateway.port,
            self.config.gateway.grpc_port,
            roles.clone(),
            self.config.node.worker_capabilities.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
        );

        let node_id = node_info.id;
        self.node_id = node_id;

        // Create node registry
        let node_registry = Arc::new(NodeRegistry::new(pool.clone(), node_info));

        // Register node in cluster
        if let Err(e) = node_registry.register().await {
            tracing::debug!("Failed to register node (tables may not exist): {}", e);
        }

        // Set node status to active
        if let Err(e) = node_registry.set_status(NodeStatus::Active).await {
            tracing::debug!("Failed to set node status: {}", e);
        }

        // Construct the shared PG NOTIFY bus up front so the leader election
        // can subscribe to `forge_leader_released` and react instantly to a
        // sibling's voluntary release instead of waiting for the next
        // `check_interval` tick. The bus is only an in-memory map until
        // `run()` is spawned further down.
        let notify_bus = Arc::new(PgNotifyBus::new(
            pool.clone(),
            &[
                "forge_changes",
                "forge_jobs_available",
                "forge_workflow_wakeup",
                forge_runtime::pg::LEADER_RELEASED_CHANNEL,
            ],
        ));

        // Create leader election for scheduler role
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

        // Create graceful shutdown coordinator
        let shutdown = Arc::new(GracefulShutdown::new(
            node_registry.clone(),
            leader_election.clone(),
            ShutdownConfig::default(),
        ));

        // Create HTTP client with circuit breaker for actions and crons.
        // Used by cron, daemons, and workflow executor for outbound HTTP.
        #[cfg(any(feature = "cron", feature = "daemons", feature = "workflows"))]
        let http_client = CircuitBreakerClient::with_ssrf_protection();

        // Start background tasks based on roles
        let mut handles = Vec::new();
        // Handles for tasks that hold (or depend on) leadership — cron, daemon,
        // workflow scheduler. These must finish before we release the advisory
        // lock so another node cannot acquire leadership and start duplicate work
        // while we are still mid-tick.
        let mut leader_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // Start heartbeat loop
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

        // Start leader election loop if scheduler role
        if let Some(ref election) = leader_election {
            let election = election.clone();
            handles.push(tokio::spawn(async move {
                election.run().await;
            }));
        }

        // Register cron bridge handlers so the worker pool can execute cron jobs.
        #[cfg(feature = "cron")]
        {
            forge_runtime::cron::register_cron_bridges(&self.cron_registry, &mut self.job_registry);
        }

        #[cfg(feature = "jobs")]
        let job_queue = JobQueue::new(pool.clone());

        // Spawn the bus run loop. The gateway role re-uses this same bus via
        // the reactor (which also spawns it), so we gate the direct spawn on
        // the absence of a gateway role to avoid running two run() tasks on
        // the same PgNotifyBus instance.
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

        // Shared KV store handle threaded into all handler contexts.
        // Handlers call `ctx.kv()` to get/set durable key-value data backed by
        // the `forge_kv` / `forge_kv_counters` tables.
        let kv_handle: Arc<dyn forge_core::function::KvHandle> =
            Arc::new(forge_runtime::KvStore::new(pool.clone(), "handlers"));

        // Register the workflow bridge handler BEFORE spawning workers.
        // `JobRegistry` is a plain map cloned by value when handed to each
        // worker; any registration after worker startup is invisible to them
        // and `$workflow_resume` jobs would fail with "unknown job type".
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

        // Start one worker pool per configured queue if worker role.
        //
        // Each queue gets its own Worker instance with a single capability
        // tag, so heavy traffic on `default` cannot starve `workflows` or
        // `cron`. The `default` queue's worker is the only one that also
        // claims jobs whose `worker_capability` is NULL (untagged user jobs).
        // Custom queues are isolated to jobs explicitly tagged with their
        // capability via `JobInfo::worker_capability` or the dispatcher.
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

                let mut worker = Worker::new(
                    worker_config,
                    job_queue.clone(),
                    self.job_registry.clone(),
                    pool.clone(),
                    notify_bus.clone(),
                )
                .with_kv(Arc::clone(&kv_handle));

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

            // Warn when the pool cannot satisfy peak worker demand.
            //
            // Formula: pool_size >= sum(workers per queue) + 6
            //   sum(workers per queue) — one connection held per concurrent job
            //   +6 — persistent connections: change listener, leader advisory lock,
            //         heartbeat, health monitor, migration lock, and headroom
            //
            // Gateway RPC handlers also draw from the pool, but they hold
            // connections briefly and the pool queues requests up to
            // `pool_timeout`. If the worker sum alone already exceeds pool_size,
            // RPC handlers will stall under load.
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

        // KV TTL + rate limit bucket cleanup runs leader-only every 5 minutes.
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

        // Start cron runner if scheduler role and is leader
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

        // Start workflow scheduler if scheduler role
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

        // Create job dispatcher (used by daemon, gateway, webhook routes).
        #[cfg(feature = "jobs")]
        let job_dispatcher = {
            let job_queue_for_dispatch = JobQueue::new(pool.clone());
            Arc::new(JobDispatcher::new(
                job_queue_for_dispatch,
                self.job_registry.clone(),
            ))
        };
        // Reuse the bridge executor for dispatch (daemon, gateway).
        #[cfg(feature = "workflows")]
        let workflow_executor = workflow_bridge_executor;

        // Start daemon runner if scheduler role (daemons run as singletons)
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

        // Reactor handle for shutdown
        #[cfg(feature = "gateway")]
        let mut reactor_handle = None;

        // Start HTTP gateway if gateway role
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
                port: self.config.gateway.port,
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

            // Build gateway server (pass Database wrapper for read replica routing)
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
            // Wire signals (product analytics + diagnostics)
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
                    .with_signals_geoip(geoip);

                // Spawn session reaper
                forge_runtime::signals::session::spawn_session_reaper(
                    signals_pool.clone(),
                    (self.config.signals.session_timeout.as_secs() / 60) as u32,
                );

                // Ensure signal partitions exist at startup
                forge_runtime::signals::partition::ensure_partitions(&signals_pool).await;

                // Spawn daily partition maintenance (leader-only via leader_election)
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
                            // Only run on the leader node
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

            if let Some(factory) = self.custom_routes_factory.take() {
                gateway = gateway.with_custom_routes(factory(pool.clone()));
                tracing::debug!("Custom routes merged into gateway middleware stack");
            }

            // Start the reactor for real-time updates
            let reactor = gateway.reactor();
            if let Err(e) = reactor.start().await {
                tracing::error!("Failed to start reactor: {}", e);
            } else {
                tracing::debug!("Reactor started");
                reactor_handle = Some(reactor);
            }

            // Build API router (all under /_api)
            let api_router = gateway.router();

            // Build final router with API
            let mut router = Router::new().nest("/_api", api_router);

            // Mount webhook routes under /_api (bypasses gateway auth middleware)
            if !self.webhook_registry.is_empty() {
                use axum::routing::post;
                use tower_http::cors::{Any, CorsLayer};

                let webhook_state = WebhookState::new(self.webhook_registry.clone(), pool.clone());
                #[cfg(feature = "jobs")]
                let webhook_state = webhook_state.with_job_dispatcher(job_dispatcher.clone());
                #[cfg(feature = "workflows")]
                let webhook_state =
                    webhook_state.with_workflow_dispatcher(workflow_executor.clone());
                let webhook_state = webhook_state.with_kv(Arc::clone(&kv_handle));
                let webhook_state = Arc::new(webhook_state);

                // Webhook routes need their own CORS layer since they're outside the API router.
                // Reuse gateway CORS policy rather than forcing wildcard access.
                let webhook_cors = if self.config.gateway.cors_enabled
                    || !self.config.gateway.cors_origins.is_empty()
                {
                    if self.config.gateway.cors_origins.iter().any(|o| o == "*") {
                        CorsLayer::new()
                            .allow_origin(Any)
                            .allow_methods(Any)
                            .allow_headers(Any)
                    } else {
                        use axum::http::Method;
                        let origins: Vec<_> = self
                            .config
                            .gateway
                            .cors_origins
                            .iter()
                            .filter_map(|o| o.parse().ok())
                            .collect();
                        CorsLayer::new()
                            .allow_origin(origins)
                            .allow_methods([
                                Method::GET,
                                Method::POST,
                                Method::PUT,
                                Method::DELETE,
                                Method::PATCH,
                                Method::OPTIONS,
                            ])
                            .allow_headers([
                                axum::http::header::CONTENT_TYPE,
                                axum::http::header::AUTHORIZATION,
                                axum::http::header::ACCEPT,
                                axum::http::HeaderName::from_static("x-webhook-signature"),
                                axum::http::HeaderName::from_static("x-idempotency-key"),
                            ])
                            .allow_credentials(true)
                    }
                } else {
                    CorsLayer::new()
                };

                let webhook_router = Router::new()
                    .route("/{*path}", post(webhook_handler).with_state(webhook_state))
                    .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
                    .layer(
                        tower::ServiceBuilder::new()
                            .layer(axum::error_handling::HandleErrorLayer::new(
                                |err: tower::BoxError| async move {
                                    if err.is::<tower::timeout::error::Elapsed>() {
                                        return (
                                            axum::http::StatusCode::REQUEST_TIMEOUT,
                                            "Request timed out",
                                        );
                                    }
                                    (
                                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                        "Server overloaded",
                                    )
                                },
                            ))
                            .layer(tower::limit::ConcurrencyLimitLayer::new(
                                self.config.gateway.max_connections,
                            ))
                            .layer(tower::timeout::TimeoutLayer::new(Duration::from_secs(
                                self.config.gateway.request_timeout.as_secs(),
                            ))),
                    )
                    .layer(webhook_cors);

                router = router.nest("/_api/webhooks", webhook_router);

                tracing::debug!(
                    webhooks = ?self.webhook_registry.paths().collect::<Vec<_>>(),
                    "Webhook routes registered"
                );
            }

            // MCP OAuth: mount OAuth routes or return JSON 404 for discovery
            if self.config.mcp.enabled {
                use axum::routing::get;

                // Well-known discovery routes: either live OAuth metadata (when
                // `mcp-oauth` is compiled in and configured) or a parseable JSON 404
                // that tells clients this server does not support OAuth.
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
                    // OAuth API routes under /_api/oauth/* (bypass auth middleware)
                    router = router.nest("/_api", oauth_api_router);

                    // Well-known metadata at root level
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

            // Add frontend handler as fallback if configured
            if let Some(handler) = self.frontend_handler {
                use axum::routing::get;
                router = router.fallback(get(handler));
                tracing::debug!("Frontend handler enabled");
            }

            let addr = gateway.addr();
            let tls = gateway.tls().cloned();
            // Hand the gateway a shutdown signal so Axum stops accepting new
            // connections and waits for in-flight requests to finish before
            // we release leadership. This is what drains the outbox: each
            // mutation's `dispatch_job`/`start_workflow` flush is part of the
            // request's transaction, so finishing the request finishes the flush.
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
                let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                    let _ = gateway_shutdown_rx.wait_for(|v| *v).await;
                    tracing::debug!("Gateway draining in-flight requests");
                });
                if let Err(e) = serve.await {
                    tracing::error!("Gateway server error: {}", e);
                }
            }));
        }

        // Use 0 as the count for any registry whose feature is disabled.
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

        // Startup banner: summary of config, roles, and capabilities
        let role_names: Vec<&str> = roles.iter().map(|r| r.as_str()).collect();
        let capabilities = &self.config.node.worker_capabilities;
        tracing::info!(
            node_id = %node_id,
            project = %self.config.project.name,
            version = env!("CARGO_PKG_VERSION"),
            roles = ?role_names,
            worker_capabilities = ?capabilities,
            port = self.config.gateway.port,
            db_pool_size = self.config.database.pool_size,
            cluster_discovery = ?self.config.cluster.discovery,
            observability = self.config.observability.enabled,
            mcp = self.config.mcp.enabled,
            "Forge started"
        );

        // Wait for shutdown signal
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::debug!("Received ctrl-c");
            }
            _ = shutdown_rx.recv() => {
                tracing::debug!("Received shutdown notification");
            }
        }

        // Graceful shutdown
        tracing::debug!("Graceful shutdown starting");

        // Broadcast shutdown to all broadcast-subscribed tasks (daemon runner,
        // KV cleanup, partition maintenance). This is a no-op if the shutdown
        // was already triggered via `Forge::shutdown()`.
        let _ = self.shutdown_tx.send(());

        // Signal leader-held tasks to stop. Order matters: cancel these before
        // joining so they all start winding down concurrently.
        #[cfg(feature = "workflows")]
        workflow_shutdown_token.cancel();

        #[cfg(feature = "cron")]
        if let Some(ref runner) = cron_runner_handle {
            runner.stop().await;
        }

        // Wait for all leader-held tasks (cron, daemon, workflow scheduler) to
        // finish before releasing the advisory lock. This prevents another node
        // from acquiring leadership and starting duplicate work while we are
        // still mid-tick.
        tracing::debug!("Waiting for leader-held tasks to drain");
        for handle in leader_handles {
            let _ = handle.await;
        }
        tracing::debug!("Leader-held tasks drained");

        // Release leadership and deregister from cluster. The advisory lock is
        // dropped here; only after leader tasks have fully stopped.
        if let Err(e) = shutdown.shutdown().await {
            tracing::warn!(error = %e, "Shutdown error");
        }

        // Stop leader election
        if let Some(ref election) = leader_election {
            election.stop();
        }

        // Stop reactor before closing database
        #[cfg(feature = "gateway")]
        if let Some(ref reactor) = reactor_handle {
            reactor.stop();
        }

        // Close database connections
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

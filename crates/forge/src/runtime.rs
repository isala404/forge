//! FORGE - The Rust Full-Stack Framework
//!
//! Single binary runtime that provides:
//! - HTTP Gateway with RPC endpoints
//! - SSE server for real-time subscriptions
//! - Background job workers
//! - Cron scheduler
//! - Workflow engine
//! - Cluster coordination

#[cfg(feature = "gateway")]
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
#[cfg(feature = "gateway")]
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

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
use forge_core::config::{ForgeConfig, NodeRole as ConfigNodeRole};
use forge_core::error::{ForgeError, Result};
use forge_core::function::{ForgeMutation, ForgeQuery};
use forge_runtime::migrations::{Migration, MigrationRunner, load_migrations_from_dir};

#[cfg(feature = "gateway")]
use forge_core::mcp::ForgeMcpTool;
use forge_runtime::cluster::{
    GracefulShutdown, HeartbeatConfig, HeartbeatLoop, LeaderConfig, LeaderElection, NodeRegistry,
    ShutdownConfig,
};
#[cfg(feature = "cron")]
use forge_runtime::cron::{CronRegistry, CronRunner, CronRunnerConfig};
#[cfg(feature = "daemons")]
use forge_runtime::daemon::{DaemonRegistry, DaemonRunner};
use forge_runtime::db::Database;
use forge_runtime::function::FunctionRegistry;
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
    pub use forge_core::db::ForgePool;
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
    config: ForgeConfig,
    db: Option<Database>,
    node_id: NodeId,
    function_registry: FunctionRegistry,
    #[cfg(feature = "gateway")]
    mcp_registry: McpToolRegistry,
    #[cfg(feature = "jobs")]
    job_registry: JobRegistry,
    #[cfg(feature = "cron")]
    cron_registry: Arc<CronRegistry>,
    #[cfg(feature = "workflows")]
    workflow_registry: WorkflowRegistry,
    #[cfg(feature = "daemons")]
    daemon_registry: Arc<DaemonRegistry>,
    #[cfg(feature = "gateway")]
    webhook_registry: Arc<WebhookRegistry>,
    shutdown_tx: broadcast::Sender<()>,
    /// Path to user migrations directory (default: ./migrations).
    migrations_dir: PathBuf,
    /// Additional migrations provided programmatically.
    extra_migrations: Vec<Migration>,
    /// Optional frontend handler for embedded SPA.
    #[cfg(feature = "gateway")]
    frontend_handler: Option<FrontendHandler>,
    /// Factory that produces custom axum routes once the pool is available.
    /// The returned router is merged into the gateway's `/_api` router, so
    /// the full middleware stack (auth, CORS, tracing, concurrency, timeouts)
    /// applies automatically.
    #[cfg(feature = "gateway")]
    custom_routes_factory: Option<Box<dyn FnOnce(forge_core::ForgePool) -> Router + Send + Sync>>,
    /// Optional pluggable role resolver for RBAC.
    #[cfg(feature = "gateway")]
    role_resolver: Option<forge_core::SharedRoleResolver>,
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
            .map_err(|e| ForgeError::Database(e.to_string()))?;

            if let Some(row) = existing {
                if row.workflow_signature != info.signature {
                    return Err(ForgeError::Config(format!(
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
                .map_err(|e| ForgeError::Database(e.to_string()))?;
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
                .map_err(|e| ForgeError::Database(e.to_string()))?;
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
        // jobs_pool only used by background subsystems (worker, scheduler).
        #[cfg(any(
            feature = "jobs",
            feature = "cron",
            feature = "workflows",
            feature = "daemons",
        ))]
        let jobs_pool = db.jobs_pool().clone();
        let observability_pool = db.observability_pool().clone();
        if let Some(handle) = db.start_health_monitor() {
            let mut shutdown_rx = self.shutdown_tx.subscribe();
            tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown_rx.recv() => {}
                    _ = handle => {}
                }
            });
        }
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

        // Drain check: any non-terminal workflow run referencing a
        // (name, version) we don't have a handler for is stranded. Boot still
        // succeeds — we just refuse to report ready until the operator
        // resolves them in PG (UPDATE forge_workflow_runs SET status = ...).
        #[cfg(feature = "workflows")]
        let workflow_readiness = forge_runtime::WorkflowReadiness::new();
        #[cfg(feature = "workflows")]
        let workflow_registry_arc = std::sync::Arc::new(self.workflow_registry.clone());
        #[cfg(feature = "workflows")]
        {
            let stranded = workflow_readiness
                .refresh(&workflow_registry_arc, &pool)
                .await?;
            for entry in &stranded {
                tracing::warn!(
                    workflow = %entry.workflow_name,
                    version = %entry.workflow_version,
                    in_flight_count = entry.in_flight_count,
                    oldest_started_at = %entry.oldest_started_at,
                    sample_run_ids = ?entry.sample_run_ids,
                    "Stranded workflow runs detected; readiness probe will return 503 until resolved. \
                     Run: UPDATE forge_workflow_runs SET status = 'cancelled_by_operator' \
                     (or 'retired_unresumable') WHERE id IN (...);"
                );
            }
            if !stranded.is_empty() {
                tracing::warn!(
                    drain_pending = workflow_readiness.drain_pending(),
                    "Boot continuing with drain_pending > 0; /_api/ready will return 503"
                );
            }
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

        // Create leader election for scheduler role
        let leader_election = if roles.contains(&NodeRole::Scheduler) {
            let election = Arc::new(LeaderElection::new(
                pool.clone(),
                node_id,
                LeaderRole::Scheduler,
                LeaderConfig::default(),
            ));

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
        let http_client = CircuitBreakerClient::with_defaults(reqwest::Client::new());

        // Start background tasks based on roles
        let mut handles = Vec::new();

        // Start heartbeat loop
        {
            let heartbeat_pool = pool.clone();
            let heartbeat_node_id = node_id;
            let config = HeartbeatConfig::from_cluster_config(&self.config.cluster);
            handles.push(tokio::spawn(async move {
                let heartbeat = HeartbeatLoop::new(heartbeat_pool, heartbeat_node_id, config);
                heartbeat.run().await;
            }));
        }

        // Start leader election loop if scheduler role
        if let Some(ref election) = leader_election {
            let election = election.clone();
            handles.push(tokio::spawn(async move {
                election.run().await;
            }));
        }

        // Start job worker if worker role
        #[cfg(feature = "jobs")]
        if roles.contains(&NodeRole::Worker) {
            let job_queue = JobQueue::new(jobs_pool.clone());
            let worker_config = WorkerConfig {
                id: Some(node_id.as_uuid()),
                capabilities: self.config.node.worker_capabilities.clone(),
                max_concurrent: self.config.worker.max_concurrent_jobs,
                poll_interval: Duration::from_millis(self.config.worker.poll_interval_ms()),
                ..Default::default()
            };

            let mut worker = Worker::new(
                worker_config,
                job_queue,
                self.job_registry.clone(),
                jobs_pool.clone(),
            );

            handles.push(tokio::spawn(async move {
                if let Err(e) = worker.run().await {
                    tracing::error!("Worker error: {}", e);
                }
            }));

            tracing::debug!("Job worker started");
        }

        // Start cron runner if scheduler role and is leader
        #[cfg(feature = "cron")]
        if roles.contains(&NodeRole::Scheduler) {
            let cron_registry = self.cron_registry.clone();
            let cron_pool = jobs_pool.clone();
            let cron_http = http_client.clone();
            let cron_leader_election = leader_election.clone();

            let cron_config = CronRunnerConfig {
                poll_interval: Duration::from_secs(1),
                node_id: node_id.as_uuid(),
                is_leader: cron_leader_election.is_none(),
                leader_election: cron_leader_election,
                run_stale_threshold: Duration::from_secs(15 * 60),
            };

            let cron_runner = CronRunner::new(cron_registry, cron_pool, cron_http, cron_config);

            handles.push(tokio::spawn(async move {
                if let Err(e) = cron_runner.run().await {
                    tracing::error!("Cron runner error: {}", e);
                }
            }));

            tracing::debug!("Cron scheduler started");
        }

        // Start workflow scheduler if scheduler role
        #[cfg(feature = "workflows")]
        let workflow_shutdown_token = CancellationToken::new();
        #[cfg(feature = "workflows")]
        if roles.contains(&NodeRole::Scheduler) {
            let scheduler_executor = Arc::new(WorkflowExecutor::new(
                Arc::new(self.workflow_registry.clone()),
                jobs_pool.clone(),
                http_client.clone(),
            ));
            let event_store = Arc::new(EventStore::new(jobs_pool.clone()));
            let scheduler = WorkflowScheduler::new(
                jobs_pool.clone(),
                scheduler_executor,
                event_store,
                WorkflowSchedulerConfig::default(),
            );

            let shutdown_token = workflow_shutdown_token.clone();
            handles.push(tokio::spawn(async move {
                scheduler.run(shutdown_token).await;
            }));

            tracing::debug!("Workflow scheduler started");
        }

        // Create job dispatcher (used by daemon, gateway, webhook routes).
        #[cfg(feature = "jobs")]
        let job_dispatcher = {
            let job_queue_for_dispatch = JobQueue::new(jobs_pool.clone());
            Arc::new(JobDispatcher::new(
                job_queue_for_dispatch,
                self.job_registry.clone(),
            ))
        };
        // Create workflow executor for dispatch (used by daemon and gateway).
        #[cfg(feature = "workflows")]
        let workflow_executor = Arc::new(WorkflowExecutor::new(
            Arc::new(self.workflow_registry.clone()),
            jobs_pool.clone(),
            http_client.clone(),
        ));

        // Start daemon runner if scheduler role (daemons run as singletons)
        #[cfg(feature = "daemons")]
        if roles.contains(&NodeRole::Scheduler) && !self.daemon_registry.is_empty() {
            let daemon_registry = self.daemon_registry.clone();
            let daemon_pool = jobs_pool.clone();
            let daemon_http = http_client.clone();
            let daemon_shutdown_rx = self.shutdown_tx.subscribe();

            let daemon_runner = DaemonRunner::new(
                daemon_registry,
                daemon_pool,
                daemon_http,
                node_id.as_uuid(),
                daemon_shutdown_rx,
            );
            #[cfg(feature = "jobs")]
            let daemon_runner = daemon_runner.with_job_dispatch(job_dispatcher.clone());
            #[cfg(feature = "workflows")]
            let daemon_runner = daemon_runner.with_workflow_dispatch(workflow_executor.clone());

            handles.push(tokio::spawn(async move {
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
                return Err(ForgeError::Config(
                    "One or more handlers require authentication (private scope or require_role) \
                     but auth is not configured. Set auth.jwt_secret (≥32 bytes) for HMAC or \
                     auth.jwks_url for external identity providers."
                        .into(),
                ));
            }

            let gateway_config = RuntimeGatewayConfig {
                port: self.config.gateway.port,
                max_connections: self.config.gateway.max_connections,
                sse_max_sessions: self.config.realtime.sse_max_sessions,
                request_timeout_secs: self.config.gateway.request_timeout_secs(),
                cors_enabled: self.config.gateway.cors_enabled,
                cors_origins: self.config.gateway.cors_origins.clone(),
                auth: AuthConfig::from_forge_config(&self.config.auth)
                    .map_err(|e| ForgeError::Config(e.to_string()))?,
                mcp: self.config.mcp.clone(),
                quiet_paths: self.config.gateway.quiet_paths.clone(),
                max_body_size_bytes: self.config.gateway.max_body_size_bytes()?,
                max_file_size_bytes: self.config.gateway.max_file_size_bytes()?,
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
                            debounce_ms: rt.debounce_quiet_ms(),
                            max_debounce_ms: rt.debounce_max_ms(),
                            ..InvalidationConfig::default()
                        },
                        realtime: RuntimeRealtimeConfig {
                            max_subscriptions_per_session: rt.subscription_max_per_session,
                        },
                        max_concurrent_reexecutions: rt.max_concurrent_reexecutions,
                        resync_interval_secs: rt.resync_interval_secs(),
                        shard_count: rt.shard_count,
                        ..ReactorConfig::default()
                    }
                },
                max_rpc_batch_size: self.config.gateway.max_rpc_batch_size,
                max_multipart_fields: self.config.gateway.max_multipart_fields,
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
            };

            // Build gateway server (pass Database wrapper for read replica routing)
            let db_ref = self
                .db
                .clone()
                .ok_or_else(|| ForgeError::Internal("Database not initialized".into()))?;

            let gateway = GatewayServer::new(
                gateway_config,
                self.function_registry.clone(),
                db_ref.clone(),
            );
            #[cfg(feature = "jobs")]
            let gateway = gateway.with_job_dispatcher(job_dispatcher.clone());
            #[cfg(feature = "workflows")]
            let gateway = gateway.with_workflow_dispatcher(workflow_executor.clone());
            let mut gateway = gateway.with_mcp_registry(self.mcp_registry.clone());

            let rate_limiter: std::sync::Arc<dyn forge_core::rate_limit::RateLimiterBackend> =
                match self.config.rate_limit.mode {
                    forge_core::config::RateLimitMode::Strict => std::sync::Arc::new(
                        forge_runtime::StrictRateLimiter::new(db_ref.primary().clone()),
                    ),
                    forge_core::config::RateLimitMode::Hybrid => std::sync::Arc::new(
                        forge_runtime::HybridRateLimiter::new(db_ref.primary().clone()),
                    ),
                };
            gateway = gateway.with_rate_limiter(rate_limiter);
            if let Some(resolver) = self.role_resolver.take() {
                gateway = gateway.with_role_resolver(resolver);
            }
            #[cfg(feature = "workflows")]
            {
                gateway = gateway.with_workflow_readiness(
                    workflow_registry_arc.clone(),
                    workflow_readiness.clone(),
                );
            }

            // Wire signals (product analytics + diagnostics)
            if self.config.signals.enabled {
                let signals_pool = std::sync::Arc::new(db_ref.analytics_pool().clone());
                let collector = forge_runtime::signals::SignalsCollector::spawn(
                    signals_pool.clone(),
                    self.config.signals.batch_size,
                    self.config.signals.flush_interval_duration(),
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
                    signals_pool,
                    self.config.signals.session_timeout_mins(),
                );

                tracing::info!("Signals enabled (analytics + diagnostics)");
            }

            if let Some(factory) = self.custom_routes_factory.take() {
                let forge_pool = forge_core::ForgePool::from_sqlx(pool.clone());
                gateway = gateway.with_custom_routes(factory(forge_pool));
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
                                self.config.gateway.request_timeout_secs(),
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
                    // OAuth not configured: return parseable JSON 404
                    async fn oauth_not_supported() -> impl axum::response::IntoResponse {
                        (
                            axum::http::StatusCode::NOT_FOUND,
                            axum::Json(serde_json::json!({
                                "error": "oauth_not_supported",
                                "error_description": "This server does not support OAuth. Connect without authentication."
                            })),
                        )
                    }
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
                    let _ = gateway_shutdown_rx.recv().await;
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
            let metrics_pool = observability_pool;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    forge_runtime::observability::record_pool_metrics(&metrics_pool);
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

        // Stop workflow scheduler
        #[cfg(feature = "workflows")]
        workflow_shutdown_token.cancel();

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

/// Builder for configuring the FORGE runtime.
pub struct ForgeBuilder {
    config: Option<ForgeConfig>,
    function_registry: FunctionRegistry,
    #[cfg(feature = "gateway")]
    role_resolver: Option<forge_core::SharedRoleResolver>,
    #[cfg(feature = "gateway")]
    mcp_registry: McpToolRegistry,
    #[cfg(feature = "jobs")]
    job_registry: JobRegistry,
    #[cfg(feature = "cron")]
    cron_registry: CronRegistry,
    #[cfg(feature = "workflows")]
    workflow_registry: WorkflowRegistry,
    #[cfg(feature = "daemons")]
    daemon_registry: DaemonRegistry,
    #[cfg(feature = "gateway")]
    webhook_registry: WebhookRegistry,
    migrations_dir: PathBuf,
    extra_migrations: Vec<Migration>,
    #[cfg(feature = "gateway")]
    frontend_handler: Option<FrontendHandler>,
    #[cfg(feature = "gateway")]
    custom_routes_factory: Option<Box<dyn FnOnce(forge_core::ForgePool) -> Router + Send + Sync>>,
}

impl ForgeBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            config: None,
            function_registry: FunctionRegistry::new(),
            #[cfg(feature = "gateway")]
            role_resolver: None,
            #[cfg(feature = "gateway")]
            mcp_registry: McpToolRegistry::new(),
            #[cfg(feature = "jobs")]
            job_registry: JobRegistry::new(),
            #[cfg(feature = "cron")]
            cron_registry: CronRegistry::new(),
            #[cfg(feature = "workflows")]
            workflow_registry: WorkflowRegistry::new(),
            #[cfg(feature = "daemons")]
            daemon_registry: DaemonRegistry::new(),
            #[cfg(feature = "gateway")]
            webhook_registry: WebhookRegistry::new(),
            migrations_dir: PathBuf::from("migrations"),
            extra_migrations: Vec::new(),
            #[cfg(feature = "gateway")]
            frontend_handler: None,
            #[cfg(feature = "gateway")]
            custom_routes_factory: None,
        }
    }

    /// Set the directory to load migrations from.
    ///
    /// Defaults to `./migrations`. Migration files should be named like:
    /// - `0001_create_users.sql`
    /// - `0002_add_posts.sql`
    pub fn migrations_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.migrations_dir = path.into();
        self
    }

    /// Add a migration programmatically.
    ///
    /// Use this for migrations that need to be generated at runtime,
    /// or for testing. For most cases, use migration files instead.
    pub fn migration(mut self, name: impl Into<String>, sql: impl Into<String>) -> Self {
        self.extra_migrations.push(Migration::new(name, sql));
        self
    }

    /// Set a frontend handler for serving embedded SPA assets.
    ///
    /// Use with the `embedded-frontend` feature to build a single binary
    /// that includes both backend and frontend.
    #[cfg(feature = "gateway")]
    pub fn frontend_handler(mut self, handler: FrontendHandler) -> Self {
        self.frontend_handler = Some(handler);
        self
    }

    /// Plug in a custom role resolver for RBAC extension.
    ///
    /// The default resolver returns the flat `roles` JWT claim as-is. Use
    /// this to expand roles hierarchically, perform group-membership lookups,
    /// or consult an external permission service.
    ///
    /// The resolver is called for every request that carries a `require_role`
    /// constraint. Keep it cheap — cache remote lookups internally.
    #[cfg(feature = "gateway")]
    pub fn with_role_resolver(mut self, resolver: forge_core::SharedRoleResolver) -> Self {
        self.role_resolver = Some(resolver);
        self
    }

    /// Register custom axum routes built from Forge's managed pool.
    ///
    /// The factory runs once during `run()`, after the database pool is
    /// connected. The returned router is merged into the gateway's `/_api`
    /// namespace, so every route receives the full middleware stack: auth
    /// (JWT), CORS, tracing, concurrency limits, and timeouts.
    ///
    /// Route paths are relative to `/_api`. Registering `/export/csv`
    /// exposes `GET /_api/export/csv`. Avoid paths that collide with
    /// built-ins under `/_api`: `/health`, `/ready`, `/rpc`, `/rpc/*`,
    /// `/events`, `/subscribe`, `/unsubscribe`, `/subscribe-job`,
    /// `/subscribe-workflow`, `/signal/*`, `/mcp`, and `/oauth/*`.
    ///
    /// The factory receives a [`forge_core::ForgePool`] — Forge's stable
    /// wrapper around the underlying database pool. Drop down to sqlx with
    /// `pool.as_sqlx_pool()` when you need to issue queries directly.
    /// Pinning the wrapper lets Forge change its sqlx version (or swap
    /// the driver entirely) without breaking your custom routes.
    ///
    /// If your handlers don't need the pool, ignore the argument:
    ///
    /// ```ignore
    /// builder.custom_routes(|_| Router::new().route("/healthz", get(|| async { "ok" })));
    /// ```
    ///
    /// With pool access:
    ///
    /// ```ignore
    /// use axum::{Router, routing::get, extract::State};
    /// use std::sync::Arc;
    ///
    /// builder.custom_routes(|pool| {
    ///     // Use `pool.as_sqlx_pool()` inside handlers when you need sqlx.
    ///     Router::new()
    ///         .route("/export/csv", get(export_handler))
    ///         .with_state(Arc::new(pool))
    /// });
    /// ```
    #[cfg(feature = "gateway")]
    pub fn custom_routes<F>(mut self, f: F) -> Self
    where
        F: FnOnce(forge_core::ForgePool) -> Router + Send + Sync + 'static,
    {
        self.custom_routes_factory = Some(Box::new(f));
        self
    }

    /// Automatically register all functions discovered via `#[forge::query]`,
    /// `#[forge::mutation]`, `#[forge::job]`, `#[forge::cron]`, `#[forge::workflow]`,
    /// `#[forge::daemon]`, `#[forge::webhook]`, and `#[forge::mcp_tool]` macros.
    ///
    /// This replaces the need to manually call `.register_query::<T>()` etc.
    /// for every function in your application.
    pub fn auto_register(mut self) -> Self {
        crate::auto_register::auto_register_all(
            &mut self.function_registry,
            #[cfg(feature = "jobs")]
            &mut self.job_registry,
            #[cfg(feature = "cron")]
            &mut self.cron_registry,
            #[cfg(feature = "workflows")]
            &mut self.workflow_registry,
            #[cfg(feature = "daemons")]
            &mut self.daemon_registry,
            #[cfg(feature = "gateway")]
            &mut self.webhook_registry,
            #[cfg(feature = "gateway")]
            &mut self.mcp_registry,
        );
        self
    }

    /// Set the configuration.
    pub fn config(mut self, config: ForgeConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Get mutable access to the function registry.
    pub fn function_registry_mut(&mut self) -> &mut FunctionRegistry {
        &mut self.function_registry
    }

    /// Get mutable access to the job registry.
    #[cfg(feature = "jobs")]
    pub fn job_registry_mut(&mut self) -> &mut JobRegistry {
        &mut self.job_registry
    }

    /// Get mutable access to the MCP tool registry.
    #[cfg(feature = "gateway")]
    pub fn mcp_registry_mut(&mut self) -> &mut McpToolRegistry {
        &mut self.mcp_registry
    }

    /// Register an MCP tool without manually accessing the registry.
    #[cfg(feature = "gateway")]
    pub fn register_mcp_tool<T: ForgeMcpTool>(mut self) -> Self {
        self.mcp_registry.register::<T>();
        self
    }

    /// Get mutable access to the cron registry.
    #[cfg(feature = "cron")]
    pub fn cron_registry_mut(&mut self) -> &mut CronRegistry {
        &mut self.cron_registry
    }

    /// Get mutable access to the workflow registry.
    #[cfg(feature = "workflows")]
    pub fn workflow_registry_mut(&mut self) -> &mut WorkflowRegistry {
        &mut self.workflow_registry
    }

    /// Get mutable access to the daemon registry.
    #[cfg(feature = "daemons")]
    pub fn daemon_registry_mut(&mut self) -> &mut DaemonRegistry {
        &mut self.daemon_registry
    }

    /// Get mutable access to the webhook registry.
    #[cfg(feature = "gateway")]
    pub fn webhook_registry_mut(&mut self) -> &mut WebhookRegistry {
        &mut self.webhook_registry
    }

    /// Register a query function.
    pub fn register_query<Q: ForgeQuery>(mut self) -> Self
    where
        Q::Args: serde::de::DeserializeOwned + Send + 'static,
        Q::Output: serde::Serialize + Send + 'static,
    {
        self.function_registry.register_query::<Q>();
        self
    }

    /// Register a mutation function.
    pub fn register_mutation<M: ForgeMutation>(mut self) -> Self
    where
        M::Args: serde::de::DeserializeOwned + Send + 'static,
        M::Output: serde::Serialize + Send + 'static,
    {
        self.function_registry.register_mutation::<M>();
        self
    }

    /// Register a background job.
    #[cfg(feature = "jobs")]
    pub fn register_job<J: forge_core::ForgeJob>(mut self) -> Self
    where
        J::Args: serde::de::DeserializeOwned + Send + 'static,
        J::Output: serde::Serialize + Send + 'static,
    {
        self.job_registry.register::<J>();
        self
    }

    /// Register a cron handler.
    #[cfg(feature = "cron")]
    pub fn register_cron<C: forge_core::ForgeCron>(mut self) -> Self {
        self.cron_registry.register::<C>();
        self
    }

    /// Register a workflow.
    #[cfg(feature = "workflows")]
    pub fn register_workflow<W: forge_core::ForgeWorkflow>(mut self) -> Self
    where
        W::Input: serde::de::DeserializeOwned,
        W::Output: serde::Serialize,
    {
        self.workflow_registry.register::<W>();
        self
    }

    /// Register a daemon.
    #[cfg(feature = "daemons")]
    pub fn register_daemon<D: forge_core::ForgeDaemon>(mut self) -> Self {
        self.daemon_registry.register::<D>();
        self
    }

    /// Register a webhook.
    #[cfg(feature = "gateway")]
    pub fn register_webhook<W: forge_core::ForgeWebhook>(mut self) -> Self {
        self.webhook_registry.register::<W>();
        self
    }

    /// Build the FORGE runtime.
    pub fn build(self) -> Result<Forge> {
        let config = self
            .config
            .ok_or_else(|| ForgeError::Config("Configuration is required".to_string()))?;

        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Forge {
            config,
            db: None,
            node_id: NodeId::new(),
            function_registry: self.function_registry,
            #[cfg(feature = "gateway")]
            mcp_registry: self.mcp_registry,
            #[cfg(feature = "jobs")]
            job_registry: self.job_registry,
            #[cfg(feature = "cron")]
            cron_registry: Arc::new(self.cron_registry),
            #[cfg(feature = "workflows")]
            workflow_registry: self.workflow_registry,
            #[cfg(feature = "daemons")]
            daemon_registry: Arc::new(self.daemon_registry),
            #[cfg(feature = "gateway")]
            webhook_registry: Arc::new(self.webhook_registry),
            shutdown_tx,
            migrations_dir: self.migrations_dir,
            extra_migrations: self.extra_migrations,
            #[cfg(feature = "gateway")]
            frontend_handler: self.frontend_handler,
            #[cfg(feature = "gateway")]
            custom_routes_factory: self.custom_routes_factory,
            #[cfg(feature = "gateway")]
            role_resolver: self.role_resolver,
        })
    }
}

impl Default for ForgeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
fn get_hostname() -> String {
    nix::unistd::gethostname()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(not(unix))]
fn get_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Convert config NodeRole to cluster NodeRole.
fn config_role_to_node_role(role: &ConfigNodeRole) -> NodeRole {
    match role {
        ConfigNodeRole::Gateway => NodeRole::Gateway,
        ConfigNodeRole::Function => NodeRole::Function,
        ConfigNodeRole::Worker => NodeRole::Worker,
        ConfigNodeRole::Scheduler => NodeRole::Scheduler,
        // ConfigNodeRole is #[non_exhaustive]; default unknown future roles to
        // Function so the node can still serve RPCs while the runtime catches up.
        _ => NodeRole::Function,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

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

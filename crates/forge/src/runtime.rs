//! FORGE - The Rust Full-Stack Framework
//!
//! Single binary runtime that provides:
//! - HTTP Gateway with RPC endpoints
//! - WebSocket server for real-time subscriptions
//! - Background job workers
//! - Cron scheduler
//! - Workflow engine
//! - Observability dashboard
//! - Cluster coordination

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

use forge_core::cluster::{LeaderRole, NodeId, NodeInfo, NodeRole, NodeStatus};
use forge_core::config::{ForgeConfig, NodeRole as ConfigNodeRole};
use forge_core::error::{ForgeError, Result};
use forge_runtime::migrations::{load_migrations_from_dir, Migration, MigrationRunner};

use forge_runtime::cluster::{
    GracefulShutdown, HeartbeatConfig, HeartbeatLoop, LeaderConfig, LeaderElection, NodeRegistry,
    ShutdownConfig,
};
use forge_runtime::cron::{CronRegistry, CronRunner, CronRunnerConfig};
use forge_runtime::dashboard::{
    create_api_router, create_dashboard_router, DashboardConfig, DashboardState,
};
use forge_runtime::db::Database;
use forge_runtime::function::FunctionRegistry;
use forge_runtime::gateway::{AuthConfig, GatewayConfig as RuntimeGatewayConfig, GatewayServer};
use forge_runtime::jobs::{JobQueue, JobRegistry, Worker, WorkerConfig};
use forge_runtime::realtime::{WebSocketConfig, WebSocketServer};
use forge_runtime::workflow::WorkflowRegistry;

/// Prelude module for common imports.
pub mod prelude {
    // Common types
    pub use chrono::{DateTime, Utc};
    pub use uuid::Uuid;

    /// Timestamp type alias for convenience.
    pub type Timestamp = DateTime<Utc>;

    // Core types
    pub use forge_core::cluster::NodeRole;
    pub use forge_core::config::ForgeConfig;
    pub use forge_core::cron::{CronContext, ForgeCron};
    pub use forge_core::error::{ForgeError, Result};
    pub use forge_core::function::{
        ActionContext, AuthContext, ForgeMutation, ForgeQuery, MutationContext, QueryContext,
    };
    pub use forge_core::job::{ForgeJob, JobContext, JobPriority};
    pub use forge_core::realtime::Delta;
    pub use forge_core::schema::{FieldDef, ModelMeta, SchemaRegistry, TableDef};
    pub use forge_core::workflow::{ForgeWorkflow, WorkflowContext};

    pub use crate::{Forge, ForgeBuilder};
}

/// The main FORGE runtime.
pub struct Forge {
    config: ForgeConfig,
    db: Option<Database>,
    node_id: NodeId,
    function_registry: FunctionRegistry,
    job_registry: JobRegistry,
    cron_registry: Arc<CronRegistry>,
    workflow_registry: WorkflowRegistry,
    shutdown_tx: broadcast::Sender<()>,
    /// Path to user migrations directory (default: ./migrations).
    migrations_dir: PathBuf,
    /// Additional migrations provided programmatically.
    extra_migrations: Vec<Migration>,
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

    /// Get the job registry.
    pub fn job_registry(&self) -> &JobRegistry {
        &self.job_registry
    }

    /// Get the job registry mutably.
    pub fn job_registry_mut(&mut self) -> &mut JobRegistry {
        &mut self.job_registry
    }

    /// Get the cron registry.
    pub fn cron_registry(&self) -> Arc<CronRegistry> {
        self.cron_registry.clone()
    }

    /// Get the workflow registry.
    pub fn workflow_registry(&self) -> &WorkflowRegistry {
        &self.workflow_registry
    }

    /// Get the workflow registry mutably.
    pub fn workflow_registry_mut(&mut self) -> &mut WorkflowRegistry {
        &mut self.workflow_registry
    }

    /// Run the FORGE server.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!("FORGE runtime starting");

        // Connect to database
        let db = Database::from_config(&self.config.database).await?;
        let pool = db.primary().clone();
        self.db = Some(db);

        tracing::info!("Connected to database");

        // Run migrations with mesh-safe locking
        // This acquires an advisory lock, so only one node runs migrations at a time
        let runner = MigrationRunner::new(pool.clone());

        // Load user migrations from directory + any programmatic ones
        let mut user_migrations = load_migrations_from_dir(&self.migrations_dir)?;
        user_migrations.extend(self.extra_migrations.clone());

        runner.run(user_migrations).await?;
        tracing::info!("Migrations completed");

        // Get local node info
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let ip_address: IpAddr = "127.0.0.1".parse().unwrap();
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
            tracing::warn!("Failed to register node (tables may not exist): {}", e);
        }

        // Set node status to active
        if let Err(e) = node_registry.set_status(NodeStatus::Active).await {
            tracing::warn!("Failed to set node status: {}", e);
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
                tracing::warn!("Failed to acquire leadership: {}", e);
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

        // Create HTTP client for actions and crons
        let http_client = reqwest::Client::new();

        // Start background tasks based on roles
        let mut handles = Vec::new();

        // Start heartbeat loop
        {
            let heartbeat_pool = pool.clone();
            let heartbeat_node_id = node_id;
            let config = HeartbeatConfig::default();
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
        if roles.contains(&NodeRole::Worker) {
            let job_queue = JobQueue::new(pool.clone());
            let worker_config = WorkerConfig {
                id: Some(node_id.as_uuid()),
                capabilities: self.config.node.worker_capabilities.clone(),
                max_concurrent: self.config.worker.max_concurrent_jobs,
                poll_interval: Duration::from_millis(self.config.worker.poll_interval_ms),
                ..Default::default()
            };

            let mut worker = Worker::new(
                worker_config,
                job_queue,
                self.job_registry.clone(),
                pool.clone(),
            );

            handles.push(tokio::spawn(async move {
                if let Err(e) = worker.run().await {
                    tracing::error!("Worker error: {}", e);
                }
            }));

            tracing::info!("Job worker started");
        }

        // Start cron runner if scheduler role and is leader
        if roles.contains(&NodeRole::Scheduler) {
            let cron_registry = self.cron_registry.clone();
            let cron_pool = pool.clone();
            let cron_http = http_client.clone();
            let is_leader = leader_election
                .as_ref()
                .map(|e| e.is_leader())
                .unwrap_or(false);

            let cron_config = CronRunnerConfig {
                poll_interval: Duration::from_secs(1),
                node_id: node_id.as_uuid(),
                is_leader,
            };

            let cron_runner = CronRunner::new(cron_registry, cron_pool, cron_http, cron_config);

            handles.push(tokio::spawn(async move {
                if let Err(e) = cron_runner.run().await {
                    tracing::error!("Cron runner error: {}", e);
                }
            }));

            tracing::info!("Cron scheduler started");
        }

        // Start HTTP gateway if gateway role
        if roles.contains(&NodeRole::Gateway) {
            let gateway_config = RuntimeGatewayConfig {
                port: self.config.gateway.port,
                max_connections: self.config.gateway.max_connections,
                request_timeout_secs: self.config.gateway.request_timeout_secs,
                cors_enabled: true,
                cors_origins: vec!["*".to_string()],
                auth: AuthConfig::default(),
            };

            // Create dashboard state
            let dashboard_state = DashboardState {
                pool: pool.clone(),
                config: DashboardConfig::default(),
            };

            // Build gateway router with dashboard
            let gateway =
                GatewayServer::new(gateway_config, self.function_registry.clone(), pool.clone());

            // Start the reactor for real-time updates
            let reactor = gateway.reactor();
            if let Err(e) = reactor.start().await {
                tracing::error!("Failed to start reactor: {}", e);
            } else {
                tracing::info!("Reactor started for real-time updates");
            }

            let mut router = gateway.router();

            // Mount dashboard at /_dashboard
            router = router
                .nest(
                    "/_dashboard",
                    create_dashboard_router(dashboard_state.clone()),
                )
                .nest("/_dashboard/api", create_api_router(dashboard_state));

            let addr = gateway.addr();

            handles.push(tokio::spawn(async move {
                tracing::info!("Gateway server listening on {}", addr);
                let listener = tokio::net::TcpListener::bind(addr)
                    .await
                    .expect("Failed to bind");
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!("Gateway server error: {}", e);
                }
            }));

            tracing::info!("HTTP gateway started on port {}", self.config.gateway.port);
        }

        // Start WebSocket server if gateway role
        if roles.contains(&NodeRole::Gateway) {
            let ws_config = WebSocketConfig::default();
            let ws_server = WebSocketServer::new(node_id, ws_config);

            // WebSocket upgrade handling would be added to the gateway router
            // For now, we just hold the server state
            tracing::info!("WebSocket server initialized");
        }

        tracing::info!("FORGE runtime started successfully");
        tracing::info!("  Node ID: {}", node_id);
        tracing::info!("  Roles: {:?}", roles);

        // Wait for shutdown signal
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received shutdown signal");
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("Received shutdown notification");
            }
        }

        // Graceful shutdown
        tracing::info!("Starting graceful shutdown...");

        if let Err(e) = shutdown.shutdown().await {
            tracing::warn!("Shutdown error: {}", e);
        }

        // Stop leader election
        if let Some(ref election) = leader_election {
            election.stop();
        }

        // Close database connections
        if let Some(ref db) = self.db {
            db.close().await;
        }

        tracing::info!("FORGE runtime stopped");
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
    job_registry: JobRegistry,
    cron_registry: CronRegistry,
    workflow_registry: WorkflowRegistry,
    migrations_dir: PathBuf,
    extra_migrations: Vec<Migration>,
}

impl ForgeBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            config: None,
            function_registry: FunctionRegistry::new(),
            job_registry: JobRegistry::new(),
            cron_registry: CronRegistry::new(),
            workflow_registry: WorkflowRegistry::new(),
            migrations_dir: PathBuf::from("migrations"),
            extra_migrations: Vec::new(),
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
    pub fn job_registry_mut(&mut self) -> &mut JobRegistry {
        &mut self.job_registry
    }

    /// Get mutable access to the cron registry.
    pub fn cron_registry_mut(&mut self) -> &mut CronRegistry {
        &mut self.cron_registry
    }

    /// Get mutable access to the workflow registry.
    pub fn workflow_registry_mut(&mut self) -> &mut WorkflowRegistry {
        &mut self.workflow_registry
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
            job_registry: self.job_registry,
            cron_registry: Arc::new(self.cron_registry),
            workflow_registry: self.workflow_registry,
            shutdown_tx,
            migrations_dir: self.migrations_dir,
            extra_migrations: self.extra_migrations,
        })
    }
}

impl Default for ForgeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert config NodeRole to cluster NodeRole.
fn config_role_to_node_role(role: &ConfigNodeRole) -> NodeRole {
    match role {
        ConfigNodeRole::Gateway => NodeRole::Gateway,
        ConfigNodeRole::Function => NodeRole::Function,
        ConfigNodeRole::Worker => NodeRole::Worker,
        ConfigNodeRole::Scheduler => NodeRole::Scheduler,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

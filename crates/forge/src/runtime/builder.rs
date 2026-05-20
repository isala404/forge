//! Builder for configuring the FORGE runtime.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "gateway")]
use axum::Router;
use tokio::sync::broadcast;

use forge_core::cluster::{NodeId, NodeRole};
use forge_core::config::{ForgeConfig, NodeRole as ConfigNodeRole};
use forge_core::error::{ForgeError, Result};
use forge_core::function::{ForgeMutation, ForgeQuery};
use forge_runtime::pg::migration::Migration;

#[cfg(feature = "gateway")]
use forge_core::mcp::ForgeMcpTool;
#[cfg(feature = "cron")]
use forge_runtime::cron::CronRegistry;
#[cfg(feature = "daemons")]
use forge_runtime::daemon::DaemonRegistry;
use forge_runtime::function::FunctionRegistry;
#[cfg(feature = "jobs")]
use forge_runtime::jobs::JobRegistry;
#[cfg(feature = "gateway")]
use forge_runtime::mcp::McpToolRegistry;
#[cfg(feature = "gateway")]
use forge_runtime::webhook::WebhookRegistry;
#[cfg(feature = "workflows")]
use forge_runtime::workflow::WorkflowRegistry;

use super::{Forge, FrontendHandler};

/// Builder for configuring the FORGE runtime.
pub struct ForgeBuilder {
    pub(super) config: Option<ForgeConfig>,
    pub(super) function_registry: FunctionRegistry,
    #[cfg(feature = "gateway")]
    pub(super) role_resolver: Option<forge_core::SharedRoleResolver>,
    #[cfg(feature = "gateway")]
    pub(super) mcp_registry: McpToolRegistry,
    #[cfg(feature = "jobs")]
    pub(super) job_registry: JobRegistry,
    #[cfg(feature = "cron")]
    pub(super) cron_registry: CronRegistry,
    #[cfg(feature = "workflows")]
    pub(super) workflow_registry: WorkflowRegistry,
    #[cfg(feature = "daemons")]
    pub(super) daemon_registry: DaemonRegistry,
    #[cfg(feature = "gateway")]
    pub(super) webhook_registry: WebhookRegistry,
    pub(super) migrations_dir: PathBuf,
    pub(super) extra_migrations: Vec<Migration>,
    #[cfg(feature = "gateway")]
    pub(super) frontend_handler: Option<FrontendHandler>,
    #[cfg(feature = "gateway")]
    pub(super) custom_routes_factory: Option<Box<dyn FnOnce(sqlx::PgPool) -> Router + Send + Sync>>,
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
    /// The factory receives the framework's `sqlx::PgPool`. Cloning it is
    /// cheap (`PgPool` is internally an `Arc`).
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
    /// use axum::{Router, routing::get};
    ///
    /// builder.custom_routes(|pool| {
    ///     Router::new()
    ///         .route("/export/csv", get(export_handler))
    ///         .with_state(pool)
    /// });
    /// ```
    #[cfg(feature = "gateway")]
    pub fn custom_routes<F>(mut self, f: F) -> Self
    where
        F: FnOnce(sqlx::PgPool) -> Router + Send + Sync + 'static,
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
        let mut registries = crate::auto_register::HandlerRegistries {
            functions: std::mem::take(&mut self.function_registry),
            #[cfg(feature = "jobs")]
            jobs: std::mem::take(&mut self.job_registry),
            #[cfg(feature = "cron")]
            crons: std::mem::take(&mut self.cron_registry),
            #[cfg(feature = "workflows")]
            workflows: std::mem::take(&mut self.workflow_registry),
            #[cfg(feature = "daemons")]
            daemons: std::mem::take(&mut self.daemon_registry),
            #[cfg(feature = "gateway")]
            webhooks: std::mem::take(&mut self.webhook_registry),
            #[cfg(feature = "gateway")]
            mcp_tools: std::mem::take(&mut self.mcp_registry),
        };
        crate::auto_register::auto_register_all(&mut registries);
        self.function_registry = registries.functions;
        #[cfg(feature = "jobs")]
        {
            self.job_registry = registries.jobs;
        }
        #[cfg(feature = "cron")]
        {
            self.cron_registry = registries.crons;
        }
        #[cfg(feature = "workflows")]
        {
            self.workflow_registry = registries.workflows;
        }
        #[cfg(feature = "daemons")]
        {
            self.daemon_registry = registries.daemons;
        }
        #[cfg(feature = "gateway")]
        {
            self.webhook_registry = registries.webhooks;
            self.mcp_registry = registries.mcp_tools;
        }
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
            .ok_or_else(|| ForgeError::config("Configuration is required"))?;

        config.auth.validate()?;

        let (shutdown_tx, _) = broadcast::channel(1);

        #[cfg(feature = "workflows")]
        let workflow_registry = {
            let mut reg = self.workflow_registry;
            reg.signature_check = config.workflow.signature_check;
            reg
        };

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
            workflow_registry,
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
pub(super) fn get_hostname() -> String {
    nix::unistd::gethostname()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(not(unix))]
pub(super) fn get_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Convert config NodeRole to cluster NodeRole.
pub(super) fn config_role_to_node_role(role: &ConfigNodeRole) -> NodeRole {
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

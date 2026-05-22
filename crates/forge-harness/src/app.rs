use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge::HandlerRegistries;
use forge_core::TokenIssuer;
use forge_core::function::{JobDispatch, WorkflowDispatch};
use forge_runtime::cron::CronRegistry;
use forge_runtime::daemon::DaemonRegistry;
use forge_runtime::function::FunctionRegistry;
use forge_runtime::gateway::{AuthConfig, GatewayConfig, GatewayServer, HmacTokenIssuer};
use forge_runtime::jobs::{JobDispatcher, JobQueue, JobRegistry, Worker, WorkerConfig};
use forge_runtime::mcp::McpToolRegistry;
use forge_runtime::pg::{Database, PgNotifyBus};
use forge_runtime::webhook::{WebhookRegistry, WebhookState, webhook_handler};
use forge_runtime::workflow::{
    EventStore, WorkflowExecutor, WorkflowRegistry, WorkflowScheduler, WorkflowSchedulerConfig,
};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::client::HarnessClient;
use crate::error::HarnessError;
use crate::sse::HarnessSession;
use crate::{Result, sse};

/// Minimum bytes for an HMAC JWT secret (matches the framework's startup validator).
const TEST_JWT_SECRET: &str = "forge-harness-test-jwt-secret-please-rotate-32b";

/// Builder for the in-process harness app. Use this to override defaults
/// before starting; the simple path is `HarnessApp::start(test_name)`.
pub struct HarnessAppBuilder {
    test_name: String,
    migrations_dir: Option<PathBuf>,
    jwt_secret: String,
    extra_internal_sql: Vec<String>,
    cors_enabled: bool,
}

impl HarnessAppBuilder {
    /// Create a new builder using the given test name. The name is sanitized
    /// and turned into a unique Postgres database for this run.
    pub fn new(test_name: impl Into<String>) -> Self {
        Self {
            test_name: test_name.into(),
            migrations_dir: None,
            jwt_secret: TEST_JWT_SECRET.to_string(),
            extra_internal_sql: Vec::new(),
            cors_enabled: false,
        }
    }

    /// Apply user migrations from the given directory after the system schema.
    /// When unset, the harness applies only the forge system migrations.
    pub fn migrations_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.migrations_dir = Some(dir.into());
        self
    }

    /// Override the JWT secret used to mint and verify tokens.
    pub fn jwt_secret(mut self, secret: impl Into<String>) -> Self {
        self.jwt_secret = secret.into();
        self
    }

    /// Append additional SQL to run after the forge system schema and before
    /// user migrations. Useful for inserting fixture rows that handlers expect.
    pub fn extra_sql(mut self, sql: impl Into<String>) -> Self {
        self.extra_internal_sql.push(sql.into());
        self
    }

    /// Enable permissive CORS on the gateway. Off by default — most harness
    /// tests don't go through a browser.
    pub fn cors(mut self, enabled: bool) -> Self {
        self.cors_enabled = enabled;
        self
    }

    /// Boot the harness app: provision the DB, run migrations, register
    /// every `#[forge::*]` handler available via inventory, wire up the
    /// gateway, worker, reactor, and bind on `127.0.0.1:0`.
    pub async fn start(self) -> Result<HarnessApp> {
        HarnessApp::start_with_builder(self).await
    }
}

/// A running, fully-wired Forge instance against a temporary Postgres.
///
/// Drop the app to shut every subsystem down and drop the isolated database.
pub struct HarnessApp {
    base_url: String,
    pool: PgPool,
    jwt_secret: String,
    token_issuer: Arc<dyn TokenIssuer>,
    http_client: reqwest::Client,
    shutdown: Arc<tokio::sync::Notify>,
    handles: Vec<JoinHandle<()>>,
    _db: forge_core::testing::IsolatedTestDb,
}

impl HarnessApp {
    /// Convenience: start with defaults (no user migrations, embedded JWT secret).
    pub async fn start(test_name: impl Into<String>) -> Result<Self> {
        HarnessAppBuilder::new(test_name).start().await
    }

    /// Start a new builder.
    pub fn builder(test_name: impl Into<String>) -> HarnessAppBuilder {
        HarnessAppBuilder::new(test_name)
    }

    async fn start_with_builder(builder: HarnessAppBuilder) -> Result<Self> {
        let internal_sql = forge::get_internal_sql();
        let migrations_dir = builder
            .migrations_dir
            .unwrap_or_else(|| PathBuf::from(".harness-no-user-migrations"));

        let db = forge_core::testing::IsolatedTestDb::setup(
            &builder.test_name,
            &internal_sql,
            &migrations_dir,
        )
        .await
        .map_err(HarnessError::Forge)?;

        for sql in &builder.extra_internal_sql {
            db.run_sql(sql).await.map_err(HarnessError::Forge)?;
        }

        let pool = db.pool().clone();
        let database = Database::from_pool(pool.clone());

        // Build the registries by walking inventory exactly like the real runtime.
        let mut registries = HandlerRegistries {
            functions: FunctionRegistry::new(),
            jobs: JobRegistry::new(),
            crons: CronRegistry::new(),
            workflows: WorkflowRegistry::new(),
            daemons: DaemonRegistry::new(),
            webhooks: WebhookRegistry::new(),
            mcp_tools: McpToolRegistry::new(),
        };
        forge::auto_register_all(&mut registries);

        // Workflow runs refuse to start unless the (name, version, signature) row
        // exists. Same upsert logic the production runtime runs at boot.
        registries
            .workflows
            .persist_definitions(&pool)
            .await
            .map_err(HarnessError::Forge)?;

        // Shared NOTIFY bus drives the reactor, the job worker wakeup, and
        // workflow scheduling. Same channels the real runtime opens.
        let notify_bus = Arc::new(PgNotifyBus::new(
            pool.clone(),
            &[
                "forge_changes",
                "forge_jobs_available",
                "forge_workflow_wakeup",
                forge_runtime::pg::LEADER_RELEASED_CHANNEL,
            ],
        ));

        let (bus_shutdown_tx, bus_shutdown_rx) = tokio::sync::watch::channel(false);
        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        let bus_for_task = notify_bus.clone();
        handles.push(tokio::spawn(async move {
            bus_for_task.run(bus_shutdown_rx).await;
        }));

        let job_queue = JobQueue::new(pool.clone());
        let job_dispatcher: Arc<dyn JobDispatch> = Arc::new(JobDispatcher::new(
            job_queue.clone(),
            registries.jobs.clone(),
        ));

        let http_circuit = forge_core::CircuitBreakerClient::with_ssrf_protection();
        let workflow_executor = Arc::new(WorkflowExecutor::new(
            Arc::new(registries.workflows.clone()),
            pool.clone(),
            job_queue.clone(),
            http_circuit,
        ));
        forge_runtime::workflow::register_workflow_bridge(
            workflow_executor.clone(),
            &mut registries.jobs,
        );
        let workflow_dispatcher: Arc<dyn WorkflowDispatch> = workflow_executor.clone();

        let auth_config = AuthConfig::with_secret(builder.jwt_secret.clone());
        let token_issuer: Arc<dyn TokenIssuer> =
            Arc::new(HmacTokenIssuer::from_config(&auth_config).ok_or_else(|| {
                HarnessError::setup(
                    "HmacTokenIssuer::from_config returned None; JWT secret missing or empty",
                )
            })?);

        let gateway_config = GatewayConfig {
            port: 0,
            auth: auth_config.clone(),
            cors_enabled: builder.cors_enabled,
            security_headers: false,
            request_timeout_secs: 30,
            ..GatewayConfig::default()
        };

        let gateway = GatewayServer::new(
            gateway_config.clone(),
            registries.functions.clone(),
            database.clone(),
            notify_bus.clone(),
        )
        .with_job_dispatcher(job_dispatcher.clone())
        .with_workflow_dispatcher(workflow_dispatcher.clone());

        let reactor = gateway.reactor();
        reactor
            .start()
            .await
            .map_err(|e| HarnessError::setup(format!("reactor start failed: {e}")))?;

        let api_router = gateway.router();
        let mut router = axum::Router::new().nest("/_api", api_router);

        if !registries.webhooks.is_empty() {
            use axum::routing::post;
            let webhook_state = Arc::new(
                WebhookState::new(Arc::new(registries.webhooks.clone()), pool.clone())
                    .with_job_dispatcher(job_dispatcher.clone())
                    .with_workflow_dispatcher(workflow_dispatcher.clone()),
            );
            let webhook_router = axum::Router::new()
                .route("/{*path}", post(webhook_handler).with_state(webhook_state));
            router = router.nest("/_api/webhooks", webhook_router);
        }

        let bind_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let listener = forge_runtime::gateway::bind_listener(bind_addr, None)
            .await
            .map_err(HarnessError::Io)?;
        let local_addr = {
            use axum::serve::Listener;
            listener.local_addr().map_err(HarnessError::Io)?
        };
        let base_url = format!("http://{}", local_addr);

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let server_shutdown = shutdown.clone();
        let service =
            router.into_make_service_with_connect_info::<forge_runtime::gateway::PeerAddr>();
        handles.push(tokio::spawn(async move {
            let server = axum::serve(listener, service);
            tokio::select! {
                result = server => {
                    if let Err(e) = result {
                        tracing::error!(error = %e, "harness gateway exited with error");
                    }
                }
                _ = server_shutdown.notified() => {}
            }
        }));

        // Worker: poll-interval is short so tests don't have to sleep 5s for
        // a queued job to fire. NOTIFY still drives the fast path.
        let worker_config = WorkerConfig {
            poll_interval: Duration::from_millis(50),
            stale_cleanup_interval: Duration::from_secs(5),
            shutdown_grace_period: Duration::from_secs(1),
            // One worker drains every queue. `$workflow_resume` and cron jobs
            // are tagged with the `workflows`/`cron` capabilities; without
            // serving those tags a dispatched workflow sits unclaimed forever.
            // Production runs one worker per queue purely for starvation
            // isolation, which a single test process does not need.
            capabilities: vec![
                forge_core::config::DEFAULT_QUEUE.to_string(),
                forge_core::config::WORKFLOWS_QUEUE.to_string(),
                forge_core::config::CRON_QUEUE.to_string(),
            ],
            ..WorkerConfig::default()
        };
        let mut worker = Worker::new(
            worker_config,
            job_queue.clone(),
            registries.jobs.clone(),
            pool.clone(),
            notify_bus.clone(),
        )
        .with_job_dispatch(job_dispatcher.clone())
        .with_workflow_dispatch(workflow_dispatcher.clone());

        let worker_shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                result = worker.run() => {
                    if let Err(e) = result {
                        tracing::error!(error = %e, "harness worker exited with error");
                    }
                }
                _ = worker_shutdown.notified() => {}
            }
        }));

        // Workflow scheduler: resumes durable sleeps and event-waits. Without
        // it, `ctx.sleep(...)` and `ctx.wait_for_event(...)` never wake up.
        let scheduler = WorkflowScheduler::new(
            pool.clone(),
            job_queue.clone(),
            Arc::new(EventStore::new(pool.clone())),
            WorkflowSchedulerConfig {
                poll_interval: Duration::from_millis(100),
                ..WorkflowSchedulerConfig::default()
            },
            notify_bus.clone(),
        );
        let scheduler_token = CancellationToken::new();
        let scheduler_token_child = scheduler_token.clone();
        handles.push(tokio::spawn(async move {
            scheduler.run(scheduler_token_child).await;
        }));
        let scheduler_shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            scheduler_shutdown.notified().await;
            scheduler_token.cancel();
        }));

        // Tell the NOTIFY bus to stop when the harness is torn down.
        let bus_stop_shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            bus_stop_shutdown.notified().await;
            let _ = bus_shutdown_tx.send(true);
        }));

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(HarnessError::Http)?;

        wait_for_health(&http_client, &base_url, Duration::from_secs(10)).await?;

        Ok(HarnessApp {
            base_url,
            pool,
            jwt_secret: builder.jwt_secret,
            token_issuer,
            http_client,
            shutdown,
            handles,
            _db: db,
        })
    }

    /// Base URL of the bound gateway, e.g. `http://127.0.0.1:54321`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Direct access to the Postgres pool the harness is using. Useful for
    /// asserting downstream effects in tests (rows inserted, counters
    /// incremented, jobs progressed, …).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// JWT secret currently configured on the gateway.
    pub fn jwt_secret(&self) -> &str {
        &self.jwt_secret
    }

    /// An unauthenticated HTTP client targeting the gateway.
    pub fn client(&self) -> HarnessClient {
        HarnessClient::new(self.http_client.clone(), self.base_url.clone(), None)
    }

    /// An HTTP client authenticated as the given user. The user id is passed
    /// as the JWT subject claim and may be any UUID — handlers see it via
    /// `ctx.user_id()`.
    pub fn client_as(&self, user_id: Uuid) -> Result<HarnessClient> {
        let token = self.issue_token(user_id, &[])?;
        Ok(HarnessClient::new(
            self.http_client.clone(),
            self.base_url.clone(),
            Some(token),
        ))
    }

    /// An HTTP client authenticated as the given user with the supplied roles.
    pub fn client_as_with_roles(&self, user_id: Uuid, roles: &[&str]) -> Result<HarnessClient> {
        let token = self.issue_token(user_id, roles)?;
        Ok(HarnessClient::new(
            self.http_client.clone(),
            self.base_url.clone(),
            Some(token),
        ))
    }

    /// Issue a JWT for the given user with the configured secret. The token
    /// is valid for one hour and carries the supplied roles.
    pub fn issue_token(&self, user_id: Uuid, roles: &[&str]) -> Result<String> {
        let mut builder = forge_core::Claims::builder()
            .user_id(user_id)
            .duration_secs(3600);
        for role in roles {
            builder = builder.role(*role);
        }
        let claims = builder
            .build()
            .map_err(|e| HarnessError::setup(format!("build claims: {e}")))?;
        self.token_issuer.sign(&claims).map_err(HarnessError::Forge)
    }

    /// Open a long-lived SSE session for the given token (or anonymous). The
    /// returned session lets you subscribe to functions and read updates as
    /// the reactor pushes them.
    pub async fn open_session(&self, token: Option<&str>) -> Result<HarnessSession> {
        sse::HarnessSession::open(
            self.http_client.clone(),
            self.base_url.clone(),
            token.map(|t| t.to_string()),
        )
        .await
    }

    /// Shut down the gateway, worker, reactor, and notify bus, then drop the
    /// isolated database. Called automatically on drop, but exposed so tests
    /// that need to surface shutdown errors can await it directly.
    pub async fn shutdown(mut self) -> Result<()> {
        self.signal_shutdown();
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
        Ok(())
    }

    fn signal_shutdown(&self) {
        self.shutdown.notify_waiters();
    }
}

impl Drop for HarnessApp {
    fn drop(&mut self) {
        self.signal_shutdown();
    }
}

async fn wait_for_health(
    client: &reqwest::Client,
    base_url: &str,
    timeout: Duration,
) -> Result<()> {
    let url = format!("{base_url}/_api/health");
    let deadline = std::time::Instant::now() + timeout;
    let mut last_error: Option<String> = None;
    while std::time::Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => {
                last_error = Some(format!("status={}", resp.status()));
            }
            Err(e) => {
                last_error = Some(e.to_string());
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(HarnessError::setup(format!(
        "gateway health probe never succeeded: {}",
        last_error.unwrap_or_else(|| "no response".to_string())
    )))
}

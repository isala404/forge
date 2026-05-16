use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json, Router,
    error_handling::HandleErrorLayer,
    extract::DefaultBodyLimit,
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::limit::ConcurrencyLimitLayer;
use tower::timeout::TimeoutLayer;
use tower_http::cors::{Any, CorsLayer};

use forge_core::cluster::NodeId;
use forge_core::config::McpConfig;
use forge_core::function::{JobDispatch, WorkflowDispatch};
#[cfg(feature = "otel")]
use opentelemetry::global;
#[cfg(feature = "otel")]
use opentelemetry::propagation::Extractor;
use tracing::Instrument;
#[cfg(feature = "otel")]
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::admin::{AdminState, admin_router};
use super::auth::{AuthConfig, AuthMiddleware, HmacTokenIssuer, auth_middleware};
use super::mcp::{McpState, mcp_get_handler, mcp_post_handler};
use super::multipart::{MultipartConfig, rpc_multipart_handler};
use super::response::{RpcError, RpcResponse};
use super::rpc::{RpcHandler, rpc_function_handler, rpc_handler};
use super::sse::{
    SseState, sse_handler, sse_job_subscribe_handler, sse_subscribe_handler,
    sse_unsubscribe_handler, sse_workflow_subscribe_handler,
};
use super::tls::{TlsListenConfig, bind_listener};
use super::tracing::{REQUEST_ID_HEADER, SPAN_ID_HEADER, TRACE_ID_HEADER, TracingState};
use crate::function::FunctionRegistry;
use crate::mcp::McpToolRegistry;
use crate::pg::Database;
use crate::realtime::{Reactor, ReactorConfig};

const DEFAULT_MAX_JSON_BODY_SIZE: usize = 1024 * 1024;
const DEFAULT_MAX_MULTIPART_BODY_SIZE: usize = 20 * 1024 * 1024;
const DEFAULT_MAX_FILE_SIZE: usize = 10 * 1024 * 1024;
const MAX_MULTIPART_CONCURRENCY: usize = 32;
/// Fallback for visitor ID hashing when no JWT secret is configured (dev only).
const DEFAULT_SIGNAL_SECRET: &str = "forge-default-signal-secret";

/// Gateway server configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Port to listen on.
    pub port: u16,
    /// Maximum number of connections.
    pub max_connections: usize,
    /// Maximum number of active SSE sessions.
    pub sse_max_sessions: usize,
    /// Request timeout in seconds.
    pub request_timeout_secs: u64,
    /// Enable CORS.
    pub cors_enabled: bool,
    /// Allowed CORS origins.
    pub cors_origins: Vec<String>,
    /// Authentication configuration.
    pub auth: AuthConfig,
    /// MCP configuration.
    pub mcp: McpConfig,
    /// Routes excluded from request logs, metrics, and traces.
    pub quiet_paths: Vec<String>,
    /// Token TTL configuration for refresh token management.
    pub token_ttl: forge_core::AuthTokenTtl,
    /// Project name (displayed on OAuth consent page).
    pub project_name: String,
    /// Maximum body size in bytes for uploads. Defaults to 20 MB.
    pub max_body_size_bytes: usize,
    /// Default per-file cap in bytes for multipart uploads. Applies when
    /// a mutation does not declare its own `max_size`. Defaults to 10 MB.
    pub max_file_size_bytes: usize,
    /// Optional TLS configuration. When `None`, the gateway serves plain HTTP.
    pub tls: Option<TlsListenConfig>,
    /// Maximum file fields in a single multipart upload.
    pub max_multipart_fields: usize,
    /// Maximum concurrent SSE sessions per authenticated user.
    pub max_sessions_per_user: usize,
    /// Maximum concurrent SSE sessions per source IP.
    pub max_sessions_per_ip: usize,
    /// Cap on a user's total subscriptions across every active session.
    pub max_subscriptions_per_user: usize,
    /// Reactor, invalidation, listener, and SSE knobs. Defaults match production.
    pub reactor_config: ReactorConfig,
    /// Add standard security headers to all responses.
    pub security_headers: bool,
    /// Enable HTTP Strict Transport Security header.
    pub hsts: bool,
    /// Parsed trusted proxy CIDR ranges for IP extraction.
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// Maximum number of background jobs a single mutation request may dispatch.
    /// 0 disables the limit. Defaults to 10.
    pub max_jobs_per_request: usize,
    /// Maximum serialized response size in bytes. Defaults to 10 MiB.
    pub max_result_size_bytes: usize,
    /// Maximum JSON nesting depth for incoming request bodies. Defaults to 64.
    pub max_json_depth: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: 9081,
            max_connections: 512,
            sse_max_sessions: 10_000,
            request_timeout_secs: 30,
            cors_enabled: false,
            cors_origins: Vec::new(),
            auth: AuthConfig::default(),
            mcp: McpConfig::default(),
            quiet_paths: Vec::new(),
            token_ttl: forge_core::AuthTokenTtl::default(),
            project_name: "forge-app".to_string(),
            max_body_size_bytes: DEFAULT_MAX_MULTIPART_BODY_SIZE,
            max_file_size_bytes: DEFAULT_MAX_FILE_SIZE,
            tls: None,
            max_multipart_fields: 20,
            max_sessions_per_user: 8,
            max_sessions_per_ip: 32,
            max_subscriptions_per_user: 500,
            reactor_config: ReactorConfig::default(),
            security_headers: true,
            hsts: false,
            trusted_proxies: Vec::new(),
            max_jobs_per_request: 10,
            max_result_size_bytes: 10 * 1024 * 1024,
            max_json_depth: 64,
        }
    }
}

/// Parsed trusted proxy networks, shared across middleware and handlers.
#[derive(Debug, Clone)]
pub struct TrustedProxies(pub Arc<Vec<ipnet::IpNet>>);

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Public readiness probe payload.
///
/// Intentionally minimal: load-balancer probes can call this without
/// authentication and we don't want to leak internal deployment signals (queue
/// depths, blocked-run counts, version skew) to anonymous callers. The
/// Readiness probe response. Covers DB connectivity and reactor health.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct ReadinessResponse {
    pub ready: bool,
    pub database: bool,
    pub reactor: bool,
    /// NOTIFY queue usage below the failure threshold (75%).
    pub notify_queue_ok: bool,
    /// All embedded system migrations applied to the database.
    pub migrations_ok: bool,
    /// This node has an `active` row in `forge_nodes`.
    /// `None` when cluster registration is not enabled for this process.
    pub cluster_registered: Option<bool>,
    pub version: String,
}

/// State for readiness check.
#[derive(Clone)]
pub struct ReadinessState {
    db_pool: sqlx::PgPool,
    reactor: Arc<Reactor>,
    /// Local node id when cluster registration is enabled.
    node_id: Option<uuid::Uuid>,
    /// Count of embedded system migrations the process expects to be applied.
    expected_system_migrations: i64,
}

/// pg_notification_queue_usage() returns a fraction in [0, 1].
/// Above this fraction the probe reports the cluster as not ready so the
/// load balancer drains traffic before the queue fills and NOTIFY starts
/// dropping rows. Matches Postgres' own warning threshold.
const NOTIFY_QUEUE_FAIL_THRESHOLD: f64 = 0.75;

/// Gateway HTTP server.
pub struct GatewayServer {
    config: GatewayConfig,
    registry: FunctionRegistry,
    db: Database,
    reactor: Arc<Reactor>,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
    workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    mcp_registry: Option<McpToolRegistry>,
    token_ttl: forge_core::AuthTokenTtl,
    signals_collector: Option<crate::signals::SignalsCollector>,
    signals_anonymize_ip: bool,
    signals_geoip: Option<crate::signals::geoip::GeoIpResolver>,
    custom_routes: Option<Router>,
    rate_limiter: Option<Arc<dyn forge_core::rate_limit::RateLimiterBackend>>,
    role_resolver: Option<forge_core::SharedRoleResolver>,
    cluster_node_id: Option<uuid::Uuid>,
}

impl GatewayServer {
    /// Create a new gateway server.
    pub fn new(config: GatewayConfig, registry: FunctionRegistry, db: Database) -> Self {
        let node_id = NodeId::new();
        let reactor = Arc::new(Reactor::new(
            node_id,
            Arc::new(db.clone()),
            registry.clone(),
            config.reactor_config.clone(),
        ));

        let token_ttl = config.token_ttl.clone();
        Self {
            config,
            registry,
            db,
            reactor,
            job_dispatcher: None,
            workflow_dispatcher: None,
            mcp_registry: None,
            token_ttl,
            signals_collector: None,
            signals_anonymize_ip: false,
            signals_geoip: None,
            custom_routes: None,
            rate_limiter: None,
            role_resolver: None,
            cluster_node_id: None,
        }
    }

    /// Record this process's cluster node id so the readiness probe can
    /// verify the node's row in `forge_nodes` is still `active`.
    pub fn with_node_id(mut self, id: NodeId) -> Self {
        self.cluster_node_id = Some(id.as_uuid());
        self
    }

    /// Override the default rate limiter backend.
    pub fn with_rate_limiter(
        mut self,
        rate_limiter: Arc<dyn forge_core::rate_limit::RateLimiterBackend>,
    ) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    /// Set a custom role resolver for RBAC extension.
    ///
    /// See [`forge_core::RoleResolver`] for the trait contract.
    pub fn with_role_resolver(mut self, resolver: forge_core::SharedRoleResolver) -> Self {
        self.role_resolver = Some(resolver);
        self
    }

    /// Set the job dispatcher.
    pub fn with_job_dispatcher(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        self.job_dispatcher = Some(dispatcher);
        self
    }

    /// Set the workflow dispatcher.
    pub fn with_workflow_dispatcher(mut self, dispatcher: Arc<dyn WorkflowDispatch>) -> Self {
        self.workflow_dispatcher = Some(dispatcher);
        self
    }

    /// Set the MCP tool registry.
    pub fn with_mcp_registry(mut self, registry: McpToolRegistry) -> Self {
        self.mcp_registry = Some(registry);
        self
    }

    /// Set the signals collector for auto-capturing RPC events and
    /// registering client signal ingestion endpoints.
    ///
    /// Also installs the collector into the process-wide emit module so
    /// background executions (jobs, crons, workflows, daemons, webhooks,
    /// auth failures) can emit signals without threading through plumbing.
    pub fn with_signals_collector(mut self, collector: crate::signals::SignalsCollector) -> Self {
        crate::signals::install_global(Some(collector.clone()));
        self.signals_collector = Some(collector);
        self
    }

    /// Enable IP anonymization for signal events.
    /// When true, raw client IPs are not stored in event records.
    pub fn with_signals_anonymize_ip(mut self, anonymize: bool) -> Self {
        self.signals_anonymize_ip = anonymize;
        self
    }

    /// Set the GeoIP resolver for country code lookups from client IPs.
    pub fn with_signals_geoip(mut self, resolver: crate::signals::geoip::GeoIpResolver) -> Self {
        self.signals_geoip = Some(resolver);
        self
    }

    /// Set additional routes that receive the full middleware stack
    /// (auth, CORS, tracing, concurrency limits, timeouts).
    pub fn with_custom_routes(mut self, router: Router) -> Self {
        self.custom_routes = Some(router);
        self
    }

    /// Get a reference to the reactor.
    pub fn reactor(&self) -> Arc<Reactor> {
        self.reactor.clone()
    }

    /// Get the TLS configuration, if any.
    pub fn tls(&self) -> Option<&TlsListenConfig> {
        self.config.tls.as_ref()
    }

    /// Build an OAuth router (bypasses auth middleware). Returns None if OAuth is disabled.
    ///
    /// Only available when the `mcp-oauth` feature is enabled.
    #[cfg(feature = "mcp-oauth")]
    pub fn oauth_router(&self) -> Option<(Router, Arc<super::oauth::OAuthState>)> {
        if !self.config.mcp.oauth {
            return None;
        }

        let token_issuer = HmacTokenIssuer::from_config(&self.config.auth)
            .map(|issuer| Arc::new(issuer) as Arc<dyn forge_core::TokenIssuer>)?;

        let auth_middleware_state = Arc::new(AuthMiddleware::new(self.config.auth.clone()));

        let jwt_secret = self.config.auth.jwt_secret.clone().unwrap_or_default();

        let oauth_state = Arc::new(super::oauth::OAuthState::new(
            self.db.primary().clone(),
            auth_middleware_state,
            token_issuer,
            self.token_ttl.access_token_secs,
            self.token_ttl.refresh_token_days,
            self.config.auth.is_hmac(),
            self.config.project_name.clone(),
            jwt_secret,
            self.config.auth.session_cookie_ttl_secs,
            self.config.mcp.allow_unauthenticated_dcr,
        ));

        let router = Router::new()
            .route(
                "/oauth/authorize",
                get(super::oauth::oauth_authorize_get).post(super::oauth::oauth_authorize_post),
            )
            .route("/oauth/token", post(super::oauth::oauth_token))
            .route("/oauth/register", post(super::oauth::oauth_register))
            .with_state(oauth_state.clone());

        Some((router, oauth_state))
    }

    /// Stub when `mcp-oauth` is not enabled — always returns `None`.
    #[cfg(not(feature = "mcp-oauth"))]
    pub fn oauth_router(&self) -> Option<(Router, ())> {
        None
    }

    /// Build the Axum router.
    pub fn router(&self) -> Router {
        let token_issuer = HmacTokenIssuer::from_config(&self.config.auth)
            .map(|issuer| Arc::new(issuer) as Arc<dyn forge_core::TokenIssuer>);

        let mut rpc = RpcHandler::with_dispatch_and_issuer(
            self.registry.clone(),
            self.db.clone(),
            self.job_dispatcher.clone(),
            self.workflow_dispatcher.clone(),
            token_issuer,
        );
        rpc.set_token_ttl(self.token_ttl.clone());
        rpc.set_max_jobs_per_request(self.config.max_jobs_per_request);
        rpc.set_max_result_size_bytes(self.config.max_result_size_bytes);
        if let Some(rate_limiter) = &self.rate_limiter {
            rpc.set_rate_limiter(rate_limiter.clone());
        }
        if let Some(resolver) = &self.role_resolver {
            rpc.set_role_resolver(resolver.clone());
        }
        if let Some(collector) = &self.signals_collector {
            let secret = self.config.auth.jwt_secret.clone().unwrap_or_else(|| {
                tracing::warn!(
                    "No jwt_secret configured; using default signal secret for visitor ID hashing. \
                         Visitor IDs will be predictable. Set [auth] jwt_secret in forge.toml."
                );
                DEFAULT_SIGNAL_SECRET.to_string()
            });
            rpc.set_signals_collector(collector.clone(), secret);
        }
        let rpc_handler_state = Arc::new(rpc);

        // Cluster cache invalidation: peer-node mutations broadcast over PG
        // NOTIFY, and each node evicts its local query cache for the changed
        // tables/columns. Without this, every node would serve stale data
        // until the per-entry TTL expired. Dropping the JoinHandle detaches
        // the task; it exits when the broadcast channel closes during shutdown.
        let cluster_cache = rpc_handler_state.router().cache();
        drop(cluster_cache.spawn_cluster_invalidator(self.reactor.change_subscriber()));

        let auth_middleware_state = Arc::new(AuthMiddleware::new(self.config.auth.clone()));

        // Build CORS layer. When specific origins are configured, allow
        // credentials so the browser accepts cross-origin API responses
        // (the forge-svelte client sends `credentials: "include"` for
        // the SSE session cookie). Wildcard methods/headers are incompatible
        // with credentials per the CORS spec, so we enumerate them.
        let cors = if self.config.cors_enabled {
            if self.config.cors_origins.iter().any(|o| o == "*") {
                // Wildcard origin can't use credentials. Loud at startup so
                // operators don't ship `cors_origins = ["*"]` to production
                // by accident — credentialed requests will silently fail
                // (no `Access-Control-Allow-Credentials`) and there's no
                // origin allowlist limiting cross-site abuse of the gateway.
                tracing::warn!(
                    "CORS wildcard (`cors_origins = [\"*\"]`) is enabled. \
                     Credentialed requests will fail and any origin can \
                     reach the gateway. Set explicit origins for \
                     production deployments."
                );
                CorsLayer::new()
                    .allow_origin(Any)
                    .allow_methods(Any)
                    .allow_headers(Any)
            } else {
                use axum::http::Method;
                let origins: Vec<_> = self
                    .config
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
                        axum::http::HeaderName::from_static("x-correlation-id"),
                        axum::http::HeaderName::from_static("x-session-id"),
                        axum::http::HeaderName::from_static("x-forge-platform"),
                    ])
                    .allow_credentials(true)
                    // 24-hour preflight cache: browsers won't fire OPTIONS for
                    // repeat cross-origin requests within this window.
                    .max_age(Duration::from_secs(86400))
            }
        } else {
            CorsLayer::new()
        };

        // SSE state for Server-Sent Events
        let sse_state = Arc::new(SseState::with_config(
            self.reactor.clone(),
            auth_middleware_state.clone(),
            super::sse::SseConfig {
                max_sessions: self.config.sse_max_sessions,
                max_subscriptions_per_session: self
                    .config
                    .reactor_config
                    .realtime
                    .max_subscriptions_per_session,
                max_sessions_per_user: self.config.max_sessions_per_user,
                max_sessions_per_ip: self.config.max_sessions_per_ip,
                max_subscriptions_per_user: self.config.max_subscriptions_per_user,
                ..Default::default()
            },
        ));

        // Readiness state for DB + reactor health check
        let expected_system_migrations = crate::pg::migration::get_system_migrations().len() as i64;
        let readiness_state = Arc::new(ReadinessState {
            db_pool: self.db.primary().clone(),
            reactor: self.reactor.clone(),
            node_id: self.cluster_node_id,
            expected_system_migrations,
        });

        // Build the main router with middleware
        let max_json_depth = self.config.max_json_depth;
        let mut main_router = Router::new()
            // Health check endpoint (liveness)
            .route("/health", get(health_handler))
            // Readiness check endpoint (checks DB)
            .route("/ready", get(readiness_handler).with_state(readiness_state))
            // RPC endpoint
            .route("/rpc", post(rpc_handler))
            // REST-style function endpoint (JSON)
            .route("/rpc/{function}", post(rpc_function_handler))
            // Prevent oversized JSON payloads from exhausting memory.
            .layer(DefaultBodyLimit::max(DEFAULT_MAX_JSON_BODY_SIZE))
            // Reject JSON bodies that exceed the nesting depth limit to prevent
            // stack exhaustion during recursive deserialization.
            .layer(middleware::from_fn_with_state(
                max_json_depth,
                json_depth_check_middleware,
            ))
            // Add state
            .with_state(rpc_handler_state.clone());

        // Multipart RPC router. The Axum layer limit is set to the highest
        // configured size (global or any per-mutation override) so that
        // per-mutation max_size values aren't rejected at the HTTP layer.
        // The handler still enforces per-function limits chunk-by-chunk.
        let max_per_mutation = self
            .registry
            .functions()
            .filter_map(|(_, entry)| entry.info().max_upload_size_bytes)
            .max()
            .unwrap_or(0);
        let layer_limit = self.config.max_body_size_bytes.max(max_per_mutation);
        let mp_config = MultipartConfig {
            max_body_size_bytes: self.config.max_body_size_bytes,
            max_file_size_bytes: self.config.max_file_size_bytes,
            max_upload_fields: self.config.max_multipart_fields,
        };
        let multipart_router = Router::new()
            .route("/rpc/{function}/upload", post(rpc_multipart_handler))
            .layer(DefaultBodyLimit::max(layer_limit))
            .layer(Extension(mp_config))
            // Cap upload fan-out; each request buffers data in memory.
            .layer(ConcurrencyLimitLayer::new(MAX_MULTIPART_CONCURRENCY))
            .with_state(rpc_handler_state.clone());

        // SSE router
        let sse_router = Router::new()
            .route("/events", get(sse_handler))
            .route("/subscribe", post(sse_subscribe_handler))
            .route("/unsubscribe", post(sse_unsubscribe_handler))
            .route("/subscribe-job", post(sse_job_subscribe_handler))
            .route("/subscribe-workflow", post(sse_workflow_subscribe_handler))
            .with_state(sse_state);

        let mut mcp_router = Router::new();
        if self.config.mcp.enabled {
            let path = self.config.mcp.path.clone();
            let mcp_state = Arc::new(McpState::new(
                self.config.mcp.clone(),
                self.mcp_registry.clone().unwrap_or_default(),
                self.db.primary().clone(),
                self.job_dispatcher.clone(),
                self.workflow_dispatcher.clone(),
                Some(rpc_handler_state.router()),
            ));
            mcp_router = mcp_router.route(
                &path,
                post(mcp_post_handler)
                    .get(mcp_get_handler)
                    .with_state(mcp_state),
            );
        }

        // Signal ingestion endpoints (product analytics + diagnostics)
        let mut signals_router = Router::new();
        if let Some(collector) = &self.signals_collector {
            let signals_state = Arc::new(crate::signals::endpoints::SignalsState {
                collector: collector.clone(),
                pool: self.db.primary().clone(),
                server_secret: self
                    .config
                    .auth
                    .jwt_secret
                    .clone()
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            "No jwt_secret configured; using default signal secret for visitor ID hashing. \
                             Visitor IDs will be predictable. Set [auth] jwt_secret in forge.toml."
                        );
                        DEFAULT_SIGNAL_SECRET.to_string()
                    }),
                anonymize_ip: self.signals_anonymize_ip,
                geoip: self.signals_geoip.clone(),
                rate_limiter: Arc::new(crate::signals::rate_limit::SignalRateLimiter::new()),
            });
            signals_router = Router::new()
                .route("/signal", post(crate::signals::endpoints::signal_handler))
                .with_state(signals_state);
        }

        let admin_router = admin_router(AdminState {
            db_pool: self.db.primary().clone(),
        });

        main_router = main_router
            .merge(multipart_router)
            .merge(sse_router)
            .merge(mcp_router)
            .merge(signals_router)
            .merge(admin_router);

        if let Some(custom) = &self.custom_routes {
            main_router = main_router.merge(custom.clone());
        }

        // Security headers config
        let security_config = Arc::new(SecurityHeadersConfig {
            enabled: self.config.security_headers,
            hsts: self.config.hsts,
        });

        // Trusted proxies for client IP resolution
        let trusted_proxies = TrustedProxies(Arc::new(self.config.trusted_proxies.clone()));

        // Build middleware stack
        let service_builder = ServiceBuilder::new()
            .layer(HandleErrorLayer::new(handle_middleware_error))
            .layer(ConcurrencyLimitLayer::new(self.config.max_connections))
            .layer(TimeoutLayer::new(Duration::from_secs(
                self.config.request_timeout_secs,
            )))
            .layer(cors.clone())
            .layer(middleware::from_fn_with_state(
                security_config,
                security_headers_middleware,
            ))
            .layer(middleware::from_fn(api_version_middleware))
            .layer(middleware::from_fn_with_state(
                trusted_proxies,
                resolve_client_ip_middleware,
            ))
            .layer(middleware::from_fn_with_state(
                auth_middleware_state,
                auth_middleware,
            ))
            .layer(middleware::from_fn_with_state(
                Arc::new(self.config.quiet_paths.clone()),
                tracing_middleware,
            ));

        // Apply the remaining middleware layers
        main_router.layer(service_builder)
    }

    /// Get the socket address to bind to.
    pub fn addr(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from(([0, 0, 0, 0], self.config.port))
    }

    /// Run the server (blocking).
    pub async fn run(self) -> Result<(), std::io::Error> {
        let addr = self.addr();
        let tls = self.config.tls.clone();
        let service = self
            .router()
            .into_make_service_with_connect_info::<super::PeerAddr>();

        self.reactor
            .start()
            .await
            .map_err(|e| std::io::Error::other(format!("Failed to start reactor: {}", e)))?;
        tracing::info!("Reactor started for real-time updates");

        tracing::info!("Gateway server listening on {}", addr);

        let listener = bind_listener(addr, tls.as_ref()).await?;
        axum::serve(listener, service).await
    }
}

/// Health check handler (liveness probe).
async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Readiness check handler (readiness probe).
///
/// Reports green only when:
/// - The primary pool can serve `SELECT 1`.
/// - The reactor's change listener is connected.
/// - `pg_notification_queue_usage()` is below 75% (NOTIFY won't start
///   dropping events when reactivity bursts).
/// - Every embedded system migration has a corresponding row in
///   `forge_system_migrations` (no schema drift between binary and DB).
/// - When cluster registration is enabled for this process, this node's
///   row in `forge_nodes` exists with `status = 'active'`.
async fn readiness_handler(
    axum::extract::State(state): axum::extract::State<Arc<ReadinessState>>,
) -> (axum::http::StatusCode, Json<ReadinessResponse>) {
    let db_ok = sqlx::query_scalar!("SELECT 1 as \"v!\"")
        .fetch_one(&state.db_pool)
        .await
        .is_ok();

    let reactor_stats = state.reactor.stats().await;
    let reactor_ok = reactor_stats.listener_running;

    let notify_queue_ok = if db_ok {
        match sqlx::query_scalar!("SELECT pg_notification_queue_usage() AS \"usage!\"")
            .fetch_one(&state.db_pool)
            .await
        {
            Ok(usage) => usage < NOTIFY_QUEUE_FAIL_THRESHOLD,
            Err(err) => {
                tracing::warn!(error = %err, "pg_notification_queue_usage() failed; failing readiness probe");
                false
            }
        }
    } else {
        false
    };

    let migrations_ok = if db_ok {
        match sqlx::query_scalar!(
            "SELECT COUNT(*) AS \"count!\" FROM forge_system_migrations WHERE version LIKE '__forge_v%'"
        )
        .fetch_one(&state.db_pool)
        .await
        {
            Ok(applied) => applied >= state.expected_system_migrations,
            Err(err) => {
                tracing::warn!(error = %err, "forge_system_migrations count failed; failing readiness probe");
                false
            }
        }
    } else {
        false
    };

    let cluster_registered = match state.node_id {
        Some(node_id) if db_ok => match sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM forge_nodes
                   WHERE id = $1 AND status = 'active'
               ) AS "found!""#,
            node_id
        )
        .fetch_one(&state.db_pool)
        .await
        {
            Ok(found) => Some(found),
            Err(err) => {
                tracing::warn!(error = %err, "forge_nodes lookup failed; failing readiness probe");
                Some(false)
            }
        },
        Some(_) => Some(false),
        None => None,
    };

    let ready = db_ok
        && reactor_ok
        && notify_queue_ok
        && migrations_ok
        && cluster_registered.unwrap_or(true);

    let status_code = if ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status_code,
        Json(ReadinessResponse {
            ready,
            database: db_ok,
            reactor: reactor_ok,
            notify_queue_ok,
            migrations_ok,
            cluster_registered,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
    )
}

async fn handle_middleware_error(err: BoxError) -> axum::response::Response {
    let rpc_err = if err.is::<tower::timeout::error::Elapsed>() {
        RpcError::new("REQUEST_TIMEOUT", "Request timed out")
    } else {
        RpcError::new("SERVICE_UNAVAILABLE", "Server overloaded")
    };
    RpcResponse::error(rpc_err).into_response()
}

fn set_tracing_headers(response: &mut axum::response::Response, trace_id: &str, request_id: &str) {
    if let Ok(val) = trace_id.parse() {
        response.headers_mut().insert(TRACE_ID_HEADER, val);
    }
    if let Ok(val) = request_id.parse() {
        response.headers_mut().insert(REQUEST_ID_HEADER, val);
    }
}

/// Extracts W3C traceparent context from HTTP headers.
#[cfg(feature = "otel")]
struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

#[cfg(feature = "otel")]
impl<'a> Extractor for HeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Resolve the real client IP using trusted proxy configuration and inject
/// it as `Extension<ResolvedClientIp>` for downstream handlers.
async fn resolve_client_ip_middleware(
    axum::extract::State(trusted): axum::extract::State<TrustedProxies>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let peer_ip = req
        .extensions()
        .get::<axum::extract::connect_info::ConnectInfo<super::PeerAddr>>()
        .map(|ci| ci.0.ip());
    let ip = super::resolve_client_ip(req.headers(), peer_ip, &trusted.0);
    req.extensions_mut().insert(super::ResolvedClientIp(ip));
    next.run(req).await
}

#[derive(Debug, Clone)]
struct SecurityHeadersConfig {
    enabled: bool,
    hsts: bool,
}

async fn security_headers_middleware(
    axum::extract::State(config): axum::extract::State<Arc<SecurityHeadersConfig>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(req).await;
    if config.enabled {
        let headers = response.headers_mut();
        headers.insert(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            axum::http::HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            axum::http::header::X_FRAME_OPTIONS,
            axum::http::HeaderValue::from_static("DENY"),
        );
        headers.insert(
            axum::http::header::REFERRER_POLICY,
            axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
        );
        headers.insert(
            axum::http::HeaderName::from_static("permissions-policy"),
            axum::http::HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
        );
        // Forge `/_api/*` only ever returns JSON, SSE, or a small handful of
        // static error/health pages — there is no HTML, script, image, or
        // remote fetch surface. A `default-src 'none'` policy means any byte
        // mistakenly executed as a document or script is blocked by the
        // browser. `frame-ancestors 'none'` matches `X-Frame-Options: DENY`
        // for legacy clients that ignore CSP.
        headers.insert(
            axum::http::header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        );
        if config.hsts {
            headers.insert(
                axum::http::header::STRICT_TRANSPORT_SECURITY,
                axum::http::HeaderValue::from_static("max-age=63072000; includeSubDomains"),
            );
        }
    }
    response
}

/// The only wire version currently supported.
const FORGE_API_V1: &str = "application/vnd.forge.v1+json";

/// Validates the `Accept` header for RPC routes.
///
/// Clients should send `Accept: application/vnd.forge.v1+json`. Omitting the
/// header is accepted (defaults to v1). Any other value returns 406 so that
/// future versions can be introduced without ambiguity.
async fn api_version_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let is_rpc = req.uri().path().starts_with("/rpc");
    if is_rpc && let Some(accept) = req.headers().get(axum::http::header::ACCEPT) {
        let accept_str = accept.to_str().unwrap_or("");
        // Allow wildcard and explicit v1; reject anything else.
        if accept_str != "*/*" && !accept_str.is_empty() && !accept_str.contains(FORGE_API_V1) {
            return RpcResponse::error(RpcError::new(
                "UNSUPPORTED_API_VERSION",
                format!(
                    "Unsupported Accept header '{}'. Use '{}' or omit the header.",
                    accept_str, FORGE_API_V1
                ),
            ))
            .into_response();
        }
    }
    next.run(req).await
}

/// Wraps each request in a span with HTTP semantics and OpenTelemetry
/// context propagation. Incoming `traceparent` headers are extracted so
/// that spans join the caller's distributed trace.
/// Quiet routes skip spans, logs, and metrics to avoid noise from
/// probes or high-frequency internal endpoints.
async fn tracing_middleware(
    axum::extract::State(quiet_paths): axum::extract::State<Arc<Vec<String>>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let headers = req.headers();

    // Extract W3C traceparent from incoming headers for distributed tracing.
    // Without `otel`, there's no propagator and no parent context to extract.
    #[cfg(feature = "otel")]
    let parent_cx =
        global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(headers)));

    let trace_id = headers
        .get(TRACE_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let parent_span_id = headers
        .get(SPAN_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let method = req.method().to_string();
    let path = req.uri().path().to_string();

    let mut tracing_state = TracingState::with_trace_id(trace_id.clone());
    if let Some(span_id) = parent_span_id {
        tracing_state = tracing_state.with_parent_span(span_id);
    }

    let mut req = req;
    req.extensions_mut().insert(tracing_state.clone());

    if req
        .extensions()
        .get::<forge_core::function::AuthContext>()
        .is_none()
    {
        req.extensions_mut()
            .insert(forge_core::function::AuthContext::unauthenticated());
    }

    // Config uses full paths (/_api/health) but axum strips the prefix
    // for nested routers, so the middleware sees /health not /_api/health.
    let full_path = format!("/_api{}", path);
    let is_quiet = quiet_paths.iter().any(|r| *r == full_path || *r == path);

    if is_quiet {
        let mut response = next.run(req).await;
        set_tracing_headers(&mut response, &trace_id, &tracing_state.request_id);
        return response;
    }

    let span = tracing::info_span!(
        "http.request",
        http.method = %method,
        http.route = %path,
        http.status_code = tracing::field::Empty,
        trace_id = %trace_id,
        request_id = %tracing_state.request_id,
    );

    // Link this span to the incoming distributed trace context so
    // fn.execute and all downstream spans share the caller's trace ID.
    #[cfg(feature = "otel")]
    span.set_parent(parent_cx);

    let mut response = next.run(req).instrument(span.clone()).await;

    let status = response.status().as_u16();
    let elapsed = tracing_state.elapsed();

    span.record("http.status_code", status);
    let duration_ms = elapsed.as_millis() as u64;
    match status {
        500..=599 => tracing::error!(parent: &span, duration_ms, "Request failed"),
        400..=499 => tracing::warn!(parent: &span, duration_ms, "Request rejected"),
        200..=299 => tracing::info!(parent: &span, duration_ms, "Request completed"),
        _ => tracing::trace!(parent: &span, duration_ms, "Request completed"),
    }
    crate::observability::record_http_request(&method, &path, status, elapsed.as_secs_f64());

    set_tracing_headers(&mut response, &trace_id, &tracing_state.request_id);
    response
}

/// Count the maximum JSON nesting depth in a byte slice by scanning for `{`
/// and `[` delimiters. String contents are skipped so quoted brackets don't
/// inflate the count. This is a conservative O(n) pre-parse guard, not a full
/// parser — its purpose is to catch stack-busting inputs before `serde_json`
/// recurses into them.
fn json_max_depth(bytes: &[u8]) -> usize {
    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    for &b in bytes {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max_depth {
                    max_depth = depth;
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    max_depth
}

/// Middleware that rejects request bodies whose JSON nesting depth exceeds
/// `max_depth`. Only inspects bodies for POST requests with a JSON
/// `Content-Type`; all other requests pass through unchanged.
///
/// The body is buffered, inspected, and re-inserted into the request so that
/// downstream handlers see the original bytes.
async fn json_depth_check_middleware(
    axum::extract::State(max_depth): axum::extract::State<usize>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::body::Body;

    // Only check POST requests with a JSON content type.
    let is_json_post = req.method() == axum::http::Method::POST
        && req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("application/json"))
            .unwrap_or(false);

    if !is_json_post || max_depth == 0 {
        return next.run(req).await;
    }

    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => {
            return super::response::RpcResponse::error(super::response::RpcError::new(
                "BAD_REQUEST",
                "Failed to read request body",
            ))
            .into_response();
        }
    };

    let depth = json_max_depth(&bytes);
    if depth > max_depth {
        return super::response::RpcResponse::error(super::response::RpcError::new(
            "BAD_REQUEST",
            format!(
                "JSON nesting depth {} exceeds the maximum of {}",
                depth, max_depth
            ),
        ))
        .into_response();
    }

    // Re-assemble the request with the buffered body.
    let req = axum::extract::Request::from_parts(parts, Body::from(bytes));
    next.run(req).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_config_default() {
        let config = GatewayConfig::default();
        assert_eq!(config.port, 9081);
        assert_eq!(config.max_connections, 512);
        assert!(!config.cors_enabled);
    }

    #[test]
    fn test_health_response_serialization() {
        let resp = HealthResponse {
            status: "healthy".to_string(),
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("healthy"));
    }
}

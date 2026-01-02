use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    middleware,
    routing::{any, get, post},
    Json, Router,
};
use serde::Serialize;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};

use forge_core::cluster::NodeId;
use forge_core::function::{JobDispatch, WorkflowDispatch};

use super::auth::{auth_middleware, AuthConfig, AuthMiddleware};
use super::metrics::{metrics_middleware, MetricsState};
use super::rpc::{rpc_function_handler, rpc_handler, RpcHandler};
use super::tracing::TracingState;
use super::websocket::{ws_handler, WsState};
use crate::function::FunctionRegistry;
use crate::observability::ObservabilityState;
use crate::realtime::{Reactor, ReactorConfig};

/// Gateway server configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Port to listen on.
    pub port: u16,
    /// Maximum number of connections.
    pub max_connections: usize,
    /// Request timeout in seconds.
    pub request_timeout_secs: u64,
    /// Enable CORS.
    pub cors_enabled: bool,
    /// Allowed CORS origins.
    pub cors_origins: Vec<String>,
    /// Authentication configuration.
    pub auth: AuthConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            max_connections: 10000,
            request_timeout_secs: 30,
            cors_enabled: true,
            cors_origins: vec!["*".to_string()],
            auth: AuthConfig::default(),
        }
    }
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Readiness check response.
#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    pub ready: bool,
    pub database: bool,
    pub version: String,
}

/// State for readiness check.
#[derive(Clone)]
pub struct ReadinessState {
    db_pool: sqlx::PgPool,
}

/// Gateway HTTP server.
pub struct GatewayServer {
    config: GatewayConfig,
    registry: FunctionRegistry,
    db_pool: sqlx::PgPool,
    reactor: Arc<Reactor>,
    observability: Option<ObservabilityState>,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
    workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
}

impl GatewayServer {
    /// Create a new gateway server.
    pub fn new(config: GatewayConfig, registry: FunctionRegistry, db_pool: sqlx::PgPool) -> Self {
        let node_id = NodeId::new();
        let reactor = Arc::new(Reactor::new(
            node_id,
            db_pool.clone(),
            registry.clone(),
            ReactorConfig::default(),
        ));

        Self {
            config,
            registry,
            db_pool,
            reactor,
            observability: None,
            job_dispatcher: None,
            workflow_dispatcher: None,
        }
    }

    /// Create a new gateway server with observability.
    pub fn with_observability(
        config: GatewayConfig,
        registry: FunctionRegistry,
        db_pool: sqlx::PgPool,
        observability: ObservabilityState,
    ) -> Self {
        let node_id = NodeId::new();
        let reactor = Arc::new(Reactor::new(
            node_id,
            db_pool.clone(),
            registry.clone(),
            ReactorConfig::default(),
        ));

        Self {
            config,
            registry,
            db_pool,
            reactor,
            observability: Some(observability),
            job_dispatcher: None,
            workflow_dispatcher: None,
        }
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

    /// Get a reference to the reactor.
    pub fn reactor(&self) -> Arc<Reactor> {
        self.reactor.clone()
    }

    /// Build the Axum router.
    pub fn router(&self) -> Router {
        let rpc_handler_state = Arc::new(RpcHandler::with_dispatch(
            self.registry.clone(),
            self.db_pool.clone(),
            self.job_dispatcher.clone(),
            self.workflow_dispatcher.clone(),
        ));

        let auth_middleware_state = Arc::new(AuthMiddleware::new(self.config.auth.clone()));

        // Build CORS layer
        let cors = if self.config.cors_enabled {
            if self.config.cors_origins.contains(&"*".to_string()) {
                CorsLayer::new()
                    .allow_origin(Any)
                    .allow_methods(Any)
                    .allow_headers(Any)
            } else {
                let origins: Vec<_> = self
                    .config
                    .cors_origins
                    .iter()
                    .filter_map(|o| o.parse().ok())
                    .collect();
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods(Any)
                    .allow_headers(Any)
            }
        } else {
            CorsLayer::new()
        };

        // WebSocket state uses the reactor and db_pool for session tracking
        let node_id = self.reactor.node_id();
        let ws_state = Arc::new(WsState::new(
            self.reactor.clone(),
            self.db_pool.clone(),
            node_id,
        ));

        // Readiness state for DB health check
        let readiness_state = Arc::new(ReadinessState {
            db_pool: self.db_pool.clone(),
        });

        // Build the main router with middleware
        let mut main_router = Router::new()
            // Health check endpoint (liveness)
            .route("/health", get(health_handler))
            // Readiness check endpoint (checks DB)
            .route(
                "/ready",
                get(readiness_handler).with_state(readiness_state),
            )
            // RPC endpoint
            .route("/rpc", post(rpc_handler))
            // REST-style function endpoint
            .route("/rpc/{function}", post(rpc_function_handler))
            // Add state
            .with_state(rpc_handler_state);

        // Build middleware stack
        let service_builder = ServiceBuilder::new()
            .layer(cors.clone())
            .layer(middleware::from_fn_with_state(
                auth_middleware_state,
                auth_middleware,
            ))
            .layer(middleware::from_fn(tracing_middleware));

        // Add metrics middleware if observability is enabled
        if let Some(ref observability) = self.observability {
            let metrics_state = Arc::new(MetricsState::new(observability.clone()));
            main_router = main_router.layer(middleware::from_fn_with_state(
                metrics_state,
                metrics_middleware,
            ));
        }

        // Apply the remaining middleware layers
        main_router = main_router.layer(service_builder);

        // WebSocket router without auth middleware (just CORS)
        let ws_router = Router::new()
            .route("/ws", any(ws_handler).with_state(ws_state))
            .layer(cors);

        // Merge routers - WebSocket route is separate from middleware stack
        main_router.merge(ws_router)
    }

    /// Get the socket address to bind to.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], self.config.port))
    }

    /// Run the server (blocking).
    pub async fn run(self) -> Result<(), std::io::Error> {
        let addr = self.addr();
        let router = self.router();

        // Start the reactor for real-time updates
        if let Err(e) = self.reactor.start().await {
            tracing::error!("Failed to start reactor: {}", e);
        } else {
            tracing::info!("Reactor started for real-time updates");
        }

        tracing::info!("Gateway server listening on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await
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
async fn readiness_handler(
    axum::extract::State(state): axum::extract::State<Arc<ReadinessState>>,
) -> (axum::http::StatusCode, Json<ReadinessResponse>) {
    // Check database connectivity
    let db_ok = sqlx::query("SELECT 1")
        .fetch_one(&state.db_pool)
        .await
        .is_ok();

    let ready = db_ok;
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
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
    )
}

/// Simple tracing middleware that adds TracingState to extensions.
async fn tracing_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header::HeaderName;

    // Extract or generate trace ID
    let trace_id = req
        .headers()
        .get(HeaderName::from_static("x-trace-id"))
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let tracing_state = TracingState::with_trace_id(trace_id.clone());

    let mut req = req;
    req.extensions_mut().insert(tracing_state.clone());

    // Also insert AuthContext default if not present
    if req
        .extensions()
        .get::<forge_core::function::AuthContext>()
        .is_none()
    {
        req.extensions_mut()
            .insert(forge_core::function::AuthContext::unauthenticated());
    }

    let mut response = next.run(req).await;

    // Add trace ID to response headers
    if let Ok(val) = trace_id.parse() {
        response.headers_mut().insert("x-trace-id", val);
    }
    if let Ok(val) = tracing_state.request_id.parse() {
        response.headers_mut().insert("x-request-id", val);
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_config_default() {
        let config = GatewayConfig::default();
        assert_eq!(config.port, 8080);
        assert_eq!(config.max_connections, 10000);
        assert!(config.cors_enabled);
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

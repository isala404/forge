use std::sync::Arc;

use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, header::USER_AGENT},
};
use forge_core::function::{
    AuthContext, FunctionInfo, JobDispatch, RequestMetadata, WorkflowDispatch,
};

use super::request::RpcRequest;
use super::response::{RpcError, RpcResponse};
use super::tracing::TracingState;
use crate::function::{FunctionRegistry, FunctionRouter};
use crate::pg::Database;

/// RPC handler for function invocations.
#[derive(Clone)]
pub struct RpcHandler {
    /// Function router.
    router: Arc<FunctionRouter>,
}

impl RpcHandler {
    /// Create a new RPC handler.
    pub fn new(registry: FunctionRegistry, db: Database) -> Self {
        let router = FunctionRouter::new(Arc::new(registry), db);
        Self {
            router: Arc::new(router),
        }
    }

    /// Create a new RPC handler with dispatch capabilities.
    pub fn with_dispatch(
        registry: FunctionRegistry,
        db: Database,
        job_dispatcher: Option<Arc<dyn JobDispatch>>,
        workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    ) -> Self {
        Self::with_dispatch_and_issuer(registry, db, job_dispatcher, workflow_dispatcher, None)
    }

    /// Create a new RPC handler with dispatch and token issuer.
    pub fn with_dispatch_and_issuer(
        registry: FunctionRegistry,
        db: Database,
        job_dispatcher: Option<Arc<dyn JobDispatch>>,
        workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
        token_issuer: Option<Arc<dyn forge_core::TokenIssuer>>,
    ) -> Self {
        let router = FunctionRouter::with_dispatch_and_issuer(
            Arc::new(registry),
            db,
            job_dispatcher,
            workflow_dispatcher,
            token_issuer,
        );
        Self {
            router: Arc::new(router),
        }
    }

    /// Set the token TTL config. Must be called before any requests are handled.
    pub fn set_token_ttl(&mut self, ttl: forge_core::AuthTokenTtl) {
        if let Some(router) = Arc::get_mut(&mut self.router) {
            router.set_token_ttl(ttl);
        }
    }

    /// Replace the rate-limiter backend. Call before handling requests.
    pub fn set_rate_limiter(
        &mut self,
        rate_limiter: Arc<dyn forge_core::rate_limit::RateLimiterBackend>,
    ) {
        if let Some(router) = Arc::get_mut(&mut self.router) {
            router.set_rate_limiter(rate_limiter);
        }
    }

    /// Set a custom role resolver. Call before handling requests.
    pub fn set_role_resolver(&mut self, resolver: forge_core::SharedRoleResolver) {
        if let Some(router) = Arc::get_mut(&mut self.router) {
            router.set_role_resolver(resolver);
        }
    }

    /// Expose the underlying [`FunctionRouter`] so other components (e.g. MCP)
    /// can delegate calls to all registered queries and mutations.
    pub fn router(&self) -> Arc<FunctionRouter> {
        Arc::clone(&self.router)
    }

    /// Look up function metadata by name.
    pub fn function_info(&self, name: &str) -> Option<FunctionInfo> {
        self.router.function_info(name)
    }

    /// Set the signals collector for auto-capturing RPC events.
    pub fn set_signals_collector(
        &mut self,
        collector: crate::signals::SignalsCollector,
        server_secret: String,
    ) {
        if let Some(router) = Arc::get_mut(&mut self.router) {
            router.set_signals_collector(collector, server_secret);
        }
    }

    /// Handle an RPC request.
    pub async fn handle(
        &self,
        request: RpcRequest,
        auth: AuthContext,
        metadata: RequestMetadata,
    ) -> RpcResponse {
        // Don't check has_function early - let router try jobs/workflows too
        match self
            .router
            .execute(&request.function, request.args, auth, metadata.clone())
            .await
        {
            Ok(value) => {
                RpcResponse::success(value).with_request_id(metadata.request_id().to_string())
            }
            Err(e) => RpcResponse::error(RpcError::from(e))
                .with_request_id(metadata.request_id().to_string()),
        }
    }
}

use super::ResolvedClientIp;

/// Extract user agent from headers.
fn extract_user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

/// Build request metadata from tracing state, resolved IP, and headers.
fn build_metadata(
    tracing: TracingState,
    client_ip: Option<String>,
    headers: &HeaderMap,
) -> RequestMetadata {
    RequestMetadata::__build_internal(
        uuid::Uuid::parse_str(&tracing.request_id).unwrap_or_else(|_| uuid::Uuid::new_v4()),
        tracing.trace_id,
        client_ip,
        extract_user_agent(headers),
        extract_correlation_id(headers),
    )
}

/// Extract the correlation ID from the x-correlation-id header.
fn extract_correlation_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(String::from)
}

/// Axum handler for POST /rpc.
pub async fn rpc_handler(
    State(handler): State<Arc<RpcHandler>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing): Extension<TracingState>,
    Extension(resolved_ip): Extension<ResolvedClientIp>,
    headers: HeaderMap,
    Json(request): Json<RpcRequest>,
) -> RpcResponse {
    if !is_valid_function_name(&request.function) {
        return RpcResponse::error(RpcError::validation(
            "Invalid function name: must be 1-256 alphanumeric characters, underscores, dots, colons, or hyphens",
        ));
    }
    handler
        .handle(
            request,
            auth,
            build_metadata(tracing, resolved_ip.0, &headers),
        )
        .await
}

/// Request body wrapper for REST-style RPC calls.
#[derive(Debug, serde::Deserialize)]
pub struct RpcFunctionBody {
    /// Function arguments.
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Validate that a function name contains only safe characters.
/// Prevents log injection and unexpected behavior from special characters.
fn is_valid_function_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':' || c == '-')
}

/// Axum handler for POST /rpc/:function (REST-style).
pub async fn rpc_function_handler(
    State(handler): State<Arc<RpcHandler>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing): Extension<TracingState>,
    Extension(resolved_ip): Extension<ResolvedClientIp>,
    headers: HeaderMap,
    axum::extract::Path(function): axum::extract::Path<String>,
    Json(body): Json<RpcFunctionBody>,
) -> RpcResponse {
    if !is_valid_function_name(&function) {
        return RpcResponse::error(RpcError::validation(
            "Invalid function name: must be 1-256 alphanumeric characters, underscores, dots, colons, or hyphens",
        ));
    }
    let request = RpcRequest::new(function, body.args);
    handler
        .handle(
            request,
            auth,
            build_metadata(tracing, resolved_ip.0, &headers),
        )
        .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn create_mock_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool")
    }

    fn create_test_handler() -> RpcHandler {
        let registry = FunctionRegistry::new();
        let db = Database::from_pool(create_mock_pool());
        RpcHandler::new(registry, db)
    }

    #[tokio::test]
    async fn test_handle_unknown_function() {
        let handler = create_test_handler();
        let request = RpcRequest::new("unknown_function", serde_json::json!({}));
        let auth = AuthContext::unauthenticated();
        let metadata = RequestMetadata::new();

        let response = handler.handle(request, auth, metadata).await;

        assert!(!response.success);
        assert!(response.error.is_some());
        assert_eq!(response.error.as_ref().unwrap().code, "NOT_FOUND");
    }
}

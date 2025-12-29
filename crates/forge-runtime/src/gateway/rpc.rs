use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    Json,
};
use forge_core::function::{AuthContext, JobDispatch, RequestMetadata, WorkflowDispatch};

use super::request::RpcRequest;
use super::response::{RpcError, RpcResponse};
use super::tracing::TracingState;
use crate::function::{FunctionExecutor, FunctionRegistry};

/// RPC handler for function invocations.
#[derive(Clone)]
pub struct RpcHandler {
    /// Function executor.
    executor: Arc<FunctionExecutor>,
}

impl RpcHandler {
    /// Create a new RPC handler.
    pub fn new(registry: FunctionRegistry, db_pool: sqlx::PgPool) -> Self {
        let executor = FunctionExecutor::new(Arc::new(registry), db_pool);
        Self {
            executor: Arc::new(executor),
        }
    }

    /// Create a new RPC handler with dispatch capabilities.
    pub fn with_dispatch(
        registry: FunctionRegistry,
        db_pool: sqlx::PgPool,
        job_dispatcher: Option<Arc<dyn JobDispatch>>,
        workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    ) -> Self {
        let executor = FunctionExecutor::with_dispatch(
            Arc::new(registry),
            db_pool,
            job_dispatcher,
            workflow_dispatcher,
        );
        Self {
            executor: Arc::new(executor),
        }
    }

    /// Handle an RPC request.
    pub async fn handle(
        &self,
        request: RpcRequest,
        auth: AuthContext,
        metadata: RequestMetadata,
    ) -> RpcResponse {
        // Check if function exists
        if !self.executor.has_function(&request.function) {
            return RpcResponse::error(RpcError::not_found(format!(
                "Function '{}' not found",
                request.function
            )))
            .with_request_id(metadata.request_id.to_string());
        }

        // Execute function
        match self
            .executor
            .execute(&request.function, request.args, auth, metadata.clone())
            .await
        {
            Ok(exec_result) => {
                if exec_result.success {
                    RpcResponse::success(exec_result.result)
                        .with_request_id(metadata.request_id.to_string())
                } else {
                    RpcResponse::error(RpcError::internal(
                        exec_result
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    ))
                    .with_request_id(metadata.request_id.to_string())
                }
            }
            Err(e) => RpcResponse::error(RpcError::from(e))
                .with_request_id(metadata.request_id.to_string()),
        }
    }
}

/// Axum handler for POST /rpc.
pub async fn rpc_handler(
    State(handler): State<Arc<RpcHandler>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing): Extension<TracingState>,
    Json(request): Json<RpcRequest>,
) -> RpcResponse {
    let metadata = RequestMetadata {
        request_id: uuid::Uuid::parse_str(&tracing.request_id)
            .unwrap_or_else(|_| uuid::Uuid::new_v4()),
        trace_id: tracing.trace_id,
        client_ip: None,
        user_agent: None,
        timestamp: chrono::Utc::now(),
    };

    handler.handle(request, auth, metadata).await
}

/// Axum handler for POST /rpc/:function (REST-style).
pub async fn rpc_function_handler(
    State(handler): State<Arc<RpcHandler>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing): Extension<TracingState>,
    axum::extract::Path(function): axum::extract::Path<String>,
    Json(args): Json<serde_json::Value>,
) -> RpcResponse {
    let request = RpcRequest::new(function, args);

    let metadata = RequestMetadata {
        request_id: uuid::Uuid::parse_str(&tracing.request_id)
            .unwrap_or_else(|_| uuid::Uuid::new_v4()),
        trace_id: tracing.trace_id,
        client_ip: None,
        user_agent: None,
        timestamp: chrono::Utc::now(),
    };

    handler.handle(request, auth, metadata).await
}

#[cfg(test)]
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
        let db_pool = create_mock_pool();
        RpcHandler::new(registry, db_pool)
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

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]

use super::super::ResolvedClientIp;
use super::super::tracing::TracingState;
use super::*;
use axum::body::to_bytes;
use forge_core::function::AuthContext;
use forge_core::mcp::{ForgeMcpTool, McpToolAnnotations, McpToolContext, McpToolInfo};
use forge_core::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// `McpConfig` is `#[non_exhaustive]`, so cross-crate functional-record-update
/// is blocked. Build a default and mutate the field we care about.
fn mcp_enabled() -> McpConfig {
    let mut c = McpConfig::default();
    c.enabled = true;
    c
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EchoArgs {
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    echoed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ExportFormat {
    Json,
    Csv,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct MetadataArgs {
    #[schemars(description = "Project UUID to export")]
    project_id: String,
    format: ExportFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct MetadataOutput {
    accepted: bool,
}

struct EchoTool;

impl forge_core::__sealed::Sealed for EchoTool {}

impl ForgeMcpTool for EchoTool {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn info() -> McpToolInfo {
        McpToolInfo {
            name: "echo",
            title: Some("Echo"),
            description: Some("Echo back the message"),
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
        _ctx: &McpToolContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Self::Output>> + Send + '_>> {
        Box::pin(async move {
            Ok(EchoOutput {
                echoed: args.message,
            })
        })
    }
}

struct AdminTool;

impl forge_core::__sealed::Sealed for AdminTool {}

impl ForgeMcpTool for AdminTool {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn info() -> McpToolInfo {
        McpToolInfo {
            name: "admin.echo",
            title: Some("Admin Echo"),
            description: Some("Admin only echo"),
            required_role: Some("admin"),
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
        _ctx: &McpToolContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Self::Output>> + Send + '_>> {
        Box::pin(async move {
            Ok(EchoOutput {
                echoed: args.message,
            })
        })
    }
}

struct MetadataTool;

impl forge_core::__sealed::Sealed for MetadataTool {}

impl ForgeMcpTool for MetadataTool {
    type Args = MetadataArgs;
    type Output = MetadataOutput;

    fn info() -> McpToolInfo {
        McpToolInfo {
            name: "export.project",
            title: Some("Export Project"),
            description: Some("Export project data"),
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
        _ctx: &McpToolContext,
        _args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Self::Output>> + Send + '_>> {
        Box::pin(async move { Ok(MetadataOutput { accepted: true }) })
    }
}

#[test]
fn test_json_rpc_helpers() {
    let success = json_rpc_success(
        Some(serde_json::json!(1)),
        serde_json::json!({ "ok": true }),
    );
    assert_eq!(success["jsonrpc"], "2.0");
    assert!(success.get("result").is_some());

    let err = json_rpc_error(Some(serde_json::json!(1)), -32601, "not found", None);
    assert_eq!(err["error"]["code"], -32601);
}

fn test_state(config: McpConfig) -> Arc<McpState> {
    test_state_with_registry(config, McpToolRegistry::new())
}

fn test_state_with_registry(config: McpConfig, registry: McpToolRegistry) -> Arc<McpState> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgres://localhost/nonexistent")
        .expect("lazy pool must build");
    Arc::new(McpState::new(config, registry, pool, None, None, None))
}

async fn response_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    if bytes.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_slice(&bytes).expect("valid json")
}

async fn initialize_session(state: Arc<McpState>) -> String {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0.0" }
        }
    });
    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        HeaderMap::new(),
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .expect("session id must exist")
        .to_string()
}

async fn mark_initialized(state: Arc<McpState>, headers: HeaderMap) {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

async fn initialized_headers(state: Arc<McpState>) -> HeaderMap {
    let session_id = initialize_session(state.clone()).await;
    let mut headers = HeaderMap::new();
    headers.insert(
        MCP_SESSION_HEADER,
        HeaderValue::from_str(&session_id).expect("valid session id header"),
    );
    headers.insert(
        MCP_PROTOCOL_HEADER,
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    mark_initialized(state, headers.clone()).await;
    headers
}

#[tokio::test]
async fn test_initialize_sets_session_header() {
    let state = test_state(mcp_enabled());
    let session = initialize_session(state).await;
    assert!(!session.is_empty());
}

#[tokio::test]
async fn test_initialize_rejects_unsupported_protocol_version() {
    let state = test_state(mcp_enabled());
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-01-01",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0.0" }
        }
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        HeaderMap::new(),
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], -32602);
    let supported = body["error"]["data"]["supported"]
        .as_array()
        .expect("supported versions array");
    assert!(
        supported
            .iter()
            .any(|value| value.as_str() == Some(MCP_PROTOCOL_VERSION))
    );
}

#[tokio::test]
async fn test_tools_list_requires_initialized_session() {
    let state = test_state(mcp_enabled());

    let session_id = initialize_session(state.clone()).await;

    let mut headers = HeaderMap::new();
    headers.insert(
        MCP_SESSION_HEADER,
        HeaderValue::from_str(&session_id).expect("valid"),
    );
    headers.insert(
        MCP_PROTOCOL_HEADER,
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );

    let list_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(list_payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_tools_list_returns_registered_tools() {
    let mut registry = McpToolRegistry::new();
    registry.register::<EchoTool>();

    let state = test_state_with_registry(mcp_enabled(), registry);
    let headers = initialized_headers(state.clone()).await;
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools list should be array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "echo");
    assert!(tools[0].get("inputSchema").is_some());
    assert!(tools[0].get("outputSchema").is_some());
}

#[tokio::test]
async fn test_tools_list_exposes_parameter_metadata() {
    let mut registry = McpToolRegistry::new();
    registry.register::<MetadataTool>();

    let state = test_state_with_registry(mcp_enabled(), registry);
    let headers = initialized_headers(state.clone()).await;
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/list",
        "params": {}
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools list should be array");
    assert_eq!(tools.len(), 1);

    let input_schema = &tools[0]["inputSchema"];
    assert_eq!(
        input_schema["properties"]["project_id"]["description"],
        "Project UUID to export"
    );

    let schema_text = input_schema.to_string();
    assert!(schema_text.contains("\"json\""));
    assert!(schema_text.contains("\"csv\""));
}

#[tokio::test]
async fn test_tools_call_success_returns_structured_content() {
    let mut registry = McpToolRegistry::new();
    registry.register::<EchoTool>();

    let state = test_state_with_registry(mcp_enabled(), registry);
    let headers = initialized_headers(state.clone()).await;
    let auth = AuthContext::authenticated(
        uuid::Uuid::new_v4(),
        vec!["member".to_string()],
        HashMap::new(),
    );
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "echo",
            "arguments": { "message": "hello" }
        }
    });

    let response = mcp_post_handler(
        State(state),
        Extension(auth),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["structuredContent"]["echoed"], "hello");
    assert_eq!(body["result"]["content"][0]["type"], "text");
}

#[tokio::test]
async fn test_tools_call_validation_failure_returns_is_error() {
    let mut registry = McpToolRegistry::new();
    registry.register::<EchoTool>();

    let state = test_state_with_registry(mcp_enabled(), registry);
    let headers = initialized_headers(state.clone()).await;
    let auth = AuthContext::authenticated(
        uuid::Uuid::new_v4(),
        vec!["member".to_string()],
        HashMap::new(),
    );
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "echo",
            "arguments": {}
        }
    });

    let response = mcp_post_handler(
        State(state),
        Extension(auth),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["isError"], true);
}

#[tokio::test]
async fn test_tools_call_requires_authentication() {
    let mut registry = McpToolRegistry::new();
    registry.register::<EchoTool>();

    let state = test_state_with_registry(mcp_enabled(), registry);
    let headers = initialized_headers(state.clone()).await;
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "echo",
            "arguments": { "message": "hello" }
        }
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], -32001);
}

#[tokio::test]
async fn test_tools_call_requires_role() {
    let mut registry = McpToolRegistry::new();
    registry.register::<AdminTool>();

    let state = test_state_with_registry(mcp_enabled(), registry);
    let headers = initialized_headers(state.clone()).await;
    let auth = AuthContext::authenticated(
        uuid::Uuid::new_v4(),
        vec!["member".to_string()],
        HashMap::new(),
    );
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "admin.echo",
            "arguments": { "message": "hello" }
        }
    });

    let response = mcp_post_handler(
        State(state),
        Extension(auth),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], -32003);
}

#[tokio::test]
async fn test_invalid_protocol_header_returns_400() {
    let state = test_state(mcp_enabled());
    let session_id = initialize_session(state.clone()).await;
    let mut headers = HeaderMap::new();
    headers.insert(
        MCP_SESSION_HEADER,
        HeaderValue::from_str(&session_id).expect("valid"),
    );
    headers.insert(
        MCP_PROTOCOL_HEADER,
        HeaderValue::from_static("invalid-version"),
    );

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/list",
        "params": {}
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_expired_session_is_rejected_after_cleanup() {
    let state = test_state(mcp_enabled());
    let session_id = "expired-session".to_string();
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            session_id.clone(),
            McpSession {
                initialized: true,
                protocol_version: MCP_PROTOCOL_VERSION.to_string(),
                expires_at: Instant::now() - Duration::from_secs(1),
                principal_id: None,
            },
        );
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        MCP_SESSION_HEADER,
        HeaderValue::from_str(&session_id).expect("valid session id"),
    );
    headers.insert(
        MCP_PROTOCOL_HEADER,
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/list",
        "params": {}
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], -32600);
    assert_eq!(
        body["error"]["message"],
        "Unknown MCP session. Re-initialize."
    );
}

#[tokio::test]
async fn test_missing_protocol_header_returns_400() {
    let state = test_state(mcp_enabled());
    let session_id = initialize_session(state.clone()).await;
    let mut headers = HeaderMap::new();
    headers.insert(
        MCP_SESSION_HEADER,
        HeaderValue::from_str(&session_id).expect("valid"),
    );

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/list",
        "params": {}
    });

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_notifications_return_202() {
    let state = test_state(mcp_enabled());
    let mut headers = HeaderMap::new();
    headers.insert(
        MCP_PROTOCOL_HEADER,
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {}
    });
    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn test_invalid_origin_rejected() {
    let state = test_state({
        let mut c = McpConfig::default();
        c.enabled = true;
        c.allowed_origins = vec!["https://allowed.example".to_string()];
        c
    });
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0.0" }
        }
    });

    let mut headers = HeaderMap::new();
    headers.insert("origin", HeaderValue::from_static("https://evil.example"));

    let response = mcp_post_handler(
        State(state),
        Extension(AuthContext::unauthenticated()),
        Extension(TracingState::new()),
        Extension(ResolvedClientIp(None)),
        Method::POST,
        headers,
        Json(payload),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

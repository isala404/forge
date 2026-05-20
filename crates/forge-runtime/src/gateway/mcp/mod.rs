mod session;
mod tools;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use forge_core::config::McpConfig;
use forge_core::function::{AuthContext, JobDispatch, KvHandle, RequestMetadata, WorkflowDispatch};
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::function::FunctionRouter;
use crate::mcp::McpToolRegistry;
use crate::rate_limit::StrictRateLimiter;

use self::session::{
    McpSession, enforce_protocol_header, handle_initialize, handle_notification,
    required_session_id, validate_origin,
};
use self::tools::{handle_tools_call, handle_tools_list};

pub(super) const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-03-26", "2024-11-05"];
#[cfg(test)]
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub(super) const MCP_SESSION_HEADER: &str = "mcp-session-id";
pub(super) const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
pub(super) const DEFAULT_PAGE_SIZE: usize = 50;

#[derive(Clone)]
pub struct McpState {
    pub(super) config: McpConfig,
    pub(super) registry: McpToolRegistry,
    pub(super) pool: sqlx::PgPool,
    pub(super) sessions: Arc<RwLock<HashMap<String, McpSession>>>,
    pub(super) job_dispatcher: Option<Arc<dyn JobDispatch>>,
    pub(super) workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    pub(super) kv: Option<Arc<dyn KvHandle>>,
    pub(super) rate_limiter: Arc<StrictRateLimiter>,
    /// When set, all registered queries and mutations are also exposed as MCP
    /// tools without requiring a separate `#[mcp_tool]` declaration.
    pub(super) function_router: Option<Arc<FunctionRouter>>,
}

impl McpState {
    pub fn new(
        config: McpConfig,
        registry: McpToolRegistry,
        pool: sqlx::PgPool,
        job_dispatcher: Option<Arc<dyn JobDispatch>>,
        workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
        function_router: Option<Arc<FunctionRouter>>,
    ) -> Self {
        Self {
            config,
            registry,
            pool: pool.clone(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            job_dispatcher,
            workflow_dispatcher,
            kv: None,
            rate_limiter: Arc::new(StrictRateLimiter::new(pool)),
            function_router,
        }
    }

    /// Attach a KV store handle so MCP tool handlers can call `ctx.kv()`.
    pub fn with_kv(mut self, kv: Arc<dyn KvHandle>) -> Self {
        self.kv = Some(kv);
        self
    }

    pub(super) async fn cleanup_expired_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();
        sessions.retain(|_, session| session.expires_at > now);
    }

    pub(super) async fn touch_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.expires_at =
                Instant::now() + Duration::from_secs(self.config.session_ttl.as_secs());
        }
    }
}

/// Wraps an mpsc::Receiver as a Stream for MCP SSE.
struct McpReceiverStream {
    rx: tokio::sync::mpsc::Receiver<Result<Event, std::convert::Infallible>>,
}

impl Stream for McpReceiverStream {
    type Item = Result<Event, std::convert::Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// MCP Streamable HTTP GET handler.
///
/// Opens a Server-Sent Events stream for server-initiated messages.
/// Clients use this to receive notifications and asynchronous responses
/// from the MCP server. The stream starts with an `endpoint` event
/// containing the session ID, then sends keepalive pings every 30 seconds.
pub async fn mcp_get_handler(State(state): State<Arc<McpState>>, headers: HeaderMap) -> Response {
    if let Err(resp) = validate_origin(&headers, &state.config) {
        return *resp;
    }

    if let Err(resp) = enforce_protocol_header(&state.config, &headers) {
        return *resp;
    }

    // Require a valid session (created via POST initialize)
    let session_id = match required_session_id(&state, &headers, true).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    state.touch_session(&session_id).await;

    // Create a channel for server-to-client messages
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    // Send the initial endpoint event with the session binding
    let session_id_clone = session_id.clone();
    tokio::spawn(async move {
        let endpoint_data = serde_json::json!({
            "sessionId": session_id_clone,
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("endpoint")
                .data(endpoint_data.to_string())))
            .await;

        // Keep channel open until client disconnects.
        // The SSE keepalive mechanism handles pings.
        // When the client disconnects, the tx will be dropped.
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            if tx.is_closed() {
                break;
            }
        }
    });

    let stream = McpReceiverStream { rx };

    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response();

    // Set MCP session header on the response
    if let Ok(val) = HeaderValue::from_str(&session_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(MCP_SESSION_HEADER), val);
    }

    response
}

pub async fn mcp_post_handler(
    State(state): State<Arc<McpState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing): Extension<super::tracing::TracingState>,
    Extension(resolved_ip): Extension<super::ResolvedClientIp>,
    method: Method,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    if method != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json_rpc_error(None, -32601, "Only POST is supported", None)),
        )
            .into_response();
    }

    if let Err(resp) = validate_origin(&headers, &state.config) {
        return *resp;
    }

    state.cleanup_expired_sessions().await;

    let Some(method_name) = payload.get("method").and_then(Value::as_str) else {
        // Notifications / responses sent by client should get 202 when accepted.
        if payload.get("id").is_some()
            && (payload.get("result").is_some() || payload.get("error").is_some())
        {
            return StatusCode::ACCEPTED.into_response();
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(json_rpc_error(
                None,
                -32600,
                "Invalid JSON-RPC payload",
                None,
            )),
        )
            .into_response();
    };

    let id = payload.get("id").cloned();
    let params = payload
        .get("params")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // Notification flow
    if id.is_none() {
        return handle_notification(&state, method_name, params, &headers).await;
    }

    // Request flow
    if method_name != "initialize"
        && let Err(resp) = enforce_protocol_header(&state.config, &headers)
    {
        return *resp;
    }

    match method_name {
        "initialize" => handle_initialize(&state, id, &params, &auth).await,
        "tools/list" => {
            let session_id = match required_session_id(&state, &headers, true).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            state.touch_session(&session_id).await;
            handle_tools_list(&state, id, &params)
        }
        "tools/call" => {
            let session_id = match required_session_id(&state, &headers, true).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            state.touch_session(&session_id).await;

            let metadata = build_request_metadata(&tracing, resolved_ip.0.clone(), &headers);
            handle_tools_call(&state, id, &params, &auth, metadata).await
        }
        _ => (
            StatusCode::OK,
            Json(json_rpc_error(id, -32601, "Method not found", None)),
        )
            .into_response(),
    }
}

fn extract_user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

fn build_request_metadata(
    tracing: &super::tracing::TracingState,
    client_ip: Option<String>,
    headers: &HeaderMap,
) -> RequestMetadata {
    RequestMetadata::__build_internal(
        uuid::Uuid::parse_str(&tracing.request_id).unwrap_or_else(|_| uuid::Uuid::new_v4()),
        tracing.trace_id.clone(),
        client_ip,
        extract_user_agent(headers),
        None,
    )
}

pub(super) fn json_rpc_success(id: Option<Value>, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

pub(super) fn json_rpc_error(
    id: Option<Value>,
    code: i32,
    message: impl Into<String>,
    data: Option<Value>,
) -> Value {
    let mut error = serde_json::json!({
        "code": code,
        "message": message.into()
    });
    if let Some(data) = data
        && let Some(obj) = error.as_object_mut()
    {
        obj.insert("data".to_string(), data);
    }

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": error
    })
}

pub(super) fn set_header(response: &mut Response<Body>, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::from_str(value)) {
        response.headers_mut().insert(name, value);
    }
}

#[cfg(test)]
mod tests;

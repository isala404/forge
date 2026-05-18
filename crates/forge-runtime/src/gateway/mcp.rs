// TODO(pre-1.0): Split into smaller modules
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::{Extension, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{Json, body::Body};
use forge_core::config::McpConfig;
use forge_core::function::{AuthContext, JobDispatch, KvHandle, RequestMetadata, WorkflowDispatch};
use forge_core::mcp::McpToolContext;
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::function::FunctionRouter;
use crate::mcp::McpToolRegistry;
use crate::rate_limit::StrictRateLimiter;

const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-03-26", "2024-11-05"];
#[cfg(test)]
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_SESSION_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
const DEFAULT_PAGE_SIZE: usize = 50;
type ResponseError = Box<Response>;

#[derive(Debug, Clone)]
struct McpSession {
    initialized: bool,
    protocol_version: String,
    expires_at: Instant,
    principal_id: Option<String>,
}

#[derive(Clone)]
pub struct McpState {
    config: McpConfig,
    registry: McpToolRegistry,
    pool: sqlx::PgPool,
    sessions: Arc<RwLock<HashMap<String, McpSession>>>,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
    workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    kv: Option<Arc<dyn KvHandle>>,
    rate_limiter: Arc<StrictRateLimiter>,
    /// When set, all registered queries and mutations are also exposed as MCP
    /// tools without requiring a separate `#[mcp_tool]` declaration.
    function_router: Option<Arc<FunctionRouter>>,
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

    async fn cleanup_expired_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();
        sessions.retain(|_, session| session.expires_at > now);
    }

    async fn touch_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.expires_at =
                Instant::now() + Duration::from_secs(self.config.session_ttl.as_secs());
        }
    }

    fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.config.session_ttl.as_secs())
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
    Extension(resolved_ip): Extension<ResolvedClientIp>,
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

async fn handle_notification(
    state: &Arc<McpState>,
    method_name: &str,
    _params: Value,
    headers: &HeaderMap,
) -> Response {
    if let Err(resp) = enforce_protocol_header(&state.config, headers) {
        return *resp;
    }

    match method_name {
        "notifications/initialized" => {
            let session_id = match required_session_id(state, headers, false).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                session.initialized = true;
                session.expires_at = Instant::now() + state.session_ttl();
                return StatusCode::ACCEPTED.into_response();
            }

            (
                StatusCode::BAD_REQUEST,
                Json(json_rpc_error(
                    None,
                    -32600,
                    "Unknown MCP session. Re-initialize the connection.",
                    None,
                )),
            )
                .into_response()
        }
        _ => StatusCode::ACCEPTED.into_response(),
    }
}

async fn handle_initialize(
    state: &Arc<McpState>,
    id: Option<Value>,
    params: &Value,
    auth: &forge_core::function::AuthContext,
) -> Response {
    let Some(requested_version) = params.get("protocolVersion").and_then(Value::as_str) else {
        return (
            StatusCode::OK,
            Json(json_rpc_error(
                id,
                -32602,
                "Missing protocolVersion in initialize params",
                None,
            )),
        )
            .into_response();
    };

    if !SUPPORTED_VERSIONS.contains(&requested_version) {
        return (
            StatusCode::OK,
            Json(json_rpc_error(
                id,
                -32602,
                "Unsupported protocolVersion",
                Some(serde_json::json!({
                    "supported": SUPPORTED_VERSIONS
                })),
            )),
        )
            .into_response();
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let principal = auth.principal_id();
    {
        let mut sessions = state.sessions.write().await;
        // Enforce global session limit to prevent memory exhaustion DoS
        if sessions.len() >= state.config.max_sessions {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json_rpc_error(
                    id,
                    -32000,
                    "Server at MCP session capacity",
                    None,
                )),
            )
                .into_response();
        }
        // Enforce per-user session limit
        if let Some(ref pid) = principal {
            let user_count = sessions
                .values()
                .filter(|s| s.principal_id.as_ref() == Some(pid))
                .count();
            if user_count >= state.config.max_sessions_per_user {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json_rpc_error(
                        id,
                        -32000,
                        "Per-user MCP session limit reached",
                        None,
                    )),
                )
                    .into_response();
            }
        }
        sessions.insert(
            session_id.clone(),
            McpSession {
                initialized: false,
                protocol_version: requested_version.to_string(),
                expires_at: Instant::now() + state.session_ttl(),
                principal_id: principal,
            },
        );
    }

    let mut response = (
        StatusCode::OK,
        Json(json_rpc_success(
            id,
            serde_json::json!({
                "protocolVersion": requested_version,
                "capabilities": {
                    "tools": {
                        "listChanged": false
                    }
                },
                "serverInfo": {
                    "name": "forge",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
    )
        .into_response();

    set_header(&mut response, MCP_SESSION_HEADER, &session_id);
    set_header(&mut response, MCP_PROTOCOL_HEADER, requested_version);
    response
}

/// Handle `tools/list` — unauthenticated by MCP protocol design.
///
/// MCP clients call this to discover available tools before establishing an
/// authenticated session. Tool *execution* (tools/call) requires auth; only
/// discovery is open.
fn handle_tools_list(state: &Arc<McpState>, id: Option<Value>, params: &Value) -> Response {
    let cursor = params.get("cursor").and_then(Value::as_str);
    let start = match cursor {
        Some(c) => match c.parse::<usize>() {
            Ok(v) => v,
            Err(_) => {
                return (
                    StatusCode::OK,
                    Json(json_rpc_error(
                        id,
                        -32602,
                        "Invalid cursor in tools/list request",
                        None,
                    )),
                )
                    .into_response();
            }
        },
        None => 0,
    };

    // Collect explicit MCP tool entries as JSON objects, sorted by name.
    let mut mcp_tools: Vec<_> = state.registry.list().collect();
    mcp_tools.sort_by(|a, b| a.info.name.cmp(b.info.name));

    let mut all_tools: Vec<Value> = mcp_tools
        .iter()
        .map(|entry| {
            // Only include annotation fields that are set (non-null).
            // Claude Code's MCP client rejects null booleans.
            let mut annotations = serde_json::Map::new();
            if let Some(title) = &entry.info.annotations.title {
                annotations.insert("title".into(), serde_json::Value::String(title.to_string()));
            }
            if let Some(v) = entry.info.annotations.read_only_hint {
                annotations.insert("readOnlyHint".into(), serde_json::Value::Bool(v));
            }
            if let Some(v) = entry.info.annotations.destructive_hint {
                annotations.insert("destructiveHint".into(), serde_json::Value::Bool(v));
            }
            if let Some(v) = entry.info.annotations.idempotent_hint {
                annotations.insert("idempotentHint".into(), serde_json::Value::Bool(v));
            }
            if let Some(v) = entry.info.annotations.open_world_hint {
                annotations.insert("openWorldHint".into(), serde_json::Value::Bool(v));
            }

            let mut value = serde_json::json!({
                "name": entry.info.name,
                "description": entry.info.description,
                "inputSchema": entry.input_schema,
            });
            // json!({}) always produces Value::Object
            let obj = value.as_object_mut().expect("json! object literal");

            if let Some(title) = &entry.info.title {
                obj.insert("title".into(), serde_json::Value::String(title.to_string()));
            }
            if !annotations.is_empty() {
                obj.insert("annotations".into(), serde_json::Value::Object(annotations));
            }
            if !entry.info.icons.is_empty() {
                let icons: Vec<_> = entry
                    .info
                    .icons
                    .iter()
                    .map(|icon| {
                        serde_json::json!({
                            "src": icon.src,
                            "mimeType": icon.mime_type,
                            "sizes": icon.sizes,
                            "theme": icon.theme
                        })
                    })
                    .collect();
                obj.insert("icons".into(), serde_json::Value::Array(icons));
            }
            if let Some(output_schema) = &entry.output_schema {
                // MCP spec requires outputSchema to have type: "object".
                // schemars may generate type: "array" (Vec<T>) or anyOf (Option<T>).
                // Wrap non-object schemas so they conform.
                let schema = normalize_output_schema(output_schema);
                obj.insert("outputSchema".into(), schema);
            }
            value
        })
        .collect();

    // Also expose public queries and mutations as MCP tools, unless
    // they are already covered by an explicit `#[mcp_tool]` declaration.
    if let Some(router) = &state.function_router {
        let mut fn_infos = router.function_infos();
        fn_infos.retain(|info| info.is_public);
        fn_infos.sort_by(|a, b| a.name.cmp(b.name));

        for info in fn_infos {
            // Skip if an explicit MCP tool with the same name already exists.
            if state.registry.get(info.name).is_some() {
                continue;
            }

            let kind_str = match info.kind {
                forge_core::function::FunctionKind::Query => "query",
                forge_core::function::FunctionKind::Mutation => "mutation",
                _ => "function",
            };
            let description = info
                .description
                .map(|d| d.to_string())
                .unwrap_or_else(|| format!("Forge {} '{}'", kind_str, info.name));

            // Use a permissive schema — FunctionRouter handles type-safe
            // deserialization at call time via serde.
            let input_schema = serde_json::json!({
                "type": "object",
                "additionalProperties": true
            });

            let mut tool = serde_json::json!({
                "name": info.name,
                "description": description,
                "inputSchema": input_schema,
            });

            // Annotate queries as read-only hints for MCP clients.
            if info.kind == forge_core::function::FunctionKind::Query {
                let obj = tool.as_object_mut().expect("json! object literal");
                obj.insert(
                    "annotations".into(),
                    serde_json::json!({ "readOnlyHint": true }),
                );
            }

            all_tools.push(tool);
        }

        // Re-sort combined list so the final page is in stable alphabetical order.
        all_tools.sort_by(|a, b| {
            let name_a = a.get("name").and_then(Value::as_str).unwrap_or("");
            let name_b = b.get("name").and_then(Value::as_str).unwrap_or("");
            name_a.cmp(name_b)
        });
    }

    let page: Vec<Value> = all_tools
        .iter()
        .skip(start)
        .take(DEFAULT_PAGE_SIZE)
        .cloned()
        .collect();

    let end = start.saturating_add(page.len());

    // Build result: omit nextCursor when null (Claude Code expects string or absent)
    let mut result = serde_json::json!({ "tools": page });
    if end < all_tools.len() && result.is_object() {
        // json!({}) always produces Value::Object
        result
            .as_object_mut()
            .expect("json! object literal")
            .insert(
                "nextCursor".into(),
                serde_json::Value::String(end.to_string()),
            );
    }

    (StatusCode::OK, Json(json_rpc_success(id, result))).into_response()
}

/// MCP spec requires outputSchema to be `type: "object"`. Wrap schemas that
/// schemars generates as arrays or union types (anyOf/oneOf for Option<T>).
fn normalize_output_schema(schema: &Value) -> Value {
    let type_str = schema.get("type").and_then(Value::as_str).unwrap_or("");
    if type_str == "object" {
        return schema.clone();
    }

    // Wrap non-object schemas: put the original under a "result" property
    let mut wrapper = serde_json::json!({
        "type": "object",
        "properties": {
            "result": schema
        }
    });

    // Hoist $schema and definitions to the wrapper level
    if let (Some(s), Some(obj)) = (schema.get("$schema"), wrapper.as_object_mut()) {
        obj.insert("$schema".into(), s.clone());
    }
    if let (Some(d), Some(obj)) = (schema.get("definitions"), wrapper.as_object_mut()) {
        obj.insert("definitions".into(), d.clone());
        // Remove from the nested copy to avoid duplication
        if let Some(inner) = wrapper.pointer_mut("/properties/result") {
            inner.as_object_mut().map(|o| o.remove("definitions"));
        }
    }

    wrapper
}

async fn handle_tools_call(
    state: &Arc<McpState>,
    id: Option<Value>,
    params: &Value,
    auth: &AuthContext,
    request_metadata: RequestMetadata,
) -> Response {
    let Some(tool_name) = params.get("name").and_then(Value::as_str) else {
        return (
            StatusCode::OK,
            Json(json_rpc_error(id, -32602, "Missing tool name", None)),
        )
            .into_response();
    };

    let Some(entry) = state.registry.get(tool_name) else {
        // Fall through to FunctionRouter: expose registered queries/mutations
        // as MCP tools without requiring a separate `#[mcp_tool]` declaration.
        if let Some(router) = &state.function_router
            && router.has_function(tool_name)
        {
            return handle_proxied_function_call(
                router,
                id,
                tool_name,
                params,
                auth,
                request_metadata,
            )
            .await;
        }
        return (
            StatusCode::OK,
            Json(json_rpc_error(id, -32602, "Unknown tool", None)),
        )
            .into_response();
    };

    if !entry.info.is_public && !auth.is_authenticated() {
        #[cfg(feature = "mcp-oauth")]
        if state.config.oauth {
            // Return HTTP 401 with discovery header so MCP clients trigger OAuth flow
            let mut response = (
                StatusCode::UNAUTHORIZED,
                Json(json_rpc_error(id, -32001, "Authentication required", None)),
            )
                .into_response();
            response.headers_mut().insert(
                "WWW-Authenticate",
                axum::http::header::HeaderValue::from_static(
                    "Bearer resource_metadata=\"/.well-known/oauth-protected-resource\"",
                ),
            );
            return response;
        }
        return (
            StatusCode::OK,
            Json(json_rpc_error(id, -32001, "Authentication required", None)),
        )
            .into_response();
    }
    if let Some(role) = entry.info.required_role
        && !auth.has_role(role)
    {
        return (
            StatusCode::OK,
            Json(json_rpc_error(
                id,
                -32003,
                format!("Role '{}' required", role),
                None,
            )),
        )
            .into_response();
    }

    if let (Some(requests), Some(per_secs)) = (
        entry.info.rate_limit_requests,
        entry.info.rate_limit_per_secs,
    ) {
        let key_type: forge_core::RateLimitKey = entry
            .info
            .rate_limit_key
            .and_then(|k| k.parse().ok())
            .unwrap_or_default();

        let config = forge_core::RateLimitConfig::new(requests, Duration::from_secs(per_secs))
            .with_key(key_type.clone());
        let bucket_key = state
            .rate_limiter
            .build_key(key_type, tool_name, auth, &request_metadata);

        if let Err(e) = state.rate_limiter.enforce(&bucket_key, &config).await {
            return (
                StatusCode::OK,
                Json(json_rpc_error(id, -32029, e.to_string(), None)),
            )
                .into_response();
        }
    }

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    if let Err(validation_err) = jsonschema::validate(&entry.input_schema, &args) {
        let msg = format!("Invalid tool arguments: {validation_err}");
        return (
            StatusCode::OK,
            Json(json_rpc_success(
                id,
                serde_json::json!({
                    "content": [{ "type": "text", "text": msg }],
                    "isError": true
                }),
            )),
        )
            .into_response();
    }

    let mut ctx = McpToolContext::with_dispatch(
        state.pool.clone(),
        auth.clone(),
        request_metadata,
        state.job_dispatcher.clone(),
        state.workflow_dispatcher.clone(),
    );
    if let Some(ref kv) = state.kv {
        ctx.set_kv(Arc::clone(kv));
    }

    let result = if let Some(timeout_dur) = entry.info.timeout {
        match tokio::time::timeout(timeout_dur, (entry.handler)(&ctx, args)).await {
            Ok(inner) => inner,
            Err(_) => {
                return (
                    StatusCode::OK,
                    Json(json_rpc_error(id, -32000, "Tool timed out", None)),
                )
                    .into_response();
            }
        }
    } else {
        (entry.handler)(&ctx, args).await
    };

    match result {
        Ok(output) => {
            let result = tool_success_result(output);
            (
                StatusCode::OK,
                Json(json_rpc_success(id, serde_json::json!(result))),
            )
                .into_response()
        }
        Err(e) => match e {
            forge_core::ForgeError::Validation(msg)
            | forge_core::ForgeError::InvalidArgument(msg) => (
                StatusCode::OK,
                Json(json_rpc_success(
                    id,
                    serde_json::json!({
                        "content": [{ "type": "text", "text": msg }],
                        "isError": true
                    }),
                )),
            )
                .into_response(),
            forge_core::ForgeError::Unauthorized(msg) => {
                (StatusCode::OK, Json(json_rpc_error(id, -32001, msg, None))).into_response()
            }
            forge_core::ForgeError::Forbidden(msg) => {
                (StatusCode::OK, Json(json_rpc_error(id, -32003, msg, None))).into_response()
            }
            _ => (
                StatusCode::OK,
                Json(json_rpc_error(id, -32603, "Internal server error", None)),
            )
                .into_response(),
        },
    }
}

/// Proxy a `tools/call` request to the [`FunctionRouter`] for a registered
/// query or mutation that has no explicit `#[mcp_tool]` declaration.
///
/// Auth enforcement (public flag, required role, rate limits) is handled
/// entirely by [`FunctionRouter::execute`], so we only need to map the result
/// back to the MCP wire format.
async fn handle_proxied_function_call(
    router: &Arc<FunctionRouter>,
    id: Option<Value>,
    tool_name: &str,
    params: &Value,
    auth: &AuthContext,
    request_metadata: RequestMetadata,
) -> Response {
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    match router
        .execute(tool_name, args, auth.clone(), request_metadata)
        .await
    {
        Ok(output) => {
            let result = tool_success_result(output);
            (
                StatusCode::OK,
                Json(json_rpc_success(id, serde_json::json!(result))),
            )
                .into_response()
        }
        Err(e) => match e {
            forge_core::ForgeError::Validation(msg)
            | forge_core::ForgeError::InvalidArgument(msg) => (
                StatusCode::OK,
                Json(json_rpc_success(
                    id,
                    serde_json::json!({
                        "content": [{ "type": "text", "text": msg }],
                        "isError": true
                    }),
                )),
            )
                .into_response(),
            forge_core::ForgeError::Unauthorized(_) => (
                StatusCode::OK,
                Json(json_rpc_error(id, -32001, "Authentication required", None)),
            )
                .into_response(),
            forge_core::ForgeError::Forbidden(_) => (
                StatusCode::OK,
                Json(json_rpc_error(id, -32003, "Forbidden", None)),
            )
                .into_response(),
            forge_core::ForgeError::RateLimitExceeded { .. } => (
                StatusCode::OK,
                Json(json_rpc_error(id, -32029, e.to_string(), None)),
            )
                .into_response(),
            forge_core::ForgeError::Timeout(_) => (
                StatusCode::OK,
                Json(json_rpc_error(id, -32000, "Function timed out", None)),
            )
                .into_response(),
            other => (
                StatusCode::OK,
                Json(json_rpc_success(
                    id,
                    serde_json::json!({
                        "content": [{ "type": "text", "text": other.to_string() }],
                        "isError": true
                    }),
                )),
            )
                .into_response(),
        },
    }
}

fn tool_success_result(output: Value) -> Value {
    match output {
        Value::Object(_) => serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&output).unwrap_or_else(|_| "{}".to_string())
            }],
            "structuredContent": output
        }),
        Value::String(text) => serde_json::json!({
            "content": [{ "type": "text", "text": text }]
        }),
        other => serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&other).unwrap_or_else(|_| "null".to_string())
            }]
        }),
    }
}

async fn required_session_id(
    state: &Arc<McpState>,
    headers: &HeaderMap,
    require_initialized: bool,
) -> std::result::Result<String, Response> {
    let Some(session_id) = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json_rpc_error(
                None,
                -32600,
                "Missing MCP-Session-Id header",
                None,
            )),
        )
            .into_response());
    };

    let sessions = state.sessions.read().await;
    match sessions.get(session_id) {
        Some(session) => {
            if !SUPPORTED_VERSIONS.contains(&session.protocol_version.as_str()) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json_rpc_error(
                        None,
                        -32600,
                        "Session protocol version mismatch",
                        None,
                    )),
                )
                    .into_response());
            }
            if require_initialized && !session.initialized {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json_rpc_error(
                        None,
                        -32600,
                        "MCP session is not initialized",
                        None,
                    )),
                )
                    .into_response());
            }
            Ok(session_id.to_string())
        }
        None => Err((
            StatusCode::BAD_REQUEST,
            Json(json_rpc_error(
                None,
                -32600,
                "Unknown MCP session. Re-initialize.",
                None,
            )),
        )
            .into_response()),
    }
}

fn validate_origin(
    headers: &HeaderMap,
    config: &McpConfig,
) -> std::result::Result<(), ResponseError> {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };

    // When no allowed_origins are configured, reject cross-origin requests
    // rather than allowing all origins (secure by default)
    if config.allowed_origins.is_empty() {
        return Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                Json(json_rpc_error(
                    None,
                    -32600,
                    "Cross-origin requests require allowed_origins to be configured",
                    None,
                )),
            )
                .into_response(),
        ));
    }

    let allowed = config
        .allowed_origins
        .iter()
        .any(|candidate| candidate == "*" || candidate.eq_ignore_ascii_case(origin));
    if allowed {
        return Ok(());
    }

    Err(Box::new(
        (
            StatusCode::FORBIDDEN,
            Json(json_rpc_error(None, -32600, "Invalid Origin header", None)),
        )
            .into_response(),
    ))
}

fn enforce_protocol_header(
    config: &McpConfig,
    headers: &HeaderMap,
) -> std::result::Result<(), ResponseError> {
    if !config.require_protocol_version_header {
        return Ok(());
    }

    let Some(version) = headers
        .get(MCP_PROTOCOL_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(json_rpc_error(
                    None,
                    -32600,
                    "Missing MCP-Protocol-Version header",
                    None,
                )),
            )
                .into_response(),
        ));
    };

    if !SUPPORTED_VERSIONS.contains(&version) {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(json_rpc_error(
                    None,
                    -32600,
                    "Unsupported MCP-Protocol-Version",
                    Some(serde_json::json!({ "supported": SUPPORTED_VERSIONS })),
                )),
            )
                .into_response(),
        ));
    }

    Ok(())
}

use super::ResolvedClientIp;

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

fn json_rpc_success(id: Option<Value>, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

fn json_rpc_error(
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

fn set_header(response: &mut Response<Body>, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::from_str(value)) {
        response.headers_mut().insert(name, value);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::super::tracing::TracingState;
    use super::*;
    use axum::body::to_bytes;
    use forge_core::function::AuthContext;
    use forge_core::mcp::{ForgeMcpTool, McpToolAnnotations, McpToolInfo};
    use forge_core::schemars::{self, JsonSchema};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::future::Future;

    /// `McpConfig` is `#[non_exhaustive]`, so cross-crate functional-record-update
    /// is blocked. Build a default and mutate the field we care about.
    fn mcp_enabled() -> McpConfig {
        let mut c = McpConfig::default();
        c.enabled = true;
        c
    }
    use std::pin::Pin;

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
}

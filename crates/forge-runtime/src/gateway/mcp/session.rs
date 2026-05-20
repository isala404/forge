use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;
use forge_core::config::McpConfig;
use serde_json::Value;

use super::{SUPPORTED_VERSIONS, json_rpc_error, json_rpc_success, set_header, McpState};
use super::MCP_SESSION_HEADER;
use super::MCP_PROTOCOL_HEADER;

pub(super) type ResponseError = Box<Response>;

#[derive(Debug, Clone)]
pub(in super::super) struct McpSession {
    pub(super) initialized: bool,
    pub(super) protocol_version: String,
    pub(super) expires_at: Instant,
    pub(super) principal_id: Option<String>,
}

pub(super) async fn required_session_id(
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

pub(super) fn validate_origin(
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

pub(super) fn enforce_protocol_header(
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

pub(super) async fn handle_initialize(
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
                expires_at: Instant::now() + Duration::from_secs(state.config.session_ttl.as_secs()),
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

pub(super) async fn handle_notification(
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
                session.expires_at = Instant::now()
                    + Duration::from_secs(state.config.session_ttl.as_secs());
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

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;
use forge_core::function::{AuthContext, RequestMetadata};
use forge_core::mcp::McpToolContext;
use serde_json::Value;

use super::{DEFAULT_PAGE_SIZE, json_rpc_error, json_rpc_success, McpState};
use crate::function::FunctionRouter;

/// Handle `tools/list` — unauthenticated by MCP protocol design.
///
/// MCP clients call this to discover available tools before establishing an
/// authenticated session. Tool *execution* (tools/call) requires auth; only
/// discovery is open.
pub(super) fn handle_tools_list(
    state: &Arc<McpState>,
    id: Option<Value>,
    params: &Value,
) -> Response {
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

pub(super) async fn handle_tools_call(
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
pub(super) async fn handle_proxied_function_call(
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

pub(super) fn tool_success_result(output: Value) -> Value {
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

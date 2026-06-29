use async_graphql::Data;
use async_graphql::http::ALL_WEBSOCKET_PROTOCOLS;
use async_graphql_axum::{GraphQLProtocol, GraphQLRequest, GraphQLResponse, GraphQLWebSocket};
use axum::Router;
use axum::extract::{FromRequest, FromRequestParts, Request, State, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::context::{AppCtx, Ctx, CurrentUser};
use crate::gql::AppSchema;

#[derive(Clone)]
pub struct AppState {
    pub schema: AppSchema,
    pub ctx: Ctx,
}

fn strip_bearer(raw: &str) -> &str {
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim()
}

/// Resolve a bearer token to a principal: a live session (slides idle) or an API key.
async fn principal(ctx: &AppCtx, token: &str) -> Option<CurrentUser> {
    if let Ok(Some(session)) = ctx.forge.auth().validate_session(token).await
        && let Ok(uid) = Uuid::parse_str(&session.user_id)
    {
        return Some(CurrentUser {
            id: uid,
            token: token.to_string(),
        });
    }
    if let Ok(Some(info)) = ctx.forge.auth().verify_api_key(token).await
        && let Ok(uid) = Uuid::parse_str(&info.owner_id)
    {
        return Some(CurrentUser {
            id: uid,
            token: String::new(),
        });
    }
    None
}

async fn from_header(ctx: &AppCtx, headers: &HeaderMap) -> Option<CurrentUser> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    principal(ctx, strip_bearer(raw)).await
}

async fn graphql_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let mut request = req.into_inner();
    if let Some(user) = from_header(&st.ctx, &headers).await {
        request = request.data(user);
    }
    st.schema.execute(request).await.into()
}

/// A GET to `/graphql` is either a graphql-transport-ws upgrade (subscriptions, carries an
/// `Upgrade` header) or a plain query; urql sends queries over GET. Dispatch on the header
/// so both share the one endpoint, exactly as the Node and Python backends do.
async fn graphql_get(State(st): State<AppState>, req: Request) -> Response {
    if req.headers().contains_key(header::UPGRADE) {
        let (mut parts, _) = req.into_parts();
        let protocol = match GraphQLProtocol::from_request_parts(&mut parts, &()).await {
            Ok(p) => p,
            Err(rej) => return rej.into_response(),
        };
        let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(u) => u,
            Err(rej) => return rej.into_response(),
        };
        return graphql_ws(State(st), protocol, upgrade).await;
    }
    let headers = req.headers().clone();
    match GraphQLRequest::from_request(req, &()).await {
        Ok(gql) => graphql_handler(State(st), headers, gql)
            .await
            .into_response(),
        Err(rej) => rej.into_response(),
    }
}

async fn graphql_ws(
    State(st): State<AppState>,
    protocol: GraphQLProtocol,
    upgrade: WebSocketUpgrade,
) -> Response {
    let schema = st.schema.clone();
    let ctx = st.ctx.clone();
    upgrade
        .protocols(ALL_WEBSOCKET_PROTOCOLS)
        .on_upgrade(move |stream| async move {
            GraphQLWebSocket::new(stream, schema, protocol)
                .on_connection_init(move |payload: Value| async move {
                    let mut data = Data::default();
                    // Absent token => anonymous socket (unchanged). A *provided* token
                    // that doesn't resolve is rejected rather than silently downgraded.
                    if let Some(token) = payload
                        .get("authorization")
                        .and_then(Value::as_str)
                        .map(strip_bearer)
                        .filter(|t| !t.is_empty())
                    {
                        let user = principal(&ctx, token)
                            .await
                            .ok_or_else(|| async_graphql::Error::new("invalid token"))?;
                        data.insert(user);
                    }
                    Ok(data)
                })
                .serve()
                .await
        })
}

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    Router::new()
        .route("/graphql", post(graphql_handler).get(graphql_get))
        .route("/healthz", get(|| async { "ok" }))
        .layer(cors)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::strip_bearer;

    #[test]
    fn strips_bearer_prefix_either_case() {
        assert_eq!(strip_bearer("Bearer abc"), "abc");
        assert_eq!(strip_bearer("bearer abc"), "abc");
        assert_eq!(strip_bearer("abc"), "abc");
    }
}

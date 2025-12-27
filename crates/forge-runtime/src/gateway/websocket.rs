use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::{mpsc, RwLock};

use forge_core::cluster::NodeId;
use forge_core::realtime::SessionId;

use crate::realtime::{Reactor, WebSocketMessage as ReactorMessage};

/// WebSocket connection state shared across the gateway.
#[derive(Clone)]
pub struct WsState {
    pub reactor: Arc<Reactor>,
    pub db_pool: PgPool,
    pub node_id: NodeId,
}

impl WsState {
    pub fn new(reactor: Arc<Reactor>, db_pool: PgPool, node_id: NodeId) -> Self {
        Self { reactor, db_pool, node_id }
    }
}

/// Incoming WebSocket message from client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Subscribe to a query.
    Subscribe {
        id: String,
        #[serde(rename = "function")]
        function_name: String,
        args: Option<serde_json::Value>,
    },
    /// Unsubscribe from a subscription.
    Unsubscribe { id: String },
    /// Ping for keepalive.
    Ping,
    /// Authentication.
    Auth {
        #[allow(dead_code)]
        token: String,
    },
}

/// Outgoing WebSocket message to client.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Connection established.
    Connected,
    /// Ping response.
    Pong,
    /// Subscription data.
    Data { id: String, data: serde_json::Value },
    /// Subscription error.
    Error {
        id: Option<String>,
        code: String,
        message: String,
    },
    /// Subscription response (success/failure).
    #[allow(dead_code)]
    Subscribed { id: String },
    /// Unsubscribed confirmation.
    #[allow(dead_code)]
    Unsubscribed { id: String },
}

/// WebSocket upgrade handler.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WsState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle a WebSocket connection.
async fn handle_socket(socket: WebSocket, state: Arc<WsState>) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Create a session for this connection
    let session_id = SessionId::new();
    let session_uuid = session_id.0;
    let node_uuid = state.node_id.0;

    // Insert session into database for tracking
    let _ = sqlx::query(
        r#"
        INSERT INTO forge_sessions (id, node_id, status, connected_at, last_activity)
        VALUES ($1, $2, 'connected', NOW(), NOW())
        ON CONFLICT (id) DO UPDATE SET status = 'connected', last_activity = NOW()
        "#
    )
    .bind(session_uuid)
    .bind(node_uuid)
    .execute(&state.db_pool)
    .await;

    // Create channels for reactor -> websocket communication
    let (reactor_tx, mut reactor_rx) = mpsc::channel::<ReactorMessage>(256);

    // Register session with reactor
    state.reactor.register_session(session_id, reactor_tx).await;

    // Track client subscription IDs to internal subscription IDs
    #[allow(clippy::type_complexity)]
    let client_to_internal: Arc<RwLock<HashMap<String, forge_core::realtime::SubscriptionId>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let internal_to_client: Arc<RwLock<HashMap<forge_core::realtime::SubscriptionId, String>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Send connected message
    let connected = ServerMessage::Connected;
    if let Ok(json) = serde_json::to_string(&connected) {
        let _ = ws_sender.send(Message::Text(json.into())).await;
    }

    tracing::debug!(?session_id, "WebSocket connection established");

    // Clone state for the reactor message handler
    let internal_to_client_clone = internal_to_client.clone();

    // Spawn task to forward reactor messages to WebSocket
    let sender_handle = tokio::spawn(async move {
        while let Some(msg) = reactor_rx.recv().await {
            let server_msg = match msg {
                ReactorMessage::Data {
                    subscription_id,
                    data,
                } => {
                    // Map internal subscription ID back to client ID
                    let client_id = {
                        let map = internal_to_client_clone.read().await;
                        map.get(&subscription_id).cloned()
                    };

                    if let Some(id) = client_id {
                        ServerMessage::Data { id, data }
                    } else {
                        continue;
                    }
                }
                ReactorMessage::DeltaUpdate {
                    subscription_id,
                    delta,
                } => {
                    // Map internal subscription ID back to client ID
                    let client_id = {
                        let map = internal_to_client_clone.read().await;
                        map.get(&subscription_id).cloned()
                    };

                    if let Some(id) = client_id {
                        // Convert delta to data update
                        ServerMessage::Data {
                            id,
                            data: serde_json::json!({
                                "delta": {
                                    "added": delta.added,
                                    "removed": delta.removed,
                                    "updated": delta.updated
                                }
                            }),
                        }
                    } else {
                        continue;
                    }
                }
                ReactorMessage::Error { code, message } => ServerMessage::Error {
                    id: None,
                    code,
                    message,
                },
                ReactorMessage::Ping => ServerMessage::Pong,
                ReactorMessage::Pong => continue,
                _ => continue,
            };

            if let Ok(json) = serde_json::to_string(&server_msg) {
                if ws_sender.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Handle incoming messages from client
    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(data)) => {
                // Note: Can't send directly since sender is moved, but axum handles pings
                let _ = data;
                continue;
            }
            _ => continue,
        };

        // Parse client message
        let client_msg: ClientMessage = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Failed to parse client message: {}", e);
                continue;
            }
        };

        match client_msg {
            ClientMessage::Ping => {
                // Pong is handled by the reactor message sender
            }
            ClientMessage::Auth { token: _ } => {
                // TODO: Validate token and set auth context
            }
            ClientMessage::Subscribe {
                id,
                function_name,
                args,
            } => {
                let normalized_args = args.unwrap_or(serde_json::Value::Null);

                // Subscribe through reactor
                match state
                    .reactor
                    .subscribe(session_id, id.clone(), function_name, normalized_args)
                    .await
                {
                    Ok((subscription_id, data)) => {
                        // Track the mapping
                        {
                            let mut map = client_to_internal.write().await;
                            map.insert(id.clone(), subscription_id);
                        }
                        {
                            let mut map = internal_to_client.write().await;
                            map.insert(subscription_id, id.clone());
                        }

                        // Send subscribed confirmation + initial data
                        // Note: These need to go through the reactor channel
                        // For now, we'll store the messages to send
                        tracing::debug!(?subscription_id, client_id = %id, "Subscription created");

                        // The reactor will send the initial data through the reactor_tx channel
                        // But we need to send the subscribed confirmation first
                        // This is a bit awkward - let's inject directly

                        // Actually, the data is returned from subscribe, so we should send it
                        // The sender_handle has ws_sender, so we can't send from here directly
                        // Let's use the reactor channel to send these messages

                        let _ = state
                            .reactor
                            .ws_server()
                            .send_to_session(
                                session_id,
                                ReactorMessage::Data {
                                    subscription_id,
                                    data,
                                },
                            )
                            .await;
                    }
                    Err(e) => {
                        let _ = state
                            .reactor
                            .ws_server()
                            .send_to_session(
                                session_id,
                                ReactorMessage::Error {
                                    code: "SUBSCRIBE_ERROR".to_string(),
                                    message: e.to_string(),
                                },
                            )
                            .await;
                    }
                }
            }
            ClientMessage::Unsubscribe { id } => {
                // Look up internal subscription ID
                let subscription_id = {
                    let map = client_to_internal.read().await;
                    map.get(&id).copied()
                };

                if let Some(sub_id) = subscription_id {
                    state.reactor.unsubscribe(sub_id).await;

                    // Clean up mappings
                    {
                        let mut map = client_to_internal.write().await;
                        map.remove(&id);
                    }
                    {
                        let mut map = internal_to_client.write().await;
                        map.remove(&sub_id);
                    }

                    tracing::debug!(?sub_id, client_id = %id, "Subscription removed");
                }
            }
        }
    }

    // Clean up on disconnect
    sender_handle.abort();
    state.reactor.remove_session(session_id).await;

    // Remove session from database
    let _ = sqlx::query("DELETE FROM forge_sessions WHERE id = $1")
        .bind(session_uuid)
        .execute(&state.db_pool)
        .await;

    tracing::debug!(?session_id, "WebSocket connection closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_message_parsing() {
        let json = r#"{"type":"ping"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Ping));
    }

    #[test]
    fn test_subscribe_message_parsing() {
        let json = r#"{"type":"subscribe","id":"sub1","function":"get_users","args":null}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Subscribe { .. }));
    }

    #[test]
    fn test_server_message_serialization() {
        let msg = ServerMessage::Connected;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("connected"));
    }
}

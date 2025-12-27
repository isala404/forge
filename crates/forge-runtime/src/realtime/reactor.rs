use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, RwLock};

use forge_core::cluster::NodeId;
use forge_core::realtime::{Change, ReadSet, SessionId, SubscriptionId};

use super::invalidation::{InvalidationConfig, InvalidationEngine};
use super::listener::{ChangeListener, ListenerConfig};
use super::manager::SubscriptionManager;
use super::websocket::{WebSocketConfig, WebSocketMessage, WebSocketServer};
use crate::function::{FunctionEntry, FunctionRegistry};

/// Reactor configuration.
#[derive(Debug, Clone, Default)]
pub struct ReactorConfig {
    pub listener: ListenerConfig,
    pub invalidation: InvalidationConfig,
    pub websocket: WebSocketConfig,
}

/// Active subscription with execution context.
#[derive(Debug, Clone)]
pub struct ActiveSubscription {
    #[allow(dead_code)]
    pub subscription_id: SubscriptionId,
    pub session_id: SessionId,
    #[allow(dead_code)]
    pub client_sub_id: String,
    pub query_name: String,
    pub args: serde_json::Value,
    pub last_result_hash: Option<String>,
    #[allow(dead_code)]
    pub read_set: ReadSet,
}

/// The Reactor orchestrates real-time reactivity.
/// It connects: ChangeListener -> InvalidationEngine -> Query Re-execution -> WebSocket Push
pub struct Reactor {
    #[allow(dead_code)]
    node_id: NodeId,
    db_pool: sqlx::PgPool,
    registry: FunctionRegistry,
    subscription_manager: Arc<SubscriptionManager>,
    ws_server: Arc<WebSocketServer>,
    change_listener: Arc<ChangeListener>,
    invalidation_engine: Arc<InvalidationEngine>,
    /// Active subscriptions with their execution context.
    active_subscriptions: Arc<RwLock<HashMap<SubscriptionId, ActiveSubscription>>>,
    /// Shutdown signal.
    shutdown_tx: broadcast::Sender<()>,
}

impl Reactor {
    /// Create a new reactor.
    pub fn new(
        node_id: NodeId,
        db_pool: sqlx::PgPool,
        registry: FunctionRegistry,
        config: ReactorConfig,
    ) -> Self {
        let subscription_manager = Arc::new(SubscriptionManager::new(
            config.websocket.max_subscriptions_per_connection,
        ));
        let ws_server = Arc::new(WebSocketServer::new(node_id, config.websocket));
        let change_listener = Arc::new(ChangeListener::new(db_pool.clone(), config.listener));
        let invalidation_engine = Arc::new(InvalidationEngine::new(
            subscription_manager.clone(),
            config.invalidation,
        ));
        let (shutdown_tx, _) = broadcast::channel(1);

        Self {
            node_id,
            db_pool,
            registry,
            subscription_manager,
            ws_server,
            change_listener,
            invalidation_engine,
            active_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx,
        }
    }

    /// Get the node ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the WebSocket server reference.
    pub fn ws_server(&self) -> Arc<WebSocketServer> {
        self.ws_server.clone()
    }

    /// Get the subscription manager reference.
    pub fn subscription_manager(&self) -> Arc<SubscriptionManager> {
        self.subscription_manager.clone()
    }

    /// Get a shutdown receiver.
    pub fn shutdown_receiver(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Register a new WebSocket session.
    pub async fn register_session(
        &self,
        session_id: SessionId,
        sender: mpsc::Sender<WebSocketMessage>,
    ) {
        self.ws_server.register_connection(session_id, sender).await;
        tracing::debug!(?session_id, "Session registered with reactor");
    }

    /// Remove a session and all its subscriptions.
    pub async fn remove_session(&self, session_id: SessionId) {
        if let Some(subscription_ids) = self.ws_server.remove_connection(session_id).await {
            // Clean up subscriptions
            for sub_id in subscription_ids {
                self.subscription_manager.remove_subscription(sub_id).await;
                self.active_subscriptions.write().await.remove(&sub_id);
            }
        }
        tracing::debug!(?session_id, "Session removed from reactor");
    }

    /// Subscribe to a query.
    pub async fn subscribe(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        query_name: String,
        args: serde_json::Value,
    ) -> forge_core::Result<(SubscriptionId, serde_json::Value)> {
        // Create subscription in manager
        let sub_info = self
            .subscription_manager
            .create_subscription(session_id, &query_name, args.clone())
            .await?;

        let subscription_id = sub_info.id;

        // Add to WebSocket server
        self.ws_server
            .add_subscription(session_id, subscription_id)
            .await?;

        // Execute the query to get initial data
        let (data, read_set) = self.execute_query(&query_name, &args).await?;

        // Compute result hash for delta detection
        let result_hash = Self::compute_hash(&data);

        // Update subscription with read set
        let tables: Vec<_> = read_set.tables.iter().collect();
        tracing::debug!(
            ?subscription_id,
            query_name = %query_name,
            read_set_tables = ?tables,
            "Updating subscription with read set"
        );

        self.subscription_manager
            .update_subscription(subscription_id, read_set.clone(), result_hash.clone())
            .await;

        // Store active subscription
        let active = ActiveSubscription {
            subscription_id,
            session_id,
            client_sub_id,
            query_name,
            args,
            last_result_hash: Some(result_hash),
            read_set,
        };
        self.active_subscriptions
            .write()
            .await
            .insert(subscription_id, active);

        tracing::debug!(?subscription_id, "Subscription created");

        Ok((subscription_id, data))
    }

    /// Unsubscribe from a query.
    pub async fn unsubscribe(&self, subscription_id: SubscriptionId) {
        self.ws_server.remove_subscription(subscription_id).await;
        self.subscription_manager
            .remove_subscription(subscription_id)
            .await;
        self.active_subscriptions
            .write()
            .await
            .remove(&subscription_id);
        tracing::debug!(?subscription_id, "Subscription removed");
    }

    /// Execute a query and return data with read set.
    async fn execute_query(
        &self,
        query_name: &str,
        args: &serde_json::Value,
    ) -> forge_core::Result<(serde_json::Value, ReadSet)> {
        match self.registry.get(query_name) {
            Some(FunctionEntry::Query { handler, .. }) => {
                let ctx = forge_core::function::QueryContext::new(
                    self.db_pool.clone(),
                    forge_core::function::AuthContext::unauthenticated(),
                    forge_core::function::RequestMetadata::new(),
                );

                // Normalize args
                let normalized_args = match args {
                    v if v.is_object() && v.as_object().unwrap().is_empty() => {
                        serde_json::Value::Null
                    }
                    v => v.clone(),
                };

                let data = handler(&ctx, normalized_args).await?;

                // Create a read set based on the query name
                // For queries like "get_users", track the "users" table
                let mut read_set = ReadSet::new();
                let table_name = Self::extract_table_name(query_name);
                read_set.add_table(&table_name);

                Ok((data, read_set))
            }
            Some(_) => Err(forge_core::ForgeError::Validation(format!(
                "'{}' is not a query",
                query_name
            ))),
            None => Err(forge_core::ForgeError::Validation(format!(
                "Query '{}' not found",
                query_name
            ))),
        }
    }

    /// Compute a hash of the result for delta detection.
    fn compute_hash(data: &serde_json::Value) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let json = serde_json::to_string(data).unwrap_or_default();
        let mut hasher = DefaultHasher::new();
        json.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// Start the reactor (runs the change listener and invalidation loop).
    pub async fn start(&self) -> forge_core::Result<()> {
        let listener = self.change_listener.clone();
        let invalidation_engine = self.invalidation_engine.clone();
        let active_subscriptions = self.active_subscriptions.clone();
        let ws_server = self.ws_server.clone();
        let registry = self.registry.clone();
        let db_pool = self.db_pool.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        // Spawn change listener task
        let listener_clone = listener.clone();
        let listener_handle = tokio::spawn(async move {
            if let Err(e) = listener_clone.run().await {
                tracing::error!("Change listener error: {}", e);
            }
        });

        // Subscribe to changes
        let mut change_rx = listener.subscribe();

        // Main reactor loop
        tokio::spawn(async move {
            tracing::info!("Reactor started, listening for changes");

            loop {
                tokio::select! {
                    // Process incoming changes
                    result = change_rx.recv() => {
                        match result {
                            Ok(change) => {
                                Self::handle_change(
                                    &change,
                                    &invalidation_engine,
                                    &active_subscriptions,
                                    &ws_server,
                                    &registry,
                                    &db_pool,
                                ).await;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Reactor lagged by {} messages", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                tracing::info!("Change channel closed");
                                break;
                            }
                        }
                    }
                    // Handle shutdown
                    _ = shutdown_rx.recv() => {
                        tracing::info!("Reactor shutting down");
                        break;
                    }
                }
            }

            listener_handle.abort();
        });

        Ok(())
    }

    /// Handle a database change event.
    async fn handle_change(
        change: &Change,
        invalidation_engine: &Arc<InvalidationEngine>,
        active_subscriptions: &Arc<RwLock<HashMap<SubscriptionId, ActiveSubscription>>>,
        ws_server: &Arc<WebSocketServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
    ) {
        tracing::debug!(table = %change.table, op = ?change.operation, "Processing change");

        // Process change through invalidation engine
        invalidation_engine.process_change(change.clone()).await;

        // Flush all pending invalidations immediately for real-time updates
        // Note: A more sophisticated approach would use the invalidation engine's run loop
        // with proper debouncing for high-frequency changes
        let invalidated = invalidation_engine.flush_all().await;

        if invalidated.is_empty() {
            return;
        }

        tracing::debug!(count = invalidated.len(), "Invalidating subscriptions");

        // Re-execute invalidated queries and push updates
        let subscriptions = active_subscriptions.read().await;

        for sub_id in invalidated {
            if let Some(active) = subscriptions.get(&sub_id) {
                // Re-execute the query
                match Self::execute_query_static(
                    registry,
                    db_pool,
                    &active.query_name,
                    &active.args,
                )
                .await
                {
                    Ok((new_data, _read_set)) => {
                        let new_hash = Self::compute_hash(&new_data);

                        // Only push if data changed
                        if active.last_result_hash.as_ref() != Some(&new_hash) {
                            // Send updated data to client
                            let message = WebSocketMessage::Data {
                                subscription_id: sub_id,
                                data: new_data,
                            };

                            if let Err(e) =
                                ws_server.send_to_session(active.session_id, message).await
                            {
                                tracing::warn!(?sub_id, "Failed to send update: {}", e);
                            } else {
                                tracing::debug!(?sub_id, "Pushed update to client");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(?sub_id, "Failed to re-execute query: {}", e);
                    }
                }
            }
        }
    }

    /// Static version of execute_query for use in async context.
    async fn execute_query_static(
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        query_name: &str,
        args: &serde_json::Value,
    ) -> forge_core::Result<(serde_json::Value, ReadSet)> {
        match registry.get(query_name) {
            Some(FunctionEntry::Query { handler, .. }) => {
                let ctx = forge_core::function::QueryContext::new(
                    db_pool.clone(),
                    forge_core::function::AuthContext::unauthenticated(),
                    forge_core::function::RequestMetadata::new(),
                );

                let normalized_args = match args {
                    v if v.is_object() && v.as_object().unwrap().is_empty() => {
                        serde_json::Value::Null
                    }
                    v => v.clone(),
                };

                let data = handler(&ctx, normalized_args).await?;

                // Create a read set based on the query name
                let mut read_set = ReadSet::new();
                let table_name = Self::extract_table_name(query_name);
                read_set.add_table(&table_name);

                Ok((data, read_set))
            }
            _ => Err(forge_core::ForgeError::Validation(format!(
                "Query '{}' not found or not a query",
                query_name
            ))),
        }
    }

    /// Extract table name from query name using common patterns.
    fn extract_table_name(query_name: &str) -> String {
        if let Some(rest) = query_name.strip_prefix("get_") {
            rest.to_string()
        } else if let Some(rest) = query_name.strip_prefix("list_") {
            rest.to_string()
        } else if let Some(rest) = query_name.strip_prefix("find_") {
            rest.to_string()
        } else if let Some(rest) = query_name.strip_prefix("fetch_") {
            rest.to_string()
        } else {
            query_name.to_string()
        }
    }

    /// Stop the reactor.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(());
        self.change_listener.stop();
    }

    /// Get reactor statistics.
    pub async fn stats(&self) -> ReactorStats {
        let ws_stats = self.ws_server.stats().await;
        let inv_stats = self.invalidation_engine.stats().await;

        ReactorStats {
            connections: ws_stats.connections,
            subscriptions: ws_stats.subscriptions,
            pending_invalidations: inv_stats.pending_subscriptions,
            listener_running: self.change_listener.is_running(),
        }
    }
}

/// Reactor statistics.
#[derive(Debug, Clone)]
pub struct ReactorStats {
    pub connections: usize,
    pub subscriptions: usize,
    pub pending_invalidations: usize,
    pub listener_running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reactor_config_default() {
        let config = ReactorConfig::default();
        assert_eq!(config.listener.channel, "forge_changes");
        assert_eq!(config.invalidation.debounce_ms, 50);
    }

    #[test]
    fn test_compute_hash() {
        let data1 = serde_json::json!({"name": "test"});
        let data2 = serde_json::json!({"name": "test"});
        let data3 = serde_json::json!({"name": "different"});

        let hash1 = Reactor::compute_hash(&data1);
        let hash2 = Reactor::compute_hash(&data2);
        let hash3 = Reactor::compute_hash(&data3);

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }
}

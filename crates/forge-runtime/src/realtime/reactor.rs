use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, RwLock};
use uuid::Uuid;

use forge_core::cluster::NodeId;
use forge_core::realtime::{Change, ReadSet, SessionId, SubscriptionId};

use super::invalidation::{InvalidationConfig, InvalidationEngine};
use super::listener::{ChangeListener, ListenerConfig};
use super::manager::SubscriptionManager;
use super::websocket::{WebSocketConfig, WebSocketMessage, WebSocketServer};
use crate::function::{FunctionEntry, FunctionRegistry};
use crate::gateway::websocket::{JobData, WorkflowData, WorkflowStepData};

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

/// Job subscription tracking.
#[derive(Debug, Clone)]
pub struct JobSubscription {
    #[allow(dead_code)]
    pub subscription_id: SubscriptionId,
    pub session_id: SessionId,
    pub client_sub_id: String,
    #[allow(dead_code)]
    pub job_id: Uuid, // Validated UUID, not String
}

/// Workflow subscription tracking.
#[derive(Debug, Clone)]
pub struct WorkflowSubscription {
    #[allow(dead_code)]
    pub subscription_id: SubscriptionId,
    pub session_id: SessionId,
    pub client_sub_id: String,
    #[allow(dead_code)]
    pub workflow_id: Uuid, // Validated UUID, not String
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
    /// Job subscriptions: job_id -> list of subscribers.
    job_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
    /// Workflow subscriptions: workflow_id -> list of subscribers.
    workflow_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
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
            job_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            workflow_subscriptions: Arc::new(RwLock::new(HashMap::new())),
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
            // Clean up query subscriptions
            for sub_id in subscription_ids {
                self.subscription_manager.remove_subscription(sub_id).await;
                self.active_subscriptions.write().await.remove(&sub_id);
            }
        }

        // Clean up job subscriptions for this session
        {
            let mut job_subs = self.job_subscriptions.write().await;
            for subscribers in job_subs.values_mut() {
                subscribers.retain(|s| s.session_id != session_id);
            }
            // Remove empty entries
            job_subs.retain(|_, v| !v.is_empty());
        }

        // Clean up workflow subscriptions for this session
        {
            let mut workflow_subs = self.workflow_subscriptions.write().await;
            for subscribers in workflow_subs.values_mut() {
                subscribers.retain(|s| s.session_id != session_id);
            }
            // Remove empty entries
            workflow_subs.retain(|_, v| !v.is_empty());
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

    /// Subscribe to job progress updates.
    pub async fn subscribe_job(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        job_id: Uuid, // Pre-validated UUID
    ) -> forge_core::Result<JobData> {
        let subscription_id = SubscriptionId::new();

        // Fetch current job state from database
        let job_data = self.fetch_job_data(job_id).await?;

        // Register subscription
        let subscription = JobSubscription {
            subscription_id,
            session_id,
            client_sub_id: client_sub_id.clone(),
            job_id,
        };

        let mut subs = self.job_subscriptions.write().await;
        subs.entry(job_id).or_default().push(subscription);

        tracing::debug!(
            ?subscription_id,
            client_id = %client_sub_id,
            %job_id,
            "Job subscription created"
        );

        Ok(job_data)
    }

    /// Unsubscribe from job updates.
    pub async fn unsubscribe_job(&self, session_id: SessionId, client_sub_id: &str) {
        let mut subs = self.job_subscriptions.write().await;

        // Find and remove the subscription
        for subscribers in subs.values_mut() {
            subscribers
                .retain(|s| !(s.session_id == session_id && s.client_sub_id == client_sub_id));
        }

        // Remove empty entries
        subs.retain(|_, v| !v.is_empty());

        tracing::debug!(client_id = %client_sub_id, "Job subscription removed");
    }

    /// Subscribe to workflow progress updates.
    pub async fn subscribe_workflow(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        workflow_id: Uuid, // Pre-validated UUID
    ) -> forge_core::Result<WorkflowData> {
        let subscription_id = SubscriptionId::new();

        // Fetch current workflow + steps from database
        let workflow_data = self.fetch_workflow_data(workflow_id).await?;

        // Register subscription
        let subscription = WorkflowSubscription {
            subscription_id,
            session_id,
            client_sub_id: client_sub_id.clone(),
            workflow_id,
        };

        let mut subs = self.workflow_subscriptions.write().await;
        subs.entry(workflow_id).or_default().push(subscription);

        tracing::debug!(
            ?subscription_id,
            client_id = %client_sub_id,
            %workflow_id,
            "Workflow subscription created"
        );

        Ok(workflow_data)
    }

    /// Unsubscribe from workflow updates.
    pub async fn unsubscribe_workflow(&self, session_id: SessionId, client_sub_id: &str) {
        let mut subs = self.workflow_subscriptions.write().await;

        // Find and remove the subscription
        for subscribers in subs.values_mut() {
            subscribers
                .retain(|s| !(s.session_id == session_id && s.client_sub_id == client_sub_id));
        }

        // Remove empty entries
        subs.retain(|_, v| !v.is_empty());

        tracing::debug!(client_id = %client_sub_id, "Workflow subscription removed");
    }

    /// Fetch current job data from database.
    #[allow(clippy::type_complexity)]
    async fn fetch_job_data(&self, job_id: Uuid) -> forge_core::Result<JobData> {
        let row: Option<(
            String,
            Option<i32>,
            Option<String>,
            Option<serde_json::Value>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
                SELECT status, progress_percent, progress_message, output, last_error
                FROM forge_jobs WHERE id = $1
                "#,
        )
        .bind(job_id)
        .fetch_optional(&self.db_pool)
        .await
        .map_err(forge_core::ForgeError::Sql)?;

        match row {
            Some((status, progress_percent, progress_message, output, error)) => Ok(JobData {
                job_id: job_id.to_string(),
                status,
                progress_percent,
                progress_message,
                output,
                error,
            }),
            None => Err(forge_core::ForgeError::NotFound(format!(
                "Job {} not found",
                job_id
            ))),
        }
    }

    /// Fetch current workflow + steps from database.
    #[allow(clippy::type_complexity)]
    async fn fetch_workflow_data(&self, workflow_id: Uuid) -> forge_core::Result<WorkflowData> {
        // Fetch workflow run
        let row: Option<(
            String,
            Option<String>,
            Option<serde_json::Value>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
                SELECT status, current_step, output, error
                FROM forge_workflow_runs WHERE id = $1
                "#,
        )
        .bind(workflow_id)
        .fetch_optional(&self.db_pool)
        .await
        .map_err(forge_core::ForgeError::Sql)?;

        let (status, current_step, output, error) = match row {
            Some(r) => r,
            None => {
                return Err(forge_core::ForgeError::NotFound(format!(
                    "Workflow {} not found",
                    workflow_id
                )));
            }
        };

        // Fetch workflow steps
        let step_rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            r#"
            SELECT step_name, status, error
            FROM forge_workflow_steps
            WHERE workflow_run_id = $1
            ORDER BY started_at ASC NULLS LAST
            "#,
        )
        .bind(workflow_id)
        .fetch_all(&self.db_pool)
        .await
        .map_err(forge_core::ForgeError::Sql)?;

        let steps = step_rows
            .into_iter()
            .map(|(name, status, error)| WorkflowStepData {
                name,
                status,
                error,
            })
            .collect();

        Ok(WorkflowData {
            workflow_id: workflow_id.to_string(),
            status,
            current_step,
            steps,
            output,
            error,
        })
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
        let job_subscriptions = self.job_subscriptions.clone();
        let workflow_subscriptions = self.workflow_subscriptions.clone();
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
                                    &job_subscriptions,
                                    &workflow_subscriptions,
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
    #[allow(clippy::too_many_arguments)]
    async fn handle_change(
        change: &Change,
        invalidation_engine: &Arc<InvalidationEngine>,
        active_subscriptions: &Arc<RwLock<HashMap<SubscriptionId, ActiveSubscription>>>,
        job_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
        workflow_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
        ws_server: &Arc<WebSocketServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
    ) {
        tracing::debug!(table = %change.table, op = ?change.operation, row_id = ?change.row_id, "Processing change");

        // Handle job/workflow table changes first
        match change.table.as_str() {
            "forge_jobs" => {
                if let Some(job_id) = change.row_id {
                    Self::handle_job_change(job_id, job_subscriptions, ws_server, db_pool).await;
                }
                return; // Don't process through query invalidation
            }
            "forge_workflow_runs" => {
                if let Some(workflow_id) = change.row_id {
                    Self::handle_workflow_change(
                        workflow_id,
                        workflow_subscriptions,
                        ws_server,
                        db_pool,
                    )
                    .await;
                }
                return; // Don't process through query invalidation
            }
            "forge_workflow_steps" => {
                // For step changes, need to look up the parent workflow_id
                if let Some(step_id) = change.row_id {
                    Self::handle_workflow_step_change(
                        step_id,
                        workflow_subscriptions,
                        ws_server,
                        db_pool,
                    )
                    .await;
                }
                return; // Don't process through query invalidation
            }
            _ => {}
        }

        // Process change through invalidation engine for query subscriptions
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

    /// Handle a job table change event.
    async fn handle_job_change(
        job_id: Uuid,
        job_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
        ws_server: &Arc<WebSocketServer>,
        db_pool: &sqlx::PgPool,
    ) {
        let subs = job_subscriptions.read().await;
        let subscribers = match subs.get(&job_id) {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return, // No subscribers for this job
        };
        drop(subs); // Release lock before async operations

        // Fetch latest job state
        let job_data = match Self::fetch_job_data_static(job_id, db_pool).await {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!(%job_id, "Failed to fetch job data: {}", e);
                return;
            }
        };

        // Push to all subscribers
        for sub in subscribers {
            let message = WebSocketMessage::JobUpdate {
                client_sub_id: sub.client_sub_id.clone(),
                job: job_data.clone(),
            };

            if let Err(e) = ws_server.send_to_session(sub.session_id, message).await {
                // Debug level because this commonly happens when session disconnects (page refresh)
                tracing::debug!(
                    %job_id,
                    client_id = %sub.client_sub_id,
                    "Failed to send job update (session likely disconnected): {}",
                    e
                );
            } else {
                tracing::debug!(
                    %job_id,
                    client_id = %sub.client_sub_id,
                    "Pushed job update to client"
                );
            }
        }
    }

    /// Handle a workflow table change event.
    async fn handle_workflow_change(
        workflow_id: Uuid,
        workflow_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
        ws_server: &Arc<WebSocketServer>,
        db_pool: &sqlx::PgPool,
    ) {
        let subs = workflow_subscriptions.read().await;
        let subscribers = match subs.get(&workflow_id) {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return, // No subscribers for this workflow
        };
        drop(subs); // Release lock before async operations

        // Fetch latest workflow + steps state
        let workflow_data = match Self::fetch_workflow_data_static(workflow_id, db_pool).await {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!(%workflow_id, "Failed to fetch workflow data: {}", e);
                return;
            }
        };

        // Push to all subscribers
        for sub in subscribers {
            let message = WebSocketMessage::WorkflowUpdate {
                client_sub_id: sub.client_sub_id.clone(),
                workflow: workflow_data.clone(),
            };

            if let Err(e) = ws_server.send_to_session(sub.session_id, message).await {
                // Debug level because this commonly happens when session disconnects (page refresh)
                tracing::debug!(
                    %workflow_id,
                    client_id = %sub.client_sub_id,
                    "Failed to send workflow update (session likely disconnected): {}",
                    e
                );
            } else {
                tracing::debug!(
                    %workflow_id,
                    client_id = %sub.client_sub_id,
                    "Pushed workflow update to client"
                );
            }
        }
    }

    /// Handle a workflow step change event.
    async fn handle_workflow_step_change(
        step_id: Uuid,
        workflow_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
        ws_server: &Arc<WebSocketServer>,
        db_pool: &sqlx::PgPool,
    ) {
        // Look up the workflow_run_id for this step
        let workflow_id: Option<Uuid> =
            sqlx::query_scalar("SELECT workflow_run_id FROM forge_workflow_steps WHERE id = $1")
                .bind(step_id)
                .fetch_optional(db_pool)
                .await
                .ok()
                .flatten();

        if let Some(wf_id) = workflow_id {
            // Delegate to workflow change handler
            Self::handle_workflow_change(wf_id, workflow_subscriptions, ws_server, db_pool).await;
        }
    }

    /// Static version of fetch_job_data for use in handle_change.
    #[allow(clippy::type_complexity)]
    async fn fetch_job_data_static(
        job_id: Uuid,
        db_pool: &sqlx::PgPool,
    ) -> forge_core::Result<JobData> {
        let row: Option<(
            String,
            Option<i32>,
            Option<String>,
            Option<serde_json::Value>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
                SELECT status, progress_percent, progress_message, output, last_error
                FROM forge_jobs WHERE id = $1
                "#,
        )
        .bind(job_id)
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Sql)?;

        match row {
            Some((status, progress_percent, progress_message, output, error)) => Ok(JobData {
                job_id: job_id.to_string(),
                status,
                progress_percent,
                progress_message,
                output,
                error,
            }),
            None => Err(forge_core::ForgeError::NotFound(format!(
                "Job {} not found",
                job_id
            ))),
        }
    }

    /// Static version of fetch_workflow_data for use in handle_change.
    #[allow(clippy::type_complexity)]
    async fn fetch_workflow_data_static(
        workflow_id: Uuid,
        db_pool: &sqlx::PgPool,
    ) -> forge_core::Result<WorkflowData> {
        let row: Option<(
            String,
            Option<String>,
            Option<serde_json::Value>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
                SELECT status, current_step, output, error
                FROM forge_workflow_runs WHERE id = $1
                "#,
        )
        .bind(workflow_id)
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Sql)?;

        let (status, current_step, output, error) = match row {
            Some(r) => r,
            None => {
                return Err(forge_core::ForgeError::NotFound(format!(
                    "Workflow {} not found",
                    workflow_id
                )));
            }
        };

        let step_rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            r#"
            SELECT step_name, status, error
            FROM forge_workflow_steps
            WHERE workflow_run_id = $1
            ORDER BY started_at ASC NULLS LAST
            "#,
        )
        .bind(workflow_id)
        .fetch_all(db_pool)
        .await
        .map_err(forge_core::ForgeError::Sql)?;

        let steps = step_rows
            .into_iter()
            .map(|(name, status, error)| WorkflowStepData {
                name,
                status,
                error,
            })
            .collect();

        Ok(WorkflowData {
            workflow_id: workflow_id.to_string(),
            status,
            current_step,
            steps,
            output,
            error,
        })
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

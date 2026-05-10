use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::sync::{RwLock, Semaphore, broadcast, mpsc};
use uuid::Uuid;

use forge_core::cluster::NodeId;
use forge_core::realtime::{Change, ReadSet, SessionId, SubscriptionId};

use super::invalidation::{InvalidationConfig, InvalidationEngine};
use super::listener::{ChangeListener, ListenerConfig};
use super::manager::SubscriptionManager;
use super::message::{
    JobData, RealtimeConfig, RealtimeMessage, SessionServer, WorkflowData, WorkflowStepData,
};
use crate::function::{FunctionEntry, FunctionRegistry};

#[derive(Debug, Clone)]
pub struct ReactorConfig {
    pub listener: ListenerConfig,
    pub invalidation: InvalidationConfig,
    pub realtime: RealtimeConfig,
    pub max_listener_restarts: u32,
    /// Doubles with each attempt for exponential backoff
    pub listener_restart_delay_ms: u64,
    /// Maximum concurrent re-executions during flush.
    pub max_concurrent_reexecutions: usize,
    /// Session cleanup interval in seconds.
    pub session_cleanup_interval_secs: u64,
    /// Periodic resync sweep interval in seconds. PG silently drops
    /// `LISTEN/NOTIFY` payloads when listeners are slow, leaving subscribers
    /// stale. Re-evaluating every active group on this cadence recovers from
    /// dropped notifications. `0` disables the sweep.
    pub resync_interval_secs: u64,
    /// Number of DashMap shards for the subscription manager.
    pub shard_count: usize,
}

impl Default for ReactorConfig {
    fn default() -> Self {
        Self {
            listener: ListenerConfig::default(),
            invalidation: InvalidationConfig::default(),
            realtime: RealtimeConfig::default(),
            max_listener_restarts: 5,
            listener_restart_delay_ms: 1000,
            max_concurrent_reexecutions: 64,
            session_cleanup_interval_secs: 60,
            resync_interval_secs: 60,
            shard_count: 64,
        }
    }
}

/// Job subscription tracking.
/// The job_id is the HashMap key, not stored here.
#[derive(Debug, Clone)]
pub struct JobSubscription {
    pub session_id: SessionId,
    pub client_sub_id: String,
    pub auth_context: forge_core::function::AuthContext,
}

/// Workflow subscription tracking.
/// The workflow_id is the HashMap key, not stored here.
#[derive(Debug, Clone)]
pub struct WorkflowSubscription {
    pub session_id: SessionId,
    pub client_sub_id: String,
    pub auth_context: forge_core::function::AuthContext,
}

/// ChangeListener -> InvalidationEngine -> Group Re-execution -> SSE Fan-out
pub struct Reactor {
    node_id: NodeId,
    /// Pool for read operations (query re-execution, job/workflow fetches).
    /// Routes to replicas when configured, falls back to primary otherwise.
    /// The ChangeListener uses the primary pool (LISTEN requires it), passed
    /// at construction time.
    ///
    /// Replication lag tradeoff: a NOTIFY may arrive before the replica has
    /// the committed data. The debounce window (50ms+) and periodic resync
    /// sweep (60s) compensate. For deployments with high replication lag,
    /// disable read-from-replica in config to route all reads to primary.
    read_pool: sqlx::PgPool,
    registry: FunctionRegistry,
    subscription_manager: Arc<SubscriptionManager>,
    session_server: Arc<SessionServer>,
    change_listener: Arc<ChangeListener>,
    invalidation_engine: Arc<InvalidationEngine>,
    /// Job subscriptions: job_id -> list of subscribers.
    job_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
    /// Workflow subscriptions: workflow_id -> list of subscribers.
    workflow_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
    shutdown_tx: broadcast::Sender<()>,
    max_listener_restarts: u32,
    listener_restart_delay_ms: u64,
    max_concurrent_reexecutions: usize,
    session_cleanup_interval_secs: u64,
    resync_interval_secs: u64,
}

impl Reactor {
    /// Create a new reactor.
    pub fn new(
        node_id: NodeId,
        db_pool: sqlx::PgPool,
        read_pool: sqlx::PgPool,
        registry: FunctionRegistry,
        config: ReactorConfig,
    ) -> Self {
        let subscription_manager = Arc::new(SubscriptionManager::with_shard_count(
            config.realtime.max_subscriptions_per_session,
            config.shard_count,
        ));
        let session_server = Arc::new(SessionServer::new(node_id, config.realtime.clone()));
        let change_listener = Arc::new(ChangeListener::new(db_pool.clone(), config.listener));
        let invalidation_engine = Arc::new(InvalidationEngine::new(
            subscription_manager.clone(),
            config.invalidation,
        ));
        let (shutdown_tx, _) = broadcast::channel(1);

        Self {
            node_id,
            read_pool,
            registry,
            subscription_manager,
            session_server,
            change_listener,
            invalidation_engine,
            job_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            workflow_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx,
            max_listener_restarts: config.max_listener_restarts,
            listener_restart_delay_ms: config.listener_restart_delay_ms,
            max_concurrent_reexecutions: config.max_concurrent_reexecutions,
            session_cleanup_interval_secs: config.session_cleanup_interval_secs,
            resync_interval_secs: config.resync_interval_secs,
        }
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn session_server(&self) -> Arc<SessionServer> {
        self.session_server.clone()
    }

    pub fn subscription_manager(&self) -> Arc<SubscriptionManager> {
        self.subscription_manager.clone()
    }

    pub fn shutdown_receiver(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Register a new session.
    ///
    /// `token_exp` is the JWT `exp` claim (Unix timestamp) from the session's
    /// authentication token. Pass `None` for unauthenticated sessions.
    pub fn register_session(
        &self,
        session_id: SessionId,
        sender: mpsc::Sender<RealtimeMessage>,
        token_exp: Option<i64>,
    ) {
        self.session_server
            .register_connection(session_id, sender, token_exp);
        tracing::trace!(?session_id, "Session registered");
    }

    /// Remove a session and all its subscriptions.
    pub async fn remove_session(&self, session_id: SessionId) {
        // Clean up query subscriptions via the subscription manager
        self.subscription_manager
            .remove_session_subscriptions(session_id);

        // Clean up session server
        self.session_server.remove_connection(session_id);

        // Clean up job subscriptions
        {
            let mut job_subs = self.job_subscriptions.write().await;
            for subscribers in job_subs.values_mut() {
                subscribers.retain(|s| s.session_id != session_id);
            }
            job_subs.retain(|_, v| !v.is_empty());
        }

        // Clean up workflow subscriptions
        {
            let mut workflow_subs = self.workflow_subscriptions.write().await;
            for subscribers in workflow_subs.values_mut() {
                subscribers.retain(|s| s.session_id != session_id);
            }
            workflow_subs.retain(|_, v| !v.is_empty());
        }

        tracing::trace!(?session_id, "Session removed");
    }

    /// Subscribe to a query. Uses query groups for coalescing.
    pub async fn subscribe(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        query_name: String,
        args: serde_json::Value,
        auth_context: forge_core::function::AuthContext,
    ) -> forge_core::Result<(SubscriptionId, serde_json::Value)> {
        // Look up function info for compile-time metadata
        let (table_deps, selected_cols) = match self.registry.get(&query_name) {
            Some(FunctionEntry::Query { info, .. }) => {
                (info.table_dependencies, info.selected_columns)
            }
            _ => (&[] as &[&str], &[] as &[&str]),
        };

        let (group_id, subscription_id, is_new_group) = self.subscription_manager.subscribe(
            session_id,
            client_sub_id,
            &query_name,
            &args,
            &auth_context,
            table_deps,
            selected_cols,
        )?;

        // Register subscription in session server for message routing
        if let Err(error) = self
            .session_server
            .add_subscription(session_id, subscription_id)
        {
            self.subscription_manager.unsubscribe(subscription_id);
            return Err(error);
        }

        // Only execute the query if this is a new group (no cached result yet)
        let data = if is_new_group {
            let (data, read_set) = match self.execute_query(&query_name, &args, &auth_context).await
            {
                Ok(result) => result,
                Err(error) => {
                    self.unsubscribe(subscription_id);
                    return Err(error);
                }
            };

            let result_hash = Self::compute_hash(&data);

            tracing::trace!(
                ?group_id,
                query = %query_name,
                "New query group created"
            );

            self.subscription_manager
                .update_group(group_id, read_set, result_hash);

            data
        } else {
            // Group exists, re-execute to get fresh data for this subscriber
            // (they might have joined mid-cycle)
            let (data, _) = match self.execute_query(&query_name, &args, &auth_context).await {
                Ok(result) => result,
                Err(error) => {
                    self.unsubscribe(subscription_id);
                    return Err(error);
                }
            };
            data
        };

        tracing::trace!(?subscription_id, "Subscription created");
        Ok((subscription_id, data))
    }

    /// Unsubscribe from a query.
    pub fn unsubscribe(&self, subscription_id: SubscriptionId) {
        self.session_server.remove_subscription(subscription_id);
        self.subscription_manager.unsubscribe(subscription_id);
        tracing::trace!(?subscription_id, "Subscription removed");
    }

    /// Subscribe to job progress updates.
    pub async fn subscribe_job(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        job_id: Uuid,
        auth_context: &forge_core::function::AuthContext,
    ) -> forge_core::Result<JobData> {
        Self::ensure_job_access(&self.read_pool, job_id, auth_context).await?;
        let job_data = self.fetch_job_data(job_id).await?;

        let subscription = JobSubscription {
            session_id,
            client_sub_id,
            auth_context: auth_context.clone(),
        };

        let mut subs = self.job_subscriptions.write().await;
        subs.entry(job_id).or_default().push(subscription);

        tracing::trace!(%job_id, %session_id, "Job subscription created");
        Ok(job_data)
    }

    /// Unsubscribe from job updates.
    pub async fn unsubscribe_job(&self, session_id: SessionId, client_sub_id: &str) {
        let mut subs = self.job_subscriptions.write().await;
        for subscribers in subs.values_mut() {
            subscribers
                .retain(|s| !(s.session_id == session_id && s.client_sub_id == client_sub_id));
        }
        subs.retain(|_, v| !v.is_empty());
    }

    /// Subscribe to workflow progress updates.
    pub async fn subscribe_workflow(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        workflow_id: Uuid,
        auth_context: &forge_core::function::AuthContext,
    ) -> forge_core::Result<WorkflowData> {
        Self::ensure_workflow_access(&self.read_pool, workflow_id, auth_context).await?;
        let workflow_data = self.fetch_workflow_data(workflow_id).await?;

        let subscription = WorkflowSubscription {
            session_id,
            client_sub_id,
            auth_context: auth_context.clone(),
        };

        let mut subs = self.workflow_subscriptions.write().await;
        subs.entry(workflow_id).or_default().push(subscription);

        tracing::trace!(%workflow_id, %session_id, "Workflow subscription created");
        Ok(workflow_data)
    }

    /// Unsubscribe from workflow updates.
    pub async fn unsubscribe_workflow(&self, session_id: SessionId, client_sub_id: &str) {
        let mut subs = self.workflow_subscriptions.write().await;
        for subscribers in subs.values_mut() {
            subscribers
                .retain(|s| !(s.session_id == session_id && s.client_sub_id == client_sub_id));
        }
        subs.retain(|_, v| !v.is_empty());
    }

    #[allow(clippy::type_complexity)]
    async fn fetch_job_data(&self, job_id: Uuid) -> forge_core::Result<JobData> {
        Self::fetch_job_data_static(job_id, &self.read_pool).await
    }

    async fn fetch_workflow_data(&self, workflow_id: Uuid) -> forge_core::Result<WorkflowData> {
        Self::fetch_workflow_data_static(workflow_id, &self.read_pool).await
    }

    /// Execute a query and return data with read set.
    async fn execute_query(
        &self,
        query_name: &str,
        args: &serde_json::Value,
        auth_context: &forge_core::function::AuthContext,
    ) -> forge_core::Result<(serde_json::Value, ReadSet)> {
        Self::execute_query_static(
            &self.registry,
            &self.read_pool,
            query_name,
            args,
            auth_context,
        )
        .await
    }

    fn compute_hash(data: &serde_json::Value) -> String {
        let json = serde_json::to_string(data).unwrap_or_default();
        crate::stable_hash::sha256_hex(json.as_bytes())
    }

    /// Parallel group re-execution with bounded concurrency.
    async fn flush_invalidations(
        invalidation_engine: &Arc<InvalidationEngine>,
        subscription_manager: &Arc<SubscriptionManager>,
        session_server: &Arc<SessionServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        max_concurrent: usize,
    ) {
        let invalidated_groups = invalidation_engine.check_pending().await;
        if invalidated_groups.is_empty() {
            return;
        }

        tracing::trace!(
            count = invalidated_groups.len(),
            "Invalidating query groups"
        );

        Self::reexecute_groups(
            &invalidated_groups,
            subscription_manager,
            session_server,
            registry,
            db_pool,
            max_concurrent,
        )
        .await;
    }

    /// Trim old entries from the durable change log. Runs on the cleanup
    /// interval (default 60s). Silently skips if the table doesn't exist yet.
    async fn trim_change_log(db_pool: &sqlx::PgPool) {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(1);
        match crate::pg::trim_change_log(db_pool, cutoff).await {
            Ok(deleted) if deleted > 0 => {
                tracing::debug!(deleted, "Trimmed change log");
            }
            Err(e) => {
                tracing::trace!(error = %e, "Change log trim skipped (table may not exist)");
            }
            _ => {}
        }
    }

    /// Resync sweep: re-evaluate every active subscription group. Recovers
    /// from `LISTEN/NOTIFY` payload drops where invalidation never fired.
    /// Hash comparison inside `reexecute_groups` ensures we only push when
    /// the result actually changed, so an idle resync is a no-op on the wire.
    async fn resync_all_groups(
        subscription_manager: &Arc<SubscriptionManager>,
        session_server: &Arc<SessionServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        max_concurrent: usize,
    ) {
        let group_ids = subscription_manager.all_group_ids();
        if group_ids.is_empty() {
            return;
        }

        tracing::debug!(count = group_ids.len(), "Resyncing all subscription groups");

        Self::reexecute_groups(
            &group_ids,
            subscription_manager,
            session_server,
            registry,
            db_pool,
            max_concurrent,
        )
        .await;
    }

    /// Re-run the queries for `group_ids`, push to subscribers when the result
    /// hash changes. Shared by invalidation flush and the periodic resync.
    async fn reexecute_groups(
        group_ids: &[forge_core::realtime::QueryGroupId],
        subscription_manager: &Arc<SubscriptionManager>,
        session_server: &Arc<SessionServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        max_concurrent: usize,
    ) {
        // Collect group data we need for re-execution. Skip groups whose
        // cached auth context has an expired JWT — re-running the query would
        // either leak fresh data past an expired session or get torn down by
        // `try_send_to_session`'s own expiry check anyway. Cheaper to filter
        // here and let session eviction reclaim the subscription via the
        // existing cleanup paths.
        let groups_to_process: Vec<_> = group_ids
            .iter()
            .filter_map(|gid| {
                subscription_manager.get_group(*gid).and_then(|g| {
                    if g.auth_context.token_is_expired() {
                        tracing::debug!(
                            group_id = ?g.id,
                            "Skipping reactor re-execute: cached auth token expired"
                        );
                        None
                    } else {
                        Some((
                            g.id,
                            g.query_name.clone(),
                            (*g.args).clone(),
                            g.last_result_hash.clone(),
                            g.auth_context.clone(),
                        ))
                    }
                })
            })
            .collect();

        // Parallel re-execution bounded by semaphore
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut futures = FuturesUnordered::new();

        for (group_id, query_name, args, last_hash, auth_context) in groups_to_process {
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break,
            };
            let registry = registry.clone();
            let db_pool = db_pool.clone();

            futures.push(async move {
                let result = Self::execute_query_static(
                    &registry,
                    &db_pool,
                    &query_name,
                    &args,
                    &auth_context,
                )
                .await;
                drop(permit);
                (group_id, last_hash, result)
            });
        }

        // Process results and fan out to subscribers
        while let Some((group_id, last_hash, result)) = futures.next().await {
            match result {
                Ok((new_data, read_set)) => {
                    let new_hash = Self::compute_hash(&new_data);

                    if last_hash.as_ref() != Some(&new_hash) {
                        // Update group state
                        subscription_manager.update_group(group_id, read_set, new_hash);

                        // Fan out to all subscribers in this group
                        let subscribers = subscription_manager.get_group_subscribers(group_id);
                        for (session_id, client_sub_id) in subscribers {
                            let message = RealtimeMessage::Data {
                                subscription_id: client_sub_id.clone(),
                                data: new_data.clone(),
                            };

                            if let Err(e) = session_server.try_send_to_session(session_id, message)
                            {
                                tracing::trace!(
                                    client_id = %client_sub_id,
                                    error = ?e,
                                    "Failed to send update to subscriber"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(?group_id, error = %e, "Failed to re-execute query group");
                }
            }
        }
    }

    /// Start the reactor.
    pub async fn start(&self) -> forge_core::Result<()> {
        let listener = self.change_listener.clone();
        let invalidation_engine = self.invalidation_engine.clone();
        let subscription_manager = self.subscription_manager.clone();
        let job_subscriptions = self.job_subscriptions.clone();
        let workflow_subscriptions = self.workflow_subscriptions.clone();
        let session_server = self.session_server.clone();
        let registry = self.registry.clone();
        let db_pool = self.read_pool.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let max_restarts = self.max_listener_restarts;
        let base_delay_ms = self.listener_restart_delay_ms;
        let max_concurrent = self.max_concurrent_reexecutions;
        let cleanup_secs = self.session_cleanup_interval_secs;
        let resync_secs = self.resync_interval_secs;

        let mut change_rx = listener.subscribe();

        tokio::spawn(async move {
            tracing::debug!("Reactor listening for changes");

            let mut restart_count: u32 = 0;
            let (listener_error_tx, mut listener_error_rx) = mpsc::channel::<String>(1);

            // Start initial listener
            let listener_clone = listener.clone();
            let error_tx = listener_error_tx.clone();
            let mut listener_handle = Some(tokio::spawn(async move {
                if let Err(e) = listener_clone.run().await {
                    let _ = error_tx.send(format!("Change listener error: {}", e)).await;
                }
            }));

            let mut flush_interval = tokio::time::interval(std::time::Duration::from_millis(25));
            flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut cleanup_interval =
                tokio::time::interval(std::time::Duration::from_secs(cleanup_secs));
            cleanup_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            // `resync_secs == 0` disables the sweep. Use a far-future interval
            // so the select! arm is well-typed but never fires.
            let mut resync_interval = if resync_secs == 0 {
                let mut i = tokio::time::interval(std::time::Duration::from_secs(86400 * 365));
                i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                i
            } else {
                let mut i = tokio::time::interval(std::time::Duration::from_secs(resync_secs));
                i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // Skip the immediate first tick so startup doesn't trigger a sweep.
                i.tick().await;
                i
            };

            loop {
                tokio::select! {
                    result = change_rx.recv() => {
                        match result {
                            Ok(change) => {
                                Self::handle_change(
                                    &change,
                                    &invalidation_engine,
                                    &job_subscriptions,
                                    &workflow_subscriptions,
                                    &session_server,
                                    &db_pool,
                                ).await;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Reactor lagged by {} messages", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                tracing::debug!("Change channel closed");
                                break;
                            }
                        }
                    }
                    _ = flush_interval.tick() => {
                        if listener.take_needs_resync() {
                            tracing::info!("Change log gap detected, running immediate full resync");
                            Self::resync_all_groups(
                                &subscription_manager,
                                &session_server,
                                &registry,
                                &db_pool,
                                max_concurrent,
                            ).await;
                        }
                        Self::flush_invalidations(
                            &invalidation_engine,
                            &subscription_manager,
                            &session_server,
                            &registry,
                            &db_pool,
                            max_concurrent,
                        ).await;
                    }
                    _ = cleanup_interval.tick() => {
                        session_server.cleanup_stale(std::time::Duration::from_secs(300));
                        session_server.cleanup_expired_tokens();
                        Self::trim_change_log(&db_pool).await;
                    }
                    _ = resync_interval.tick(), if resync_secs != 0 => {
                        Self::resync_all_groups(
                            &subscription_manager,
                            &session_server,
                            &registry,
                            &db_pool,
                            max_concurrent,
                        ).await;
                    }
                    Some(error_msg) = listener_error_rx.recv() => {
                        if restart_count >= max_restarts {
                            tracing::error!(
                                attempts = restart_count,
                                last_error = %error_msg,
                                "Change listener failed permanently, real-time updates disabled"
                            );
                            break;
                        }

                        restart_count += 1;
                        let delay = base_delay_ms * 2u64.saturating_pow(restart_count - 1);
                        tracing::warn!(
                            attempt = restart_count,
                            max = max_restarts,
                            delay_ms = delay,
                            error = %error_msg,
                            "Change listener restarting"
                        );

                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;

                        let listener_clone = listener.clone();
                        let error_tx = listener_error_tx.clone();
                        if let Some(handle) = listener_handle.take() {
                            handle.abort();
                        }
                        change_rx = listener.subscribe();
                        listener_handle = Some(tokio::spawn(async move {
                            if let Err(e) = listener_clone.run().await {
                                let _ = error_tx.send(format!("Change listener error: {}", e)).await;
                            }
                        }));
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("Reactor shutting down");
                        break;
                    }
                }
            }

            if let Some(handle) = listener_handle {
                handle.abort();
            }
        });

        Ok(())
    }

    /// Handle a database change event.
    #[allow(clippy::too_many_arguments)]
    async fn handle_change(
        change: &Change,
        invalidation_engine: &Arc<InvalidationEngine>,
        job_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
        workflow_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
        session_server: &Arc<SessionServer>,
        db_pool: &sqlx::PgPool,
    ) {
        tracing::trace!(table = %change.table, op = ?change.operation, row_id = ?change.row_id, "Processing change");

        match change.table.as_str() {
            "forge_jobs" => {
                if let Some(job_id) = change.row_id {
                    Self::handle_job_change(job_id, job_subscriptions, session_server, db_pool)
                        .await;
                }
                return;
            }
            "forge_workflow_runs" => {
                if let Some(workflow_id) = change.row_id {
                    Self::handle_workflow_change(
                        workflow_id,
                        workflow_subscriptions,
                        session_server,
                        db_pool,
                    )
                    .await;
                }
                return;
            }
            "forge_workflow_steps" => {
                if let Some(step_id) = change.row_id {
                    Self::handle_workflow_step_change(
                        step_id,
                        workflow_subscriptions,
                        session_server,
                        db_pool,
                    )
                    .await;
                }
                return;
            }
            _ => {}
        }

        // Record change for debounced group invalidation
        invalidation_engine.process_change(change.clone()).await;
    }

    async fn handle_job_change(
        job_id: Uuid,
        job_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
        session_server: &Arc<SessionServer>,
        db_pool: &sqlx::PgPool,
    ) {
        let subs = job_subscriptions.read().await;
        let subscribers = match subs.get(&job_id) {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return,
        };
        drop(subs);

        let job_data = match Self::fetch_job_data_static(job_id, db_pool).await {
            Ok(data) => data,
            Err(e) => {
                tracing::debug!(%job_id, error = %e, "Failed to fetch job data");
                return;
            }
        };

        let owner_subject = match Self::fetch_job_owner_subject_static(job_id, db_pool).await {
            Ok(owner) => owner,
            Err(e) => {
                tracing::debug!(%job_id, error = %e, "Failed to fetch job owner");
                return;
            }
        };

        let mut unauthorized: HashSet<(SessionId, String)> = HashSet::new();

        for sub in &subscribers {
            if Self::check_owner_access(owner_subject.clone(), &sub.auth_context).is_err() {
                unauthorized.insert((sub.session_id, sub.client_sub_id.clone()));
                continue;
            }

            let message = RealtimeMessage::JobUpdate {
                client_sub_id: sub.client_sub_id.clone(),
                job: job_data.clone(),
            };

            if let Err(e) = session_server
                .send_to_session(sub.session_id, message)
                .await
            {
                tracing::trace!(%job_id, error = %e, "Failed to send job update");
            }
        }

        if !unauthorized.is_empty() {
            let mut subs = job_subscriptions.write().await;
            if let Some(entries) = subs.get_mut(&job_id) {
                entries
                    .retain(|e| !unauthorized.contains(&(e.session_id, e.client_sub_id.clone())));
            }
            subs.retain(|_, v| !v.is_empty());
        }
    }

    async fn handle_workflow_change(
        workflow_id: Uuid,
        workflow_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
        session_server: &Arc<SessionServer>,
        db_pool: &sqlx::PgPool,
    ) {
        let subs = workflow_subscriptions.read().await;
        let subscribers = match subs.get(&workflow_id) {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return,
        };
        drop(subs);

        let workflow_data = match Self::fetch_workflow_data_static(workflow_id, db_pool).await {
            Ok(data) => data,
            Err(e) => {
                tracing::debug!(%workflow_id, error = %e, "Failed to fetch workflow data");
                return;
            }
        };

        let owner_subject =
            match Self::fetch_workflow_owner_subject_static(workflow_id, db_pool).await {
                Ok(owner) => owner,
                Err(e) => {
                    tracing::debug!(%workflow_id, error = %e, "Failed to fetch workflow owner");
                    return;
                }
            };

        let mut unauthorized: HashSet<(SessionId, String)> = HashSet::new();

        for sub in &subscribers {
            if Self::check_owner_access(owner_subject.clone(), &sub.auth_context).is_err() {
                unauthorized.insert((sub.session_id, sub.client_sub_id.clone()));
                continue;
            }

            let message = RealtimeMessage::WorkflowUpdate {
                client_sub_id: sub.client_sub_id.clone(),
                workflow: workflow_data.clone(),
            };

            if let Err(e) = session_server
                .send_to_session(sub.session_id, message)
                .await
            {
                tracing::trace!(%workflow_id, error = %e, "Failed to send workflow update");
            }
        }

        if !unauthorized.is_empty() {
            let mut subs = workflow_subscriptions.write().await;
            if let Some(entries) = subs.get_mut(&workflow_id) {
                entries
                    .retain(|e| !unauthorized.contains(&(e.session_id, e.client_sub_id.clone())));
            }
            subs.retain(|_, v| !v.is_empty());
        }
    }

    async fn handle_workflow_step_change(
        step_id: Uuid,
        workflow_subscriptions: &Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
        session_server: &Arc<SessionServer>,
        db_pool: &sqlx::PgPool,
    ) {
        let workflow_id: Option<Uuid> = match sqlx::query_scalar!(
            "SELECT workflow_run_id FROM forge_workflow_steps WHERE id = $1",
            step_id,
        )
        .fetch_optional(db_pool)
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::debug!(%step_id, error = %e, "Failed to look up workflow for step");
                return;
            }
        };

        if let Some(wf_id) = workflow_id {
            Self::handle_workflow_change(wf_id, workflow_subscriptions, session_server, db_pool)
                .await;
        }
    }

    #[allow(clippy::type_complexity)]
    async fn fetch_job_data_static(
        job_id: Uuid,
        db_pool: &sqlx::PgPool,
    ) -> forge_core::Result<JobData> {
        let row = sqlx::query!(
            r#"
                SELECT status, progress_percent, progress_message, output, last_error
                FROM forge_jobs WHERE id = $1
                "#,
            job_id
        )
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        match row {
            Some(row) => Ok(JobData {
                job_id: job_id.to_string(),
                status: row.status,
                progress_percent: row.progress_percent,
                progress_message: row.progress_message,
                output: row.output,
                error: row.last_error,
            }),
            None => Err(forge_core::ForgeError::NotFound(format!(
                "Job {} not found",
                job_id
            ))),
        }
    }

    async fn fetch_job_owner_subject_static(
        job_id: Uuid,
        db_pool: &sqlx::PgPool,
    ) -> forge_core::Result<Option<String>> {
        let owner_subject: Option<Option<String>> =
            sqlx::query_scalar!("SELECT owner_subject FROM forge_jobs WHERE id = $1", job_id)
                .fetch_optional(db_pool)
                .await
                .map_err(forge_core::ForgeError::Database)?;

        owner_subject
            .ok_or_else(|| forge_core::ForgeError::NotFound(format!("Job {} not found", job_id)))
    }

    #[allow(clippy::type_complexity)]
    async fn fetch_workflow_data_static(
        workflow_id: Uuid,
        db_pool: &sqlx::PgPool,
    ) -> forge_core::Result<WorkflowData> {
        let row = sqlx::query!(
            r#"
                SELECT status, current_step, waiting_for_event, output, error
                FROM forge_workflow_runs WHERE id = $1
                "#,
            workflow_id
        )
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let row = match row {
            Some(r) => r,
            None => {
                return Err(forge_core::ForgeError::NotFound(format!(
                    "Workflow {} not found",
                    workflow_id
                )));
            }
        };

        let step_rows = sqlx::query!(
            r#"
            SELECT step_name, status, error
            FROM forge_workflow_steps
            WHERE workflow_run_id = $1
            ORDER BY started_at ASC NULLS LAST
            "#,
            workflow_id
        )
        .fetch_all(db_pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let steps = step_rows
            .into_iter()
            .map(|row| WorkflowStepData {
                name: row.step_name,
                status: row.status,
                error: row.error,
            })
            .collect();

        Ok(WorkflowData {
            workflow_id: workflow_id.to_string(),
            status: row.status,
            current_step: row.current_step,
            waiting_for: row.waiting_for_event,
            steps,
            output: row.output,
            error: row.error,
        })
    }

    async fn fetch_workflow_owner_subject_static(
        workflow_id: Uuid,
        db_pool: &sqlx::PgPool,
    ) -> forge_core::Result<Option<String>> {
        let owner_subject: Option<Option<String>> = sqlx::query_scalar!(
            "SELECT owner_subject FROM forge_workflow_runs WHERE id = $1",
            workflow_id,
        )
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        owner_subject.ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!("Workflow {} not found", workflow_id))
        })
    }

    async fn execute_query_static(
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        query_name: &str,
        args: &serde_json::Value,
        auth_context: &forge_core::function::AuthContext,
    ) -> forge_core::Result<(serde_json::Value, ReadSet)> {
        match registry.get(query_name) {
            Some(FunctionEntry::Query { info, handler }) => {
                Self::check_query_auth(info, auth_context)?;

                let ctx = forge_core::function::QueryContext::new(
                    db_pool.clone(),
                    auth_context.clone(),
                    forge_core::function::RequestMetadata::new(),
                );

                let normalized_args = match args {
                    v if v.as_object().is_some_and(|o| o.is_empty()) => serde_json::Value::Null,
                    v => v.clone(),
                };

                let data = handler(&ctx, normalized_args).await?;

                let mut read_set = ReadSet::new();

                if info.table_dependencies.is_empty() {
                    let table_name = Self::extract_table_name(query_name);
                    read_set.add_table(&table_name);
                    tracing::trace!(
                        query = %query_name,
                        fallback_table = %table_name,
                        "Using naming convention fallback for table dependency"
                    );
                } else {
                    for table in info.table_dependencies {
                        read_set.add_table(*table);
                    }
                }

                Ok((data, read_set))
            }
            _ => Err(forge_core::ForgeError::Validation(format!(
                "Query '{}' not found or not a query",
                query_name
            ))),
        }
    }

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

    fn check_query_auth(
        info: &forge_core::function::FunctionInfo,
        auth: &forge_core::function::AuthContext,
    ) -> forge_core::Result<()> {
        if info.is_public {
            return Ok(());
        }

        if !auth.is_authenticated() {
            return Err(forge_core::ForgeError::Unauthorized(
                "Authentication required".into(),
            ));
        }

        if let Some(role) = info.required_role
            && !auth.has_role(role)
        {
            return Err(forge_core::ForgeError::Forbidden(format!(
                "Role '{}' required",
                role
            )));
        }

        Ok(())
    }

    async fn ensure_job_access(
        db_pool: &sqlx::PgPool,
        job_id: Uuid,
        auth: &forge_core::function::AuthContext,
    ) -> forge_core::Result<()> {
        let owner_subject_row = sqlx::query_scalar!(
            r#"SELECT owner_subject FROM forge_jobs WHERE id = $1"#,
            job_id
        )
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let owner_subject = owner_subject_row
            .ok_or_else(|| forge_core::ForgeError::NotFound(format!("Job {} not found", job_id)))?;

        Self::check_owner_access(owner_subject, auth)
    }

    async fn ensure_workflow_access(
        db_pool: &sqlx::PgPool,
        workflow_id: Uuid,
        auth: &forge_core::function::AuthContext,
    ) -> forge_core::Result<()> {
        let owner_subject_row = sqlx::query_scalar!(
            r#"SELECT owner_subject FROM forge_workflow_runs WHERE id = $1"#,
            workflow_id
        )
        .fetch_optional(db_pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let owner_subject = owner_subject_row.ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!("Workflow {} not found", workflow_id))
        })?;

        Self::check_owner_access(owner_subject, auth)
    }

    fn check_owner_access(
        owner_subject: Option<String>,
        auth: &forge_core::function::AuthContext,
    ) -> forge_core::Result<()> {
        if auth.is_admin() {
            return Ok(());
        }

        // Treat empty string the same as NULL (no owner)
        let Some(owner) = owner_subject.filter(|s| !s.is_empty()) else {
            return Ok(());
        };

        let principal = auth.principal_id().ok_or_else(|| {
            forge_core::ForgeError::Unauthorized("Authentication required".to_string())
        })?;

        if owner == principal {
            Ok(())
        } else {
            Err(forge_core::ForgeError::Forbidden(
                "Not authorized to access this resource".to_string(),
            ))
        }
    }

    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(());
        self.change_listener.stop();
    }

    pub async fn stats(&self) -> ReactorStats {
        let session_stats = self.session_server.stats();
        let inv_stats = self.invalidation_engine.stats().await;

        ReactorStats {
            connections: session_stats.connections,
            subscriptions: session_stats.subscriptions,
            query_groups: self.subscription_manager.group_count(),
            pending_invalidations: inv_stats.pending_groups,
            listener_running: self.change_listener.is_running(),
        }
    }
}

/// Reactor statistics.
#[derive(Debug, Clone)]
pub struct ReactorStats {
    pub connections: usize,
    pub subscriptions: usize,
    pub query_groups: usize,
    pub pending_invalidations: usize,
    pub listener_running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_reactor_config_default() {
        let config = ReactorConfig::default();
        assert_eq!(config.listener.channel, "forge_changes");
        assert_eq!(config.invalidation.debounce_ms, 50);
        assert_eq!(config.max_listener_restarts, 5);
        assert_eq!(config.listener_restart_delay_ms, 1000);
        assert_eq!(config.max_concurrent_reexecutions, 64);
        assert_eq!(config.session_cleanup_interval_secs, 60);
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

    #[test]
    fn test_check_owner_access_allows_admin() {
        let auth = forge_core::function::AuthContext::authenticated_without_uuid(
            vec!["admin".to_string()],
            HashMap::from([(
                "sub".to_string(),
                serde_json::Value::String("admin-1".to_string()),
            )]),
        );

        let result = Reactor::check_owner_access(Some("other-user".to_string()), &auth);
        assert!(result.is_ok());
    }
}

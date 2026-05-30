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
use crate::pg::{Database, PgNotifyBus};

#[derive(Debug, Clone)]
pub struct ReactorConfig {
    pub listener: ListenerConfig,
    pub invalidation: InvalidationConfig,
    pub realtime: RealtimeConfig,
    pub max_listener_restarts: u32,
    pub listener_restart_delay_ms: u64,
    pub max_concurrent_reexecutions: usize,
    /// Per-group query timeout; slow groups are skipped for the cycle.
    pub reexecution_timeout: std::time::Duration,
    pub session_cleanup_interval_secs: u64,
    /// Periodic resync sweep interval. `0` disables.
    pub resync_interval_secs: u64,
    pub shard_count: usize,
    /// Results larger than this are not cached in QueryGroup::last_result.
    pub max_cached_result_bytes: usize,
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
            reexecution_timeout: std::time::Duration::from_secs(5),
            session_cleanup_interval_secs: 60,
            resync_interval_secs: 600,
            shard_count: 64,
            max_cached_result_bytes: 10_485_760,
        }
    }
}

/// Job subscription tracking (keyed by job_id externally).
#[derive(Debug, Clone)]
pub struct JobSubscription {
    pub session_id: SessionId,
    pub client_sub_id: String,
    pub auth_context: forge_core::function::AuthContext,
    pub token_exp: Option<i64>,
}

/// Workflow subscription tracking (keyed by workflow_id externally).
#[derive(Debug, Clone)]
pub struct WorkflowSubscription {
    pub session_id: SessionId,
    pub client_sub_id: String,
    pub auth_context: forge_core::function::AuthContext,
    pub token_exp: Option<i64>,
}

/// Drives change-listener -> invalidation -> re-execution -> SSE fan-out.
pub struct Reactor {
    node_id: NodeId,
    database: Arc<Database>,
    registry: FunctionRegistry,
    subscription_manager: Arc<SubscriptionManager>,
    session_server: Arc<SessionServer>,
    change_listener: Arc<ChangeListener>,
    notify_bus: Arc<PgNotifyBus>,
    invalidation_engine: Arc<InvalidationEngine>,
    /// Job subscriptions: job_id -> list of subscribers.
    job_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
    /// Workflow subscriptions: workflow_id -> list of subscribers.
    workflow_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
    /// Reverse index for O(1) session cleanup.
    session_job_ids: Arc<RwLock<HashMap<SessionId, HashSet<Uuid>>>>,
    /// Reverse index for O(1) session cleanup.
    session_workflow_ids: Arc<RwLock<HashMap<SessionId, HashSet<Uuid>>>>,
    shutdown_tx: broadcast::Sender<()>,
    bus_shutdown_tx: tokio::sync::watch::Sender<bool>,
    max_listener_restarts: u32,
    listener_restart_delay_ms: u64,
    max_concurrent_reexecutions: usize,
    reexecution_timeout: std::time::Duration,
    session_cleanup_interval_secs: u64,
    resync_interval_secs: u64,
}

impl Reactor {
    /// Create a new reactor.
    pub fn new(
        node_id: NodeId,
        database: Arc<Database>,
        registry: FunctionRegistry,
        config: ReactorConfig,
        notify_bus: Arc<PgNotifyBus>,
    ) -> Self {
        let subscription_manager = Arc::new(SubscriptionManager::with_config(
            config.realtime.max_subscriptions_per_session,
            config.shard_count,
            config.max_cached_result_bytes,
        ));
        let session_server = Arc::new(SessionServer::new(node_id, config.realtime.clone()));
        let change_listener = Arc::new(ChangeListener::new(
            database.primary().clone(),
            config.listener,
        ));
        let invalidation_engine = Arc::new(InvalidationEngine::new(
            subscription_manager.clone(),
            config.invalidation,
        ));
        let (shutdown_tx, _) = broadcast::channel(1);
        let (bus_shutdown_tx, _) = tokio::sync::watch::channel(false);

        Self {
            node_id,
            database,
            registry,
            subscription_manager,
            session_server,
            change_listener,
            notify_bus,
            invalidation_engine,
            job_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            workflow_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            session_job_ids: Arc::new(RwLock::new(HashMap::new())),
            session_workflow_ids: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx,
            bus_shutdown_tx,
            max_listener_restarts: config.max_listener_restarts,
            listener_restart_delay_ms: config.listener_restart_delay_ms,
            max_concurrent_reexecutions: config.max_concurrent_reexecutions,
            reexecution_timeout: config.reexecution_timeout,
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

    /// Subscribe to the cluster-wide change stream.
    pub fn change_subscriber(&self) -> broadcast::Receiver<Change> {
        self.change_listener.subscribe()
    }

    /// Register a new SSE session with optional JWT expiry.
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
        self.subscription_manager
            .remove_session_subscriptions(session_id);
        self.session_server.remove_connection(session_id);

        // Clean up job subscriptions using reverse index for O(1) lookup
        // Lock order: subscriptions map BEFORE session reverse-index.
        // Matches the cleanup task and `unsubscribe_job`/`unsubscribe_workflow`
        // to remove the deadlock window where opposite orders could meet.
        {
            let mut job_subs = self.job_subscriptions.write().await;
            let job_ids = self.session_job_ids.write().await.remove(&session_id);
            if let Some(ids) = job_ids {
                for id in ids {
                    if let Some(subscribers) = job_subs.get_mut(&id) {
                        subscribers.retain(|s| s.session_id != session_id);
                        if subscribers.is_empty() {
                            job_subs.remove(&id);
                        }
                    }
                }
            }
        }

        // Clean up workflow subscriptions using reverse index for O(1) lookup
        {
            let mut workflow_subs = self.workflow_subscriptions.write().await;
            let wf_ids = self.session_workflow_ids.write().await.remove(&session_id);
            if let Some(ids) = wf_ids {
                for id in ids {
                    if let Some(subscribers) = workflow_subs.get_mut(&id) {
                        subscribers.retain(|s| s.session_id != session_id);
                        if subscribers.is_empty() {
                            workflow_subs.remove(&id);
                        }
                    }
                }
            }
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

            // A subscription with no observable table dependencies could
            // never be invalidated, so it would sit live forever and never
            // re-execute. Reject up front instead of silently going dark.
            if table_deps.is_empty() && read_set.tables.is_empty() {
                self.unsubscribe(subscription_id);
                return Err(forge_core::ForgeError::Validation(format!(
                    "Query '{}' has no table dependencies and cannot be subscribed to",
                    query_name
                )));
            }

            let Some((result_hash, serialized_len)) = Self::compute_hash(&data) else {
                self.unsubscribe(subscription_id);
                return Err(forge_core::ForgeError::internal(
                    "Failed to serialize query result for change detection",
                ));
            };

            tracing::trace!(
                ?group_id,
                query = %query_name,
                "New query group created"
            );

            let data_arc = std::sync::Arc::new(data.clone());
            self.subscription_manager.update_group_with_data(
                group_id,
                read_set,
                result_hash,
                data_arc,
                serialized_len,
            );

            data
        } else {
            let cached = self
                .subscription_manager
                .get_group(group_id)
                .and_then(|g| g.last_result.clone());

            if let Some(cached_data) = cached {
                (*cached_data).clone()
            } else {
                let (data, _) = match self.execute_query(&query_name, &args, &auth_context).await {
                    Ok(result) => result,
                    Err(error) => {
                        self.unsubscribe(subscription_id);
                        return Err(error);
                    }
                };
                data
            }
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
        Self::ensure_job_access(self.database.read_pool(), job_id, auth_context).await?;
        let job_data = self.fetch_job_data(job_id).await?;

        let subscription = JobSubscription {
            session_id,
            client_sub_id,
            auth_context: auth_context.clone(),
            token_exp: auth_context.token_exp(),
        };

        let mut subs = self.job_subscriptions.write().await;
        subs.entry(job_id).or_default().push(subscription);
        drop(subs);

        // Track in reverse index for O(1) cleanup on session removal
        self.session_job_ids
            .write()
            .await
            .entry(session_id)
            .or_default()
            .insert(job_id);

        tracing::trace!(%job_id, %session_id, "Job subscription created");
        Ok(job_data)
    }

    /// Unsubscribe from job updates.
    ///
    /// Requires `job_id` so this is O(subscribers for one job) instead of
    /// walking every job entry. Callers always know it: it's the id they
    /// passed to `subscribe_job`.
    ///
    /// Lock order: `job_subscriptions` -> `session_job_ids`. Matches
    /// `remove_session` to prevent deadlocks under adversarial scheduling.
    pub async fn unsubscribe_job(&self, session_id: SessionId, job_id: Uuid, client_sub_id: &str) {
        let removed = {
            let mut subs = self.job_subscriptions.write().await;
            let mut removed = false;
            if let Some(subscribers) = subs.get_mut(&job_id) {
                let before = subscribers.len();
                subscribers
                    .retain(|s| !(s.session_id == session_id && s.client_sub_id == client_sub_id));
                removed = subscribers.len() < before;
                if subscribers.is_empty() {
                    subs.remove(&job_id);
                }
            }
            removed
        };

        if removed {
            let mut session_jobs = self.session_job_ids.write().await;
            if let Some(ids) = session_jobs.get_mut(&session_id) {
                ids.remove(&job_id);
                if ids.is_empty() {
                    session_jobs.remove(&session_id);
                }
            }
        }
    }

    /// Subscribe to workflow progress updates.
    pub async fn subscribe_workflow(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        workflow_id: Uuid,
        auth_context: &forge_core::function::AuthContext,
    ) -> forge_core::Result<WorkflowData> {
        Self::ensure_workflow_access(self.database.read_pool(), workflow_id, auth_context).await?;
        let workflow_data = self.fetch_workflow_data(workflow_id).await?;

        let subscription = WorkflowSubscription {
            session_id,
            client_sub_id,
            auth_context: auth_context.clone(),
            token_exp: auth_context.token_exp(),
        };

        let mut subs = self.workflow_subscriptions.write().await;
        subs.entry(workflow_id).or_default().push(subscription);
        drop(subs);

        // Track in reverse index for O(1) cleanup on session removal
        self.session_workflow_ids
            .write()
            .await
            .entry(session_id)
            .or_default()
            .insert(workflow_id);

        tracing::trace!(%workflow_id, %session_id, "Workflow subscription created");
        Ok(workflow_data)
    }

    /// Unsubscribe from workflow updates. See [`unsubscribe_job`] for the
    /// rationale on requiring the `workflow_id`.
    pub async fn unsubscribe_workflow(
        &self,
        session_id: SessionId,
        workflow_id: Uuid,
        client_sub_id: &str,
    ) {
        let removed = {
            let mut subs = self.workflow_subscriptions.write().await;
            let mut removed = false;
            if let Some(subscribers) = subs.get_mut(&workflow_id) {
                let before = subscribers.len();
                subscribers
                    .retain(|s| !(s.session_id == session_id && s.client_sub_id == client_sub_id));
                removed = subscribers.len() < before;
                if subscribers.is_empty() {
                    subs.remove(&workflow_id);
                }
            }
            removed
        };

        if removed {
            let mut session_wfs = self.session_workflow_ids.write().await;
            if let Some(ids) = session_wfs.get_mut(&session_id) {
                ids.remove(&workflow_id);
                if ids.is_empty() {
                    session_wfs.remove(&session_id);
                }
            }
        }
    }

    #[allow(clippy::type_complexity)]
    async fn fetch_job_data(&self, job_id: Uuid) -> forge_core::Result<JobData> {
        Self::fetch_job_data_static(job_id, self.database.read_pool()).await
    }

    async fn fetch_workflow_data(&self, workflow_id: Uuid) -> forge_core::Result<WorkflowData> {
        Self::fetch_workflow_data_static(workflow_id, self.database.read_pool()).await
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
            self.database.read_pool(),
            query_name,
            args,
            auth_context,
        )
        .await
    }

    /// Content hash for change detection; returns `(hash, byte_count)`.
    /// `None` if serialization fails — callers MUST skip the update so a
    /// failure sentinel isn't cached and used to suppress later, legitimate
    /// "still broken" notifications or emit spurious data on recovery.
    fn compute_hash(data: &serde_json::Value) -> Option<(String, usize)> {
        let bytes = serde_json::to_vec(data).ok()?;
        let len = bytes.len();
        Some((crate::stable_hash::sha256_hex(&bytes), len))
    }

    /// Flush pending invalidations with bounded concurrent re-execution.
    async fn flush_invalidations(
        invalidation_engine: &Arc<InvalidationEngine>,
        subscription_manager: &Arc<SubscriptionManager>,
        session_server: &Arc<SessionServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        max_concurrent: usize,
        reexecution_timeout: std::time::Duration,
    ) {
        let invalidated_groups = invalidation_engine.check_pending();
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
            reexecution_timeout,
        )
        .await;
    }

    /// Trim old entries from the durable change log.
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

    /// Resync sweep: re-evaluate every active group to recover from dropped notifications.
    async fn resync_all_groups(
        subscription_manager: &Arc<SubscriptionManager>,
        session_server: &Arc<SessionServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        max_concurrent: usize,
        reexecution_timeout: std::time::Duration,
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
            reexecution_timeout,
        )
        .await;
    }

    /// Drop every subscription for a session (query, job, workflow) and close
    /// its SSE channel after a final `AuthFailed` notification. Intended as an
    /// admin escape hatch for server-side auth revocation: cached
    /// `AuthContext` on `QueryGroup` is captured at subscribe time and only
    /// re-validated on JWT expiry, so demotions/tenant moves that happen
    /// before `exp` cannot be detected by the reactor itself. Operators wire
    /// this up to their identity system's revocation event; after the call
    /// the client must reconnect and re-subscribe with a fresh token.
    pub async fn revoke_session_auth(&self, session_id: SessionId, reason: &str) {
        self.subscription_manager
            .remove_session_subscriptions(session_id);
        self.session_server.revoke_session(session_id, reason);

        {
            let mut job_subs = self.job_subscriptions.write().await;
            let job_ids = self.session_job_ids.write().await.remove(&session_id);
            if let Some(ids) = job_ids {
                for id in ids {
                    if let Some(subscribers) = job_subs.get_mut(&id) {
                        subscribers.retain(|s| s.session_id != session_id);
                        if subscribers.is_empty() {
                            job_subs.remove(&id);
                        }
                    }
                }
            }
        }

        {
            let mut workflow_subs = self.workflow_subscriptions.write().await;
            let wf_ids = self.session_workflow_ids.write().await.remove(&session_id);
            if let Some(ids) = wf_ids {
                for id in ids {
                    if let Some(subscribers) = workflow_subs.get_mut(&id) {
                        subscribers.retain(|s| s.session_id != session_id);
                        if subscribers.is_empty() {
                            workflow_subs.remove(&id);
                        }
                    }
                }
            }
        }

        tracing::info!(?session_id, %reason, "Session auth revoked");
    }

    /// Re-run queries for groups, pushing to subscribers on hash change.
    ///
    /// Note: re-execution uses the `AuthContext` captured at subscribe time.
    /// Authorization is checked at subscribe time and only re-checked on
    /// token expiry. Server-side role/tenant changes before `exp` are not
    /// detected here; callers must invoke [`revoke_session_auth`] to evict
    /// affected sessions explicitly.
    async fn reexecute_groups(
        group_ids: &[forge_core::realtime::QueryGroupId],
        subscription_manager: &Arc<SubscriptionManager>,
        session_server: &Arc<SessionServer>,
        registry: &FunctionRegistry,
        db_pool: &sqlx::PgPool,
        max_concurrent: usize,
        reexecution_timeout: std::time::Duration,
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

        // Each group is spawned as an independent task so slow groups cannot
        // block the progress of faster ones. The semaphore still bounds total
        // concurrent DB queries; the permit is held only for the duration of
        // the query, not for the fan-out phase.
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut futures = FuturesUnordered::new();

        for (group_id, query_name, args, last_hash, auth_context) in groups_to_process {
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break,
            };
            let registry = registry.clone();
            let db_pool = db_pool.clone();

            // spawn so this group's query runs concurrently with the result
            // processing loop below, and does not block other spawned tasks.
            let handle = tokio::spawn(async move {
                let query_fut = Self::execute_query_static(
                    &registry,
                    &db_pool,
                    &query_name,
                    &args,
                    &auth_context,
                );
                let result = tokio::time::timeout(reexecution_timeout, query_fut)
                    .await
                    .unwrap_or_else(|_| {
                        Err(forge_core::ForgeError::Timeout(format!(
                            "query '{}' exceeded reexecution timeout ({:?})",
                            query_name, reexecution_timeout,
                        )))
                    });
                drop(permit);
                (group_id, last_hash, result)
            });

            futures.push(handle);
        }

        while let Some(join_result) = futures.next().await {
            let (group_id, last_hash, result) = match join_result {
                Ok(inner) => inner,
                Err(e) => {
                    tracing::warn!(error = %e, "Re-execution task panicked");
                    continue;
                }
            };
            match result {
                Ok((new_data, read_set)) => {
                    let Some((new_hash, serialized_len)) = Self::compute_hash(&new_data) else {
                        tracing::warn!(
                            ?group_id,
                            "Skipping group update: result failed to serialize"
                        );
                        continue;
                    };

                    if last_hash.as_ref() != Some(&new_hash) {
                        let data_arc = std::sync::Arc::new(new_data);
                        subscription_manager.update_group_with_data(
                            group_id,
                            read_set,
                            new_hash,
                            std::sync::Arc::clone(&data_arc),
                            serialized_len,
                        );

                        let subscribers = subscription_manager.get_group_subscribers(group_id);
                        for (session_id, client_sub_id) in subscribers {
                            let message = RealtimeMessage::Data {
                                subscription_id: client_sub_id.clone(),
                                data: std::sync::Arc::clone(&data_arc),
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
                Err(forge_core::ForgeError::Timeout(ref msg)) => {
                    tracing::warn!(
                        ?group_id,
                        message = %msg,
                        "Query group timed out during re-execution"
                    );
                }
                Err(e) => {
                    tracing::warn!(?group_id, error = %e, "Failed to re-execute query group");
                }
            }
        }
    }

    /// Start the reactor.
    pub async fn start(&self) -> forge_core::Result<()> {
        let bus = self.notify_bus.clone();
        let bus_shutdown_rx = self.bus_shutdown_tx.subscribe();
        tokio::spawn(async move {
            bus.run(bus_shutdown_rx).await;
        });

        let listener = self.change_listener.clone();
        let notify_bus = self.notify_bus.clone();
        let invalidation_engine = self.invalidation_engine.clone();
        let subscription_manager = self.subscription_manager.clone();
        let job_subscriptions = self.job_subscriptions.clone();
        let workflow_subscriptions = self.workflow_subscriptions.clone();
        let session_job_ids = self.session_job_ids.clone();
        let session_workflow_ids = self.session_workflow_ids.clone();
        let session_server = self.session_server.clone();
        let registry = self.registry.clone();
        let database = self.database.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let max_restarts = self.max_listener_restarts;
        let base_delay_ms = self.listener_restart_delay_ms;
        let max_concurrent = self.max_concurrent_reexecutions;
        let reexecution_timeout = self.reexecution_timeout;
        let cleanup_secs = self.session_cleanup_interval_secs;
        let resync_secs = self.resync_interval_secs;

        let mut change_rx = listener.subscribe();

        tokio::spawn(async move {
            tracing::debug!("Reactor listening for changes");

            let mut restart_count: u32 = 0;
            let mut consecutive_lags: u32 = 0;
            let (listener_error_tx, mut listener_error_rx) = mpsc::channel::<String>(1);

            // Start initial listener
            let listener_clone = listener.clone();
            let bus_clone = notify_bus.clone();
            let error_tx = listener_error_tx.clone();
            let mut listener_handle = Some(tokio::spawn(async move {
                if let Err(e) = listener_clone.run(&bus_clone).await {
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
                                // Any successful change means the listener is healthy
                                // again; reset so a long-lived process can absorb more
                                // transient failures over its lifetime.
                                restart_count = 0;
                                consecutive_lags = 0;
                                // A successful change proves the new listener
                                // is healthy; flush stale errors so they
                                // can't be misattributed to it later.
                                while listener_error_rx.try_recv().is_ok() {}
                                Self::handle_change(
                                    &change,
                                    &invalidation_engine,
                                    &job_subscriptions,
                                    &workflow_subscriptions,
                                    &session_server,
                                    database.read_pool(),
                                ).await;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                // Back off exponentially on consecutive lags so a
                                // sustained event rate above the broadcast buffer
                                // doesn't pin us in a resync-storm.
                                let backoff_ms = 100u64
                                    .saturating_mul(1u64 << consecutive_lags.min(6));
                                consecutive_lags = consecutive_lags.saturating_add(1);
                                tracing::warn!(
                                    missed = n,
                                    consecutive_lags,
                                    backoff_ms,
                                    "Reactor lagged; backing off before scheduling full resync"
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                listener.set_needs_resync();
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
                                database.read_pool(),
                                max_concurrent,
                                reexecution_timeout,
                            ).await;
                        }
                        Self::flush_invalidations(
                            &invalidation_engine,
                            &subscription_manager,
                            &session_server,
                            &registry,
                            database.read_pool(),
                            max_concurrent,
                            reexecution_timeout,
                        ).await;
                    }
                    _ = cleanup_interval.tick() => {
                        session_server.cleanup_stale(std::time::Duration::from_secs(300));
                        let expired_sessions = session_server.cleanup_expired_tokens();
                        if !expired_sessions.is_empty() {
                            // Immediately purge query-group subscriptions for
                            // expired sessions so stale groups are not
                            // re-executed during the next invalidation flush or
                            // resync sweep. Job/workflow subscription entries
                            // are likewise pruned so change fan-out skips them
                            // without waiting for the SSE bridge task to detect
                            // the closed channel.
                            let mut job_subs = job_subscriptions.write().await;
                            let mut wf_subs = workflow_subscriptions.write().await;
                            let mut sess_jobs = session_job_ids.write().await;
                            let mut sess_wfs = session_workflow_ids.write().await;

                            for session_id in expired_sessions {
                                subscription_manager
                                    .remove_session_subscriptions(session_id);

                                if let Some(job_ids) = sess_jobs.remove(&session_id) {
                                    for id in job_ids {
                                        if let Some(subs) = job_subs.get_mut(&id) {
                                            subs.retain(|s| s.session_id != session_id);
                                        }
                                    }
                                }
                                if let Some(wf_ids) = sess_wfs.remove(&session_id) {
                                    for id in wf_ids {
                                        if let Some(subs) = wf_subs.get_mut(&id) {
                                            subs.retain(|s| s.session_id != session_id);
                                        }
                                    }
                                }
                            }

                            job_subs.retain(|_, v| !v.is_empty());
                            wf_subs.retain(|_, v| !v.is_empty());
                        }
                        Self::trim_change_log(database.read_pool()).await;

                        // Emit subscription cardinality gauges on the cleanup
                        // cadence so dashboards show steady-state counts.
                        let counts = subscription_manager.counts();
                        crate::observability::record_subscription_counts(
                            counts.total,
                            counts.unique_queries,
                            counts.indexed_tables,
                        );
                    }
                    _ = resync_interval.tick(), if resync_secs != 0 => {
                        Self::resync_all_groups(
                            &subscription_manager,
                            &session_server,
                            &registry,
                            database.read_pool(),
                            max_concurrent,
                            reexecution_timeout,
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
                        let bus_clone = notify_bus.clone();
                        let error_tx = listener_error_tx.clone();
                        if let Some(handle) = listener_handle.take() {
                            handle.abort();
                        }
                        // Drain any further error messages that piled up
                        // while we were sleeping the backoff: the aborted
                        // listener may have queued additional failures, and
                        // the new listener must not be debited for them
                        // (each stale message would otherwise bump
                        // restart_count toward max_restarts on phantom
                        // restarts and emit a false "permanently failed").
                        while listener_error_rx.try_recv().is_ok() {}
                        change_rx = listener.subscribe();
                        listener_handle = Some(tokio::spawn(async move {
                            if let Err(e) = listener_clone.run(&bus_clone).await {
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
                } else {
                    // Statement-level trigger: refresh all active job subscriptions
                    let subs = job_subscriptions.read().await;
                    let job_ids: Vec<Uuid> = subs.keys().copied().collect();
                    drop(subs);
                    for job_id in job_ids {
                        Self::handle_job_change(job_id, job_subscriptions, session_server, db_pool)
                            .await;
                    }
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
                } else {
                    let subs = workflow_subscriptions.read().await;
                    let workflow_ids: Vec<Uuid> = subs.keys().copied().collect();
                    drop(subs);
                    for workflow_id in workflow_ids {
                        Self::handle_workflow_change(
                            workflow_id,
                            workflow_subscriptions,
                            session_server,
                            db_pool,
                        )
                        .await;
                    }
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
                } else {
                    let subs = workflow_subscriptions.read().await;
                    let workflow_ids: Vec<Uuid> = subs.keys().copied().collect();
                    drop(subs);
                    for workflow_id in workflow_ids {
                        Self::handle_workflow_change(
                            workflow_id,
                            workflow_subscriptions,
                            session_server,
                            db_pool,
                        )
                        .await;
                    }
                }
                return;
            }
            _ => {}
        }

        invalidation_engine.process_change(change.clone());
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

        let now = chrono::Utc::now().timestamp();
        let mut unauthorized: HashSet<(SessionId, String)> = HashSet::new();

        for sub in &subscribers {
            // Skip subscribers with expired tokens
            if sub.token_exp.is_some_and(|exp| exp < now) {
                unauthorized.insert((sub.session_id, sub.client_sub_id.clone()));
                continue;
            }

            if Self::check_owner_access(owner_subject.clone(), &sub.auth_context).is_err() {
                unauthorized.insert((sub.session_id, sub.client_sub_id.clone()));
                continue;
            }

            let message = RealtimeMessage::JobUpdate {
                client_sub_id: sub.client_sub_id.clone(),
                job: job_data.clone(),
            };

            if let Err(e) = session_server.try_send_to_session(sub.session_id, message) {
                tracing::trace!(%job_id, error = ?e, "Failed to send job update");
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

        let now = chrono::Utc::now().timestamp();
        let mut unauthorized: HashSet<(SessionId, String)> = HashSet::new();

        for sub in &subscribers {
            // Skip subscribers with expired tokens
            if sub.token_exp.is_some_and(|exp| exp < now) {
                unauthorized.insert((sub.session_id, sub.client_sub_id.clone()));
                continue;
            }

            if Self::check_owner_access(owner_subject.clone(), &sub.auth_context).is_err() {
                unauthorized.insert((sub.session_id, sub.client_sub_id.clone()));
                continue;
            }

            let message = RealtimeMessage::WorkflowUpdate {
                client_sub_id: sub.client_sub_id.clone(),
                workflow: workflow_data.clone(),
            };

            if let Err(e) = session_server.try_send_to_session(sub.session_id, message) {
                tracing::trace!(%workflow_id, error = ?e, "Failed to send workflow update");
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

                // No naming-convention fallback: a fake "table" equal to
                // the query name would never appear in real change events,
                // so the subscription would silently never re-execute.
                // Callers reject empty-deps subscriptions at subscribe time.
                for table in info.table_dependencies {
                    read_set.add_table(*table);
                }

                Ok((data, read_set))
            }
            _ => Err(forge_core::ForgeError::Validation(format!(
                "Query '{}' not found or not a query",
                query_name
            ))),
        }
    }

    /// Auth check for re-execution (authentication only, roles checked at subscribe time).
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
        let _ = self.bus_shutdown_tx.send(true);
        self.change_listener.stop();
    }

    pub async fn stats(&self) -> ReactorStats {
        let session_stats = self.session_server.stats();
        let inv_stats = self.invalidation_engine.stats();

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

        let (hash1, len1) = Reactor::compute_hash(&data1).expect("hash data1");
        let (hash2, _) = Reactor::compute_hash(&data2).expect("hash data2");
        let (hash3, _) = Reactor::compute_hash(&data3).expect("hash data3");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert!(len1 > 0);
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

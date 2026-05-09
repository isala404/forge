use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;

use forge_core::cluster::NodeId;
use forge_core::function::AuthContext;
use forge_core::realtime::{
    AuthScope, Change, QueryGroup, QueryGroupId, ReadSet, SessionId, SessionInfo, SessionStatus,
    Subscriber, SubscriberId, SubscriptionId, SubscriptionKey,
};

/// Session manager for tracking WebSocket connections.
pub struct SessionManager {
    sessions: DashMap<SessionId, SessionInfo>,
    node_id: NodeId,
}

impl SessionManager {
    /// Create a new session manager.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            sessions: DashMap::new(),
            node_id,
        }
    }

    /// Create a new session.
    pub fn create_session(&self) -> SessionInfo {
        let mut session = SessionInfo::new(self.node_id);
        session.connect();
        self.sessions.insert(session.id, session.clone());
        session
    }

    /// Get a session by ID.
    pub fn get_session(&self, session_id: SessionId) -> Option<SessionInfo> {
        self.sessions.get(&session_id).map(|r| r.clone())
    }

    /// Update a session.
    pub fn update_session(&self, session: SessionInfo) {
        self.sessions.insert(session.id, session);
    }

    /// Remove a session.
    pub fn remove_session(&self, session_id: SessionId) {
        self.sessions.remove(&session_id);
    }

    /// Mark a session as disconnected.
    pub fn disconnect_session(&self, session_id: SessionId) {
        if let Some(mut session) = self.sessions.get_mut(&session_id) {
            session.disconnect();
        }
    }

    /// Get all connected sessions.
    pub fn get_connected_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .filter(|r| r.is_connected())
            .map(|r| r.clone())
            .collect()
    }

    /// Count sessions by status.
    pub fn count_by_status(&self) -> SessionCounts {
        let mut counts = SessionCounts::default();

        for entry in self.sessions.iter() {
            match entry.status {
                SessionStatus::Connecting => counts.connecting += 1,
                SessionStatus::Connected => counts.connected += 1,
                SessionStatus::Reconnecting => counts.reconnecting += 1,
                SessionStatus::Disconnected => counts.disconnected += 1,
                // SessionStatus is #[non_exhaustive]; future variants are
                // counted in `total` only.
                _ => {}
            }
            counts.total += 1;
        }

        counts
    }

    /// Clean up disconnected sessions older than the given duration.
    pub fn cleanup_old_sessions(&self, max_age: std::time::Duration) {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(max_age).unwrap_or(chrono::TimeDelta::MAX);

        self.sessions.retain(|_, session| {
            session.status != SessionStatus::Disconnected || session.last_active_at > cutoff
        });
    }
}

/// Session count statistics.
#[derive(Debug, Clone, Default)]
pub struct SessionCounts {
    pub connecting: usize,
    pub connected: usize,
    pub reconnecting: usize,
    pub disconnected: usize,
    pub total: usize,
}

/// Group-based subscription manager using sharded concurrent data structures.
///
/// Primary index: groups by QueryGroupId (DashMap, 64 shards).
/// Secondary: lookup key -> QueryGroupId for dedup.
/// Subscribers stored in a HashMap for O(1) insert/remove by key.
/// Session -> subscribers mapping for cleanup.
pub struct SubscriptionManager {
    /// Query groups indexed by ID. Sharded for concurrent access.
    groups: DashMap<QueryGroupId, QueryGroup>,
    /// Lookup: hash(query_name+args+auth_scope) -> QueryGroupId for dedup.
    group_lookup: DashMap<SubscriptionKey, QueryGroupId>,
    /// Inverted index: table name -> set of group IDs that depend on that table.
    /// Maintained on subscribe/unsubscribe for O(1) affected-group lookups.
    table_index: DashMap<String, HashSet<QueryGroupId>>,
    /// Subscribers indexed by auto-incrementing key.
    subscribers: Arc<Mutex<SubscriberStore>>,
    /// Session -> subscriber IDs for fast cleanup on disconnect.
    session_subscribers: DashMap<SessionId, Vec<SubscriberId>>,
    /// Monotonic counter for group IDs.
    next_group_id: AtomicU32,
    /// Maximum subscriptions per session.
    max_per_session: usize,
}

/// Simple indexed store replacing the `slab` crate.
struct SubscriberStore {
    entries: HashMap<usize, Subscriber>,
    next_key: usize,
}

impl SubscriberStore {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next_key: 0,
        }
    }

    fn insert(&mut self, value: Subscriber) -> usize {
        let key = self.next_key;
        self.next_key += 1;
        self.entries.insert(key, value);
        key
    }

    fn get(&self, key: usize) -> Option<&Subscriber> {
        self.entries.get(&key)
    }

    fn remove(&mut self, key: usize) -> Option<Subscriber> {
        self.entries.remove(&key)
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &Subscriber)> {
        self.entries.iter().map(|(&k, v)| (k, v))
    }
}

impl SubscriptionManager {
    /// Create a new subscription manager.
    pub fn new(max_per_session: usize) -> Self {
        Self::with_shard_count(max_per_session, 64)
    }

    /// Create a new subscription manager with a custom shard count.
    pub fn with_shard_count(max_per_session: usize, shard_count: usize) -> Self {
        Self {
            groups: DashMap::with_shard_amount(shard_count),
            group_lookup: DashMap::with_shard_amount(shard_count),
            table_index: DashMap::with_shard_amount(shard_count),
            subscribers: Arc::new(Mutex::new(SubscriberStore::new())),
            session_subscribers: DashMap::with_shard_amount(shard_count),
            next_group_id: AtomicU32::new(0),
            max_per_session,
        }
    }

    /// Remove a group from the inverted table index. Covers both compile-time
    /// `table_deps` and runtime-discovered tables from the read set.
    fn remove_from_table_index(&self, group: &QueryGroup) {
        let runtime_tables = group.read_set.tables.iter().map(String::as_str);
        let compile_tables = group.table_deps.iter().copied();
        for table in compile_tables.chain(runtime_tables) {
            if let Some(mut set) = self.table_index.get_mut(table) {
                set.remove(&group.id);
                if set.is_empty() {
                    drop(set);
                    self.table_index.remove(table);
                }
            }
        }
    }

    /// Subscribe to a query group. Returns the group ID and whether this is a new group.
    /// If a group already exists for this query+args+auth_scope, the subscriber joins it.
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &self,
        session_id: SessionId,
        client_sub_id: String,
        query_name: &str,
        args: &serde_json::Value,
        auth_context: &AuthContext,
        table_deps: &'static [&'static str],
        selected_cols: &'static [&'static str],
    ) -> forge_core::Result<(QueryGroupId, SubscriptionId, bool)> {
        // Check per-session limit
        if let Some(subs) = self.session_subscribers.get(&session_id)
            && subs.len() >= self.max_per_session
        {
            return Err(forge_core::ForgeError::Validation(format!(
                "Maximum subscriptions per session ({}) exceeded",
                self.max_per_session
            )));
        }

        let auth_scope = AuthScope::from_auth(auth_context);
        let lookup_key = QueryGroup::compute_lookup_key(query_name, args, &auth_scope);

        // Atomic check-and-insert via DashMap entry API to avoid TOCTOU races
        let mut is_new = false;
        let group_id = *self.group_lookup.entry(lookup_key).or_insert_with(|| {
            is_new = true;
            let id = QueryGroupId(self.next_group_id.fetch_add(1, Ordering::Relaxed));
            let group = QueryGroup {
                id,
                query_name: query_name.to_string(),
                args: Arc::new(args.clone()),
                auth_scope: auth_scope.clone(),
                auth_context: auth_context.clone(),
                table_deps,
                selected_cols,
                read_set: ReadSet::new(),
                last_result_hash: None,
                subscribers: Vec::new(),
                created_at: chrono::Utc::now(),
                execution_count: 0,
            };
            self.groups.insert(id, group);
            id
        });

        // Populate the inverted table index for new groups
        if is_new {
            for table in table_deps {
                self.table_index
                    .entry((*table).to_string())
                    .or_default()
                    .insert(group_id);
            }
        }

        // Create subscriber in the store
        let subscription_id = SubscriptionId::new();
        let subscriber_id = {
            let mut store = self.subscribers.lock().unwrap_or_else(|e| {
                tracing::error!("Subscriber store lock was poisoned, recovering");
                e.into_inner()
            });
            let key = store.next_key;
            let sid = SubscriberId(key as u32);
            store.insert(Subscriber {
                id: sid,
                session_id,
                client_sub_id,
                group_id,
                subscription_id,
            });
            sid
        };

        // Add subscriber to group
        if let Some(mut group) = self.groups.get_mut(&group_id) {
            group.subscribers.push(subscriber_id);
        }

        // Track session -> subscriber mapping
        self.session_subscribers
            .entry(session_id)
            .or_default()
            .push(subscriber_id);

        Ok((group_id, subscription_id, is_new))
    }

    /// Remove a subscriber by its subscription ID.
    pub fn unsubscribe(&self, subscription_id: SubscriptionId) {
        let mut store = self.subscribers.lock().unwrap_or_else(|e| {
            tracing::error!("Subscriber store lock was poisoned, recovering");
            e.into_inner()
        });

        // Find the subscriber by subscription_id
        let sub_key = store
            .iter()
            .find(|(_, s)| s.subscription_id == subscription_id)
            .map(|(key, s)| (key, s.group_id, s.session_id));

        if let Some((key, group_id, session_id)) = sub_key {
            let subscriber_id = SubscriberId(key as u32);
            store.remove(key);

            // Remove from group
            drop(store); // Release lock before accessing DashMap
            if let Some(mut group) = self.groups.get_mut(&group_id) {
                group.subscribers.retain(|s| *s != subscriber_id);

                // If group is empty, remove it and its table index entries
                if group.subscribers.is_empty() {
                    let lookup_key = QueryGroup::compute_lookup_key(
                        &group.query_name,
                        &group.args,
                        &group.auth_scope,
                    );
                    self.remove_from_table_index(&group);
                    drop(group);
                    self.groups.remove(&group_id);
                    self.group_lookup.remove(&lookup_key);
                }
            }

            // Remove from session mapping
            if let Some(mut session_subs) = self.session_subscribers.get_mut(&session_id) {
                session_subs.retain(|s| *s != subscriber_id);
            }
        }
    }

    /// Remove all subscriptions for a session.
    pub fn remove_session_subscriptions(&self, session_id: SessionId) -> Vec<SubscriptionId> {
        let subscriber_ids: Vec<SubscriberId> = self
            .session_subscribers
            .remove(&session_id)
            .map(|(_, ids)| ids)
            .unwrap_or_default();

        let mut removed_sub_ids = Vec::new();
        let mut store = self.subscribers.lock().unwrap_or_else(|e| {
            tracing::error!("Subscriber store lock was poisoned, recovering");
            e.into_inner()
        });

        for sid in subscriber_ids {
            let key = sid.0 as usize;
            if let Some(sub) = store.remove(key) {
                removed_sub_ids.push(sub.subscription_id);

                // Remove from group
                if let Some(mut group) = self.groups.get_mut(&sub.group_id) {
                    group.subscribers.retain(|s| *s != sid);

                    if group.subscribers.is_empty() {
                        let lookup_key = QueryGroup::compute_lookup_key(
                            &group.query_name,
                            &group.args,
                            &group.auth_scope,
                        );
                        self.remove_from_table_index(&group);
                        drop(group);
                        self.groups.remove(&sub.group_id);
                        self.group_lookup.remove(&lookup_key);
                    }
                }
            }
        }

        removed_sub_ids
    }

    /// Find all groups affected by a change via the inverted table index.
    /// O(groups_for_table) with column-level filtering, not O(all_groups).
    pub fn find_affected_groups(&self, change: &Change) -> Vec<QueryGroupId> {
        let candidates = match self.table_index.get(&change.table) {
            Some(set) => set.clone(),
            None => return Vec::new(),
        };

        candidates
            .into_iter()
            .filter(|gid| {
                self.groups
                    .get(gid)
                    .is_some_and(|g| g.should_invalidate(change))
            })
            .collect()
    }

    /// Get a reference to a group by ID.
    pub fn get_group(
        &self,
        group_id: QueryGroupId,
    ) -> Option<dashmap::mapref::one::Ref<'_, QueryGroupId, QueryGroup>> {
        self.groups.get(&group_id)
    }

    /// Get a mutable reference to a group by ID.
    pub fn get_group_mut(
        &self,
        group_id: QueryGroupId,
    ) -> Option<dashmap::mapref::one::RefMut<'_, QueryGroupId, QueryGroup>> {
        self.groups.get_mut(&group_id)
    }

    /// Get all subscriber info for a group (for fan-out).
    pub fn get_group_subscribers(&self, group_id: QueryGroupId) -> Vec<(SessionId, String)> {
        let subscriber_ids: Vec<SubscriberId> = self
            .groups
            .get(&group_id)
            .map(|g| g.subscribers.clone())
            .unwrap_or_default();

        let store = self.subscribers.lock().unwrap_or_else(|e| {
            tracing::error!("Subscriber store lock was poisoned, recovering");
            e.into_inner()
        });
        subscriber_ids
            .iter()
            .filter_map(|sid| {
                store
                    .get(sid.0 as usize)
                    .map(|s| (s.session_id, s.client_sub_id.clone()))
            })
            .collect()
    }

    /// Update a group after re-execution. Extends the table index with any
    /// runtime-discovered tables from the read set that weren't in the
    /// compile-time `table_deps`.
    pub fn update_group(&self, group_id: QueryGroupId, read_set: ReadSet, result_hash: String) {
        if let Some(mut group) = self.groups.get_mut(&group_id) {
            for table in &read_set.tables {
                let already_indexed = group.table_deps.iter().any(|t| *t == table);
                if !already_indexed {
                    self.table_index
                        .entry(table.clone())
                        .or_default()
                        .insert(group_id);
                }
            }
            group.record_execution(read_set, result_hash);
        }
    }

    /// Get subscription counts.
    pub fn counts(&self) -> SubscriptionCounts {
        let total_subscribers: usize = self.groups.iter().map(|g| g.subscribers.len()).sum();
        let groups_count = self.groups.len();
        let sessions_count = self.session_subscribers.len();
        let indexed_tables = self.table_index.len();

        // Estimate memory usage:
        // - Each QueryGroup: ~256 bytes (name, args, auth, read_set, subscribers vec)
        // - Each subscriber entry in the store: ~128 bytes
        // - Each session mapping entry: ~64 bytes + subscriber ID vec
        // - Each table index entry: ~64 bytes + HashSet overhead
        let estimated_bytes = (groups_count * 256)
            + (total_subscribers * 128)
            + (sessions_count * 64)
            + (indexed_tables * 96);

        SubscriptionCounts {
            total: total_subscribers,
            unique_queries: groups_count,
            sessions: sessions_count,
            indexed_tables,
            memory_bytes: estimated_bytes,
        }
    }

    /// Get group count.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Snapshot every active query group ID. Used by the reactor's periodic
    /// resync sweep: PG drops `LISTEN/NOTIFY` payloads when listeners are
    /// slow, so we re-evaluate every group on a timer to recover from drops.
    pub fn all_group_ids(&self) -> Vec<QueryGroupId> {
        self.groups.iter().map(|entry| entry.id).collect()
    }
}

/// Subscription count statistics.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionCounts {
    pub total: usize,
    pub unique_queries: usize,
    pub sessions: usize,
    pub indexed_tables: usize,
    pub memory_bytes: usize,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use forge_core::function::AuthContext;

    #[test]
    fn test_session_manager_create() {
        let node_id = NodeId::new();
        let manager = SessionManager::new(node_id);

        let session = manager.create_session();
        assert!(session.is_connected());

        let retrieved = manager.get_session(session.id);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_session_manager_disconnect() {
        let node_id = NodeId::new();
        let manager = SessionManager::new(node_id);

        let session = manager.create_session();
        manager.disconnect_session(session.id);

        let retrieved = manager.get_session(session.id).unwrap();
        assert!(!retrieved.is_connected());
    }

    #[test]
    fn test_subscription_manager_create() {
        let manager = SubscriptionManager::new(50);
        let session_id = SessionId::new();
        let auth = AuthContext::unauthenticated();

        let (group_id, _sub_id, is_new) = manager
            .subscribe(
                session_id,
                "sub-1".to_string(),
                "get_projects",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        assert!(is_new);
        assert!(manager.get_group(group_id).is_some());
    }

    #[test]
    fn test_subscription_manager_coalescing() {
        let manager = SubscriptionManager::new(50);
        let session1 = SessionId::new();
        let session2 = SessionId::new();
        let auth = AuthContext::unauthenticated();

        let (g1, _, is_new1) = manager
            .subscribe(
                session1,
                "s1".to_string(),
                "get_projects",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        let (g2, _, is_new2) = manager
            .subscribe(
                session2,
                "s2".to_string(),
                "get_projects",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        assert!(is_new1);
        assert!(!is_new2);
        assert_eq!(g1, g2);

        // Group should have 2 subscribers
        let subs = manager.get_group_subscribers(g1);
        assert_eq!(subs.len(), 2);
    }

    #[test]
    fn test_subscription_manager_limit() {
        let manager = SubscriptionManager::new(2);
        let session_id = SessionId::new();
        let auth = AuthContext::unauthenticated();

        manager
            .subscribe(
                session_id,
                "s1".to_string(),
                "q1",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        manager
            .subscribe(
                session_id,
                "s2".to_string(),
                "q2",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        let result = manager.subscribe(
            session_id,
            "s3".to_string(),
            "q3",
            &serde_json::json!({}),
            &auth,
            &[],
            &[],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_subscription_manager_remove_session() {
        let manager = SubscriptionManager::new(50);
        let session_id = SessionId::new();
        let auth = AuthContext::unauthenticated();

        manager
            .subscribe(
                session_id,
                "s1".to_string(),
                "q1",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        manager
            .subscribe(
                session_id,
                "s2".to_string(),
                "q2",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        let counts = manager.counts();
        assert_eq!(counts.total, 2);

        manager.remove_session_subscriptions(session_id);

        let counts = manager.counts();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.unique_queries, 0);
    }

    #[test]
    fn test_table_index_lookup() {
        let manager = SubscriptionManager::new(50);
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        let auth = AuthContext::unauthenticated();

        // Two queries reading different tables
        static PROJECTS: &[&str] = &["projects"];
        static USERS: &[&str] = &["users"];

        let (g1, _, _) = manager
            .subscribe(s1, "a".into(), "get_projects", &serde_json::json!({}), &auth, PROJECTS, &[])
            .unwrap();
        let (g2, _, _) = manager
            .subscribe(s2, "b".into(), "get_users", &serde_json::json!({}), &auth, USERS, &[])
            .unwrap();

        // Change on projects should only find g1
        let change = Change::new("projects", forge_core::realtime::ChangeOperation::Insert);
        let affected = manager.find_affected_groups(&change);
        assert_eq!(affected, vec![g1]);

        // Change on users should only find g2
        let change = Change::new("users", forge_core::realtime::ChangeOperation::Update);
        let affected = manager.find_affected_groups(&change);
        assert_eq!(affected, vec![g2]);

        // Change on unrelated table should find nothing
        let change = Change::new("orders", forge_core::realtime::ChangeOperation::Delete);
        let affected = manager.find_affected_groups(&change);
        assert!(affected.is_empty());

        assert_eq!(manager.counts().indexed_tables, 2);
    }

    #[test]
    fn test_table_index_cleanup_on_unsubscribe() {
        let manager = SubscriptionManager::new(50);
        let session = SessionId::new();
        let auth = AuthContext::unauthenticated();
        static PROJECTS: &[&str] = &["projects"];

        let (_, sub_id, _) = manager
            .subscribe(session, "a".into(), "get_projects", &serde_json::json!({}), &auth, PROJECTS, &[])
            .unwrap();

        assert_eq!(manager.counts().indexed_tables, 1);

        manager.unsubscribe(sub_id);

        // Table index should be cleaned up when the last group for that table is removed
        assert_eq!(manager.counts().indexed_tables, 0);

        let change = Change::new("projects", forge_core::realtime::ChangeOperation::Insert);
        assert!(manager.find_affected_groups(&change).is_empty());
    }
}

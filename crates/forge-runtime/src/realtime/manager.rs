use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use dashmap::DashMap;

use forge_core::function::AuthContext;
use forge_core::realtime::{
    AuthScope, Change, QueryGroup, QueryGroupId, ReadSet, SessionId, Subscriber, SubscriberId,
    SubscriptionId, SubscriptionKey,
};

/// Group-based subscription manager using sharded concurrent data structures.
///
/// Primary index: groups by QueryGroupId (DashMap, 64 shards).
/// Secondary: lookup key -> QueryGroupId for dedup.
/// Subscribers stored in a DashMap for concurrent O(1) insert/remove.
/// Session -> subscribers mapping for cleanup.
pub struct SubscriptionManager {
    /// Query groups indexed by ID. Sharded for concurrent access.
    groups: DashMap<QueryGroupId, QueryGroup>,
    /// Lookup: hash(query_name+args+auth_scope) -> QueryGroupId for dedup.
    group_lookup: DashMap<SubscriptionKey, QueryGroupId>,
    /// Inverted index: table name -> set of group IDs that depend on that table.
    /// Maintained on subscribe/unsubscribe for O(1) affected-group lookups.
    table_index: DashMap<String, HashSet<QueryGroupId>>,
    /// Subscribers indexed by auto-incrementing key. DashMap for lock-free access.
    subscribers: DashMap<usize, Subscriber>,
    /// Reverse index from public `SubscriptionId` to the internal subscriber key.
    /// Lets `unsubscribe` skip an O(n) scan of `subscribers`.
    subscription_to_key: DashMap<SubscriptionId, usize>,
    /// Monotonic counter for subscriber keys.
    next_subscriber_key: AtomicUsize,
    /// Session -> subscriber IDs for fast cleanup on disconnect.
    session_subscribers: DashMap<SessionId, Vec<SubscriberId>>,
    /// Monotonic counter for group IDs.
    next_group_id: AtomicU32,
    /// Maximum subscriptions per session.
    max_per_session: usize,
    /// Maximum serialized-result bytes to cache in `QueryGroup::last_result`.
    /// Results larger than this are not cached; new group members will
    /// re-execute the query instead of receiving a pre-cached value.
    max_cached_result_bytes: usize,
}

impl SubscriptionManager {
    /// Create a new subscription manager.
    pub fn new(max_per_session: usize) -> Self {
        Self::with_shard_count(max_per_session, 64)
    }

    /// Create a new subscription manager with a custom shard count.
    pub fn with_shard_count(max_per_session: usize, shard_count: usize) -> Self {
        Self::with_config(max_per_session, shard_count, 10_485_760)
    }

    /// Create a new subscription manager with full configuration.
    ///
    /// `max_cached_result_bytes` caps the serialized size of results stored in
    /// `QueryGroup::last_result`. Results that exceed this limit are not cached;
    /// new subscribers joining the group re-execute the query instead.
    pub fn with_config(
        max_per_session: usize,
        shard_count: usize,
        max_cached_result_bytes: usize,
    ) -> Self {
        Self {
            groups: DashMap::with_shard_amount(shard_count),
            group_lookup: DashMap::with_shard_amount(shard_count),
            table_index: DashMap::with_shard_amount(shard_count),
            subscribers: DashMap::with_shard_amount(shard_count),
            subscription_to_key: DashMap::with_shard_amount(shard_count),
            next_subscriber_key: AtomicUsize::new(0),
            session_subscribers: DashMap::with_shard_amount(shard_count),
            next_group_id: AtomicU32::new(0),
            max_per_session,
            max_cached_result_bytes,
        }
    }

    /// Remove a group from the inverted table index using pre-extracted data.
    /// Called after releasing the group guard to avoid cross-shard deadlocks.
    fn remove_group_from_table_index(
        &self,
        group_id: QueryGroupId,
        table_deps: &[&str],
        runtime_tables: &[String],
    ) {
        let runtime_iter = runtime_tables.iter().map(String::as_str);
        for table in table_deps.iter().copied().chain(runtime_iter) {
            if let Some(mut set) = self.table_index.get_mut(table) {
                set.remove(&group_id);
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
                last_result: None,
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
        let key = self.next_subscriber_key.fetch_add(1, Ordering::Relaxed);
        let subscriber_id = SubscriberId(key as u32);
        self.subscribers.insert(
            key,
            Subscriber {
                id: subscriber_id,
                session_id,
                client_sub_id,
                group_id,
                subscription_id,
            },
        );
        self.subscription_to_key.insert(subscription_id, key);

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
        let Some((_, key)) = self.subscription_to_key.remove(&subscription_id) else {
            return;
        };
        let Some((_, sub)) = self.subscribers.remove(&key) else {
            return;
        };
        let subscriber_id = sub.id;
        let group_id = sub.group_id;
        let session_id = sub.session_id;

        // Collect eviction data while holding the group guard, then release
        // before touching table_index to avoid cross-shard deadlocks.
        let eviction_data = if let Some(mut group) = self.groups.get_mut(&group_id) {
            group.subscribers.retain(|s| *s != subscriber_id);

            if group.subscribers.is_empty() {
                let lookup_key = QueryGroup::compute_lookup_key(
                    &group.query_name,
                    &group.args,
                    &group.auth_scope,
                );
                let table_deps: Vec<&'static str> = group.table_deps.to_vec();
                let runtime_tables: Vec<String> =
                    group.read_set.tables.to_vec();
                let gid = group.id;
                drop(group);
                Some((lookup_key, table_deps, runtime_tables, gid))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((lookup_key, table_deps, runtime_tables, gid)) = eviction_data {
            self.remove_group_from_table_index(gid, &table_deps, &runtime_tables);
            self.groups.remove(&gid);
            self.group_lookup.remove(&lookup_key);
        }

        if let Some(mut session_subs) = self.session_subscribers.get_mut(&session_id) {
            session_subs.retain(|s| *s != subscriber_id);
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

        for sid in subscriber_ids {
            let key = sid.0 as usize;
            if let Some((_, sub)) = self.subscribers.remove(&key) {
                self.subscription_to_key.remove(&sub.subscription_id);
                removed_sub_ids.push(sub.subscription_id);

                // Collect eviction data while holding the group guard, then release
                // before touching table_index to avoid cross-shard deadlocks.
                let eviction_data =
                    if let Some(mut group) = self.groups.get_mut(&sub.group_id) {
                        group.subscribers.retain(|s| *s != sid);

                        if group.subscribers.is_empty() {
                            let lookup_key = QueryGroup::compute_lookup_key(
                                &group.query_name,
                                &group.args,
                                &group.auth_scope,
                            );
                            let table_deps: Vec<&'static str> = group.table_deps.to_vec();
                            let runtime_tables: Vec<String> =
                                group.read_set.tables.to_vec();
                            let gid = group.id;
                            drop(group);
                            Some((lookup_key, table_deps, runtime_tables, gid))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                if let Some((lookup_key, table_deps, runtime_tables, gid)) = eviction_data {
                    self.remove_group_from_table_index(gid, &table_deps, &runtime_tables);
                    self.groups.remove(&gid);
                    self.group_lookup.remove(&lookup_key);
                }
            }
        }

        removed_sub_ids
    }

    /// Find all groups affected by a change via the inverted table index.
    /// O(groups_for_table) with column-level filtering, not O(all_groups).
    pub fn find_affected_groups(&self, change: &Change) -> Vec<QueryGroupId> {
        let Some(set) = self.table_index.get(&change.table) else {
            return Vec::new();
        };

        // Iterate the table index under its read guard and filter directly.
        // `self.groups.get(gid)` acquires a shard lock on a different DashMap,
        // so there is no deadlock risk. This avoids materialising an
        // intermediate Vec of all candidate IDs before filtering.
        set.iter()
            .copied()
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

        subscriber_ids
            .iter()
            .filter_map(|sid| {
                self.subscribers
                    .get(&(sid.0 as usize))
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

    /// Update a group after re-execution, caching the result data for new
    /// subscribers that join the group before the next invalidation.
    ///
    /// If the serialized result exceeds `max_cached_result_bytes`, the data is
    /// not stored in `last_result`. The hash is still recorded so delta
    /// detection works correctly; new subscribers joining the group will
    /// re-execute the query rather than receiving a cached value.
    pub fn update_group_with_data(
        &self,
        group_id: QueryGroupId,
        read_set: ReadSet,
        result_hash: String,
        data: std::sync::Arc<serde_json::Value>,
        serialized_len: usize,
    ) {
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

            if serialized_len > self.max_cached_result_bytes {
                tracing::debug!(
                    query = %group.query_name,
                    result_bytes = serialized_len,
                    limit_bytes = self.max_cached_result_bytes,
                    "Result too large to cache; new subscribers will re-execute"
                );
                group.record_execution(read_set, result_hash);
            } else {
                group.record_execution_with_data(read_set, result_hash, data);
            }
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
    use uuid::Uuid;

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
            .subscribe(
                s1,
                "a".into(),
                "get_projects",
                &serde_json::json!({}),
                &auth,
                PROJECTS,
                &[],
            )
            .unwrap();
        let (g2, _, _) = manager
            .subscribe(
                s2,
                "b".into(),
                "get_users",
                &serde_json::json!({}),
                &auth,
                USERS,
                &[],
            )
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
            .subscribe(
                session,
                "a".into(),
                "get_projects",
                &serde_json::json!({}),
                &auth,
                PROJECTS,
                &[],
            )
            .unwrap();

        assert_eq!(manager.counts().indexed_tables, 1);

        manager.unsubscribe(sub_id);

        // Table index should be cleaned up when the last group for that table is removed
        assert_eq!(manager.counts().indexed_tables, 0);

        let change = Change::new("projects", forge_core::realtime::ChangeOperation::Insert);
        assert!(manager.find_affected_groups(&change).is_empty());
    }

    // --- Additional coverage: tenant isolation, args isolation, ref-counted
    // group eviction, update_group runtime-table indexing, cache size limit,
    // get_group_subscribers fan-out, all_group_ids snapshot, column-filtered
    // invalidation, and session-removal return value.

    fn authed(user_id: Uuid, roles: Vec<&str>) -> AuthContext {
        AuthContext::authenticated(
            user_id,
            roles.into_iter().map(String::from).collect(),
            std::collections::HashMap::new(),
        )
    }

    #[test]
    fn subscribe_does_not_coalesce_across_different_users() {
        // Same query + args, different principal → different groups. This is
        // the row-level security invariant: alice and bob must not share a
        // cached result, even when they ran the same query.
        let manager = SubscriptionManager::new(50);
        let alice = authed(Uuid::new_v4(), vec![]);
        let bob = authed(Uuid::new_v4(), vec![]);
        let s1 = SessionId::new();
        let s2 = SessionId::new();

        let (g_alice, _, is_new_alice) = manager
            .subscribe(
                s1,
                "a".into(),
                "list_orders",
                &serde_json::json!({}),
                &alice,
                &[],
                &[],
            )
            .unwrap();
        let (g_bob, _, is_new_bob) = manager
            .subscribe(
                s2,
                "b".into(),
                "list_orders",
                &serde_json::json!({}),
                &bob,
                &[],
                &[],
            )
            .unwrap();

        assert!(is_new_alice);
        assert!(is_new_bob);
        assert_ne!(g_alice, g_bob);
    }

    #[test]
    fn subscribe_does_not_coalesce_when_roles_differ() {
        // Same principal, different role set → different group. Roles can
        // affect what rows the query returns, so they're part of the dedup key.
        let manager = SubscriptionManager::new(50);
        let uid = Uuid::new_v4();
        let viewer = authed(uid, vec!["viewer"]);
        let admin = authed(uid, vec!["admin", "viewer"]);
        let s1 = SessionId::new();
        let s2 = SessionId::new();

        let (g_viewer, _, _) = manager
            .subscribe(
                s1,
                "a".into(),
                "list_orders",
                &serde_json::json!({}),
                &viewer,
                &[],
                &[],
            )
            .unwrap();
        let (g_admin, _, _) = manager
            .subscribe(
                s2,
                "b".into(),
                "list_orders",
                &serde_json::json!({}),
                &admin,
                &[],
                &[],
            )
            .unwrap();
        assert_ne!(g_viewer, g_admin);
    }

    #[test]
    fn subscribe_does_not_coalesce_with_different_args() {
        // Args are part of the dedup key — `{"status": "open"}` and
        // `{"status": "closed"}` are two distinct subscriptions.
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        let (g_open, _, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "list_orders",
                &serde_json::json!({"status": "open"}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        let (g_closed, _, _) = manager
            .subscribe(
                SessionId::new(),
                "b".into(),
                "list_orders",
                &serde_json::json!({"status": "closed"}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        assert_ne!(g_open, g_closed);
    }

    #[test]
    fn unsubscribe_keeps_group_alive_while_other_subscribers_remain() {
        // Group eviction is ref-counted by subscriber count, not by name —
        // two subscribers, removing one must not delete the group or its
        // table index entry.
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        static T: &[&str] = &["t"];

        let (g1, sub1, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                T,
                &[],
            )
            .unwrap();
        let (g2, _sub2, _) = manager
            .subscribe(
                SessionId::new(),
                "b".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                T,
                &[],
            )
            .unwrap();
        assert_eq!(g1, g2);

        manager.unsubscribe(sub1);

        // Group must still be present; table_index too.
        assert!(manager.get_group(g1).is_some());
        assert_eq!(manager.counts().indexed_tables, 1);
        assert_eq!(manager.get_group_subscribers(g1).len(), 1);
    }

    #[test]
    fn unsubscribe_is_noop_for_unknown_subscription_id() {
        // Calling unsubscribe on a SubscriptionId we never inserted must not
        // panic or corrupt state.
        let manager = SubscriptionManager::new(50);
        let phantom = forge_core::realtime::SubscriptionId::new();
        manager.unsubscribe(phantom);
        assert_eq!(manager.counts().total, 0);
    }

    #[test]
    fn get_group_subscribers_returns_session_and_client_sub_id_tuples() {
        // Used by the fan-out path. The mapping must round-trip exactly the
        // `(SessionId, client_sub_id)` pair we subscribed with.
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        let session = SessionId::new();
        let (group_id, _, _) = manager
            .subscribe(
                session,
                "my-client-sub".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        let subs = manager.get_group_subscribers(group_id);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].0, session);
        assert_eq!(subs[0].1, "my-client-sub");
    }

    #[test]
    fn all_group_ids_returns_every_active_group() {
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        let (g1, _, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "q1",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        let (g2, _, _) = manager
            .subscribe(
                SessionId::new(),
                "b".into(),
                "q2",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        let mut ids = manager.all_group_ids();
        ids.sort_by_key(|g| g.0);
        let mut want = vec![g1, g2];
        want.sort_by_key(|g| g.0);
        assert_eq!(ids, want);
    }

    #[test]
    fn update_group_extends_table_index_with_runtime_discovered_tables() {
        // The compile-time `table_deps` list might miss tables reached via
        // dynamic SQL or sub-functions. `update_group` is the bridge: after
        // the first execution the runtime read_set has to merge into the
        // table index so subsequent changes to those tables also invalidate.
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        static PROJECTS: &[&str] = &["projects"];

        let (group_id, _, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                PROJECTS,
                &[],
            )
            .unwrap();

        // Runtime discovers that the query also touched `tasks`.
        let mut rs = ReadSet::new();
        rs.tables.push("tasks".to_string());
        manager.update_group(group_id, rs, "hash1".to_string());

        let change = Change::new("tasks", forge_core::realtime::ChangeOperation::Update);
        assert_eq!(manager.find_affected_groups(&change), vec![group_id]);
    }

    #[test]
    fn update_group_with_data_skips_cache_when_result_exceeds_limit() {
        // With a 64-byte cache cap, a large payload must record the hash
        // for delta detection but leave `last_result` empty so new joiners
        // re-execute instead of receiving stale data from the cache.
        let manager = SubscriptionManager::with_config(50, 64, 64);
        let auth = AuthContext::unauthenticated();
        let (group_id, _, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        // Payload comfortably > 64 bytes when serialised.
        let big = std::sync::Arc::new(serde_json::json!({
            "items": vec!["row-with-padding-to-blow-past-the-cache-limit"; 8]
        }));
        let serialized_len = serde_json::to_vec(&*big).unwrap().len();
        manager.update_group_with_data(
            group_id,
            ReadSet::new(),
            "hash-big".to_string(),
            big.clone(),
            serialized_len,
        );

        let group = manager.get_group(group_id).expect("group");
        assert_eq!(group.last_result_hash.as_deref(), Some("hash-big"));
        assert!(
            group.last_result.is_none(),
            "oversize results must not be cached"
        );
    }

    #[test]
    fn update_group_with_data_caches_when_result_fits_under_limit() {
        let manager = SubscriptionManager::with_config(50, 64, 10_485_760);
        let auth = AuthContext::unauthenticated();
        let (group_id, _, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        let small = std::sync::Arc::new(serde_json::json!({"ok": true}));
        let serialized_len = serde_json::to_vec(&*small).unwrap().len();
        manager.update_group_with_data(
            group_id,
            ReadSet::new(),
            "hash-small".to_string(),
            small.clone(),
            serialized_len,
        );

        let group = manager.get_group(group_id).expect("group");
        assert_eq!(group.last_result_hash.as_deref(), Some("hash-small"));
        assert!(group.last_result.is_some());
    }

    #[test]
    fn find_affected_groups_filters_by_selected_columns_for_updates() {
        // `selected_cols = ["name"]` means an UPDATE that only touched
        // `email` shouldn't invalidate the group. INSERTs always do, since
        // they change row presence regardless of columns.
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        static USERS: &[&str] = &["users"];
        static COLS: &[&str] = &["name"];

        let (group_id, _, _) = manager
            .subscribe(
                SessionId::new(),
                "a".into(),
                "q",
                &serde_json::json!({}),
                &auth,
                USERS,
                COLS,
            )
            .unwrap();

        let unrelated = Change::new("users", forge_core::realtime::ChangeOperation::Update)
            .with_columns(vec!["email".to_string()]);
        assert!(manager.find_affected_groups(&unrelated).is_empty());

        let relevant = Change::new("users", forge_core::realtime::ChangeOperation::Update)
            .with_columns(vec!["name".to_string()]);
        assert_eq!(manager.find_affected_groups(&relevant), vec![group_id]);

        // Inserts always invalidate (row presence changes).
        let insert = Change::new("users", forge_core::realtime::ChangeOperation::Insert)
            .with_columns(vec!["email".to_string()]);
        assert_eq!(manager.find_affected_groups(&insert), vec![group_id]);
    }

    #[test]
    fn remove_session_subscriptions_returns_subscription_ids_for_cleanup() {
        // The session server uses the returned IDs to tell other components
        // (channels, presence, etc.) which subscriptions just went away.
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        let session = SessionId::new();
        let (_, sub1, _) = manager
            .subscribe(
                session,
                "a".into(),
                "q1",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();
        let (_, sub2, _) = manager
            .subscribe(
                session,
                "b".into(),
                "q2",
                &serde_json::json!({}),
                &auth,
                &[],
                &[],
            )
            .unwrap();

        let mut removed = manager.remove_session_subscriptions(session);
        removed.sort_by_key(|s| s.0);
        let mut want = vec![sub1, sub2];
        want.sort_by_key(|s| s.0);
        assert_eq!(removed, want);

        // And a follow-up call returns nothing — the session is gone.
        assert!(manager.remove_session_subscriptions(session).is_empty());
    }

    #[test]
    fn counts_reflects_groups_sessions_subscribers_and_tables() {
        let manager = SubscriptionManager::new(50);
        let auth = AuthContext::unauthenticated();
        static USERS: &[&str] = &["users"];
        static ORDERS: &[&str] = &["orders"];

        let session_a = SessionId::new();
        let session_b = SessionId::new();
        // Two subscribers in session_a sharing one group (coalesced)
        manager
            .subscribe(
                session_a,
                "1".into(),
                "list_users",
                &serde_json::json!({}),
                &auth,
                USERS,
                &[],
            )
            .unwrap();
        manager
            .subscribe(
                session_a,
                "2".into(),
                "list_orders",
                &serde_json::json!({}),
                &auth,
                ORDERS,
                &[],
            )
            .unwrap();
        // session_b shares the list_users group with session_a → same group, +1 subscriber
        manager
            .subscribe(
                session_b,
                "3".into(),
                "list_users",
                &serde_json::json!({}),
                &auth,
                USERS,
                &[],
            )
            .unwrap();

        let counts = manager.counts();
        assert_eq!(counts.total, 3);
        assert_eq!(counts.unique_queries, 2);
        assert_eq!(counts.sessions, 2);
        assert_eq!(counts.indexed_tables, 2);
        assert!(counts.memory_bytes > 0);
    }
}

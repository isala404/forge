use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;

use forge_core::error::{ForgeError, Result};

/// Hard cap on the value size accepted by `set` / `set_if_absent`. PG can
/// store much larger BYTEA blobs, but the KV API is meant for small
/// configuration / lock payloads, not blobs. Multi-MB values are almost
/// always a misuse (and round-trip the protocol per call).
const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// PostgreSQL-backed key-value store.
///
/// Provides a simple get/set/delete/set_if_absent/increment API over
/// `forge_kv` and `forge_kv_counters` tables. All operations are atomic.
/// TTLs are enforced both at read time (expired keys return `None`) and
/// via periodic cleanup.
///
/// Keys are automatically namespaced with the configured prefix to prevent
/// collisions between different subsystems sharing the same database.
pub struct KvStore {
    pool: PgPool,
    namespace: &'static str,
}

impl KvStore {
    pub fn new(pool: PgPool, namespace: &'static str) -> Self {
        Self { pool, namespace }
    }

    fn prefixed_key(&self, key: &str) -> Result<String> {
        // Reject `:` in either namespace or key — the prefix separator must
        // be unambiguous so `(namespace=a, key=b:foo)` and
        // `(namespace=a:b, key=foo)` can't collide on the same physical key.
        if self.namespace.contains(':') {
            return Err(ForgeError::InvalidArgument(format!(
                "kv namespace must not contain ':' (got {:?})",
                self.namespace
            )));
        }
        if key.contains(':') {
            return Err(ForgeError::InvalidArgument(
                "kv key must not contain ':' (reserved as namespace separator)".to_string(),
            ));
        }
        Ok(format!("{}:{}", self.namespace, key))
    }

    fn check_value_size(value: &[u8]) -> Result<()> {
        if value.len() > MAX_VALUE_BYTES {
            return Err(ForgeError::InvalidArgument(format!(
                "kv value exceeds {MAX_VALUE_BYTES} byte limit (got {})",
                value.len()
            )));
        }
        Ok(())
    }

    /// Get a value by key. Returns `None` if the key doesn't exist or is expired.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let full_key = self.prefixed_key(key)?;
        let row = sqlx::query_scalar!(
            r#"
            SELECT value
            FROM forge_kv
            WHERE key = $1
              AND (expires_at IS NULL OR expires_at > NOW())
            "#,
            full_key,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(row)
    }

    /// Set a key to a value. Overwrites any existing value.
    pub async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        Self::check_value_size(value)?;
        let full_key = self.prefixed_key(key)?;
        let expires_at = ttl.map(|d| Utc::now() + d);
        sqlx::query!(
            r#"
            INSERT INTO forge_kv (key, value, expires_at, updated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (key)
            DO UPDATE SET value = $2, expires_at = $3, updated_at = NOW()
            "#,
            full_key,
            value,
            expires_at,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }

    /// Set a key only if it doesn't already exist (or is expired).
    /// Returns `true` if the key was set, `false` if it already existed.
    ///
    /// Uses `ON CONFLICT DO UPDATE ... WHERE` to atomically treat expired rows
    /// as absent within a single statement (no CTE snapshot isolation issues).
    pub async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool> {
        Self::check_value_size(value)?;
        let full_key = self.prefixed_key(key)?;
        let expires_at = ttl.map(|d| Utc::now() + d);
        // Serialize concurrent reclaim-of-expired racers per key via a
        // transaction-scoped advisory lock keyed on the full prefixed key.
        // Without this, the ON CONFLICT WHERE branch is only race-free under
        // READ COMMITTED — under REPEATABLE READ a second writer can see the
        // pre-update snapshot and "succeed" against an already-claimed row.
        let mut tx = self.pool.begin().await.map_err(ForgeError::Database)?;
        #[allow(clippy::disallowed_methods)]
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(&full_key)
            .execute(&mut *tx)
            .await
            .map_err(ForgeError::Database)?;
        #[allow(clippy::disallowed_methods)]
        let rows = sqlx::query(
            r#"
            INSERT INTO forge_kv (key, value, expires_at, updated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (key) DO UPDATE
                SET value = $2, expires_at = $3, updated_at = NOW()
                WHERE forge_kv.expires_at IS NOT NULL AND forge_kv.expires_at <= NOW()
            "#,
        )
        .bind(&full_key)
        .bind(value)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(ForgeError::Database)?
        .rows_affected();
        tx.commit().await.map_err(ForgeError::Database)?;

        Ok(rows > 0)
    }

    /// Delete a key. Returns `true` if the key existed.
    pub async fn delete(&self, key: &str) -> Result<bool> {
        let full_key = self.prefixed_key(key)?;
        let result = sqlx::query!("DELETE FROM forge_kv WHERE key = $1", full_key)
            .execute(&self.pool)
            .await
            .map_err(ForgeError::Database)?;

        Ok(result.rows_affected() > 0)
    }

    /// Atomically increment a counter by `delta`. Creates the counter at 0 if
    /// it doesn't exist. Returns the new value. When `ttl` is `None`, an
    /// existing counter's TTL is preserved (pass `Some` to override it).
    /// Expired counters are treated as non-existent (value resets to delta).
    ///
    /// Uses `ON CONFLICT DO UPDATE ... WHERE` to handle expired rows atomically
    /// without CTE snapshot isolation issues.
    ///
    /// **Note:** counter storage is `BIGINT`, range `[-2^63, 2^63 - 1]`.
    /// Increments that overflow surface as `ForgeError::InvalidArgument` so
    /// callers can choose to reset rather than retry indefinitely.
    pub async fn increment(&self, key: &str, delta: i64, ttl: Option<Duration>) -> Result<i64> {
        let full_key = self.prefixed_key(key)?;
        let expires_at = ttl.map(|d| Utc::now() + d);
        // Expired counters reset to delta rather than accumulating.
        // Convert to query_scalar!() after next `cargo sqlx prepare`.
        #[allow(clippy::disallowed_methods)]
        let row: std::result::Result<(i64,), sqlx::Error> = sqlx::query_as(
            r#"
            INSERT INTO forge_kv_counters (key, value, expires_at, updated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (key)
            DO UPDATE SET
                value = CASE
                    WHEN forge_kv_counters.expires_at IS NOT NULL AND forge_kv_counters.expires_at <= NOW()
                    THEN $2
                    ELSE forge_kv_counters.value + $2
                END,
                expires_at = COALESCE($3, forge_kv_counters.expires_at),
                updated_at = NOW()
            RETURNING value
            "#,
        )
        .bind(&full_key)
        .bind(delta)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await;

        match row {
            Ok((v,)) => Ok(v),
            Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("22003") => {
                Err(ForgeError::InvalidArgument(format!(
                    "counter overflow at key {full_key:?}: BIGINT range exceeded"
                )))
            }
            Err(e) => Err(ForgeError::Database(e)),
        }
    }

    /// Read a counter's current value. Returns `None` if missing or expired.
    ///
    /// Mirrors the TTL filter from `get()` so an expired counter behaves the
    /// same as a missing one — keeps a future generic `get` over both tables
    /// consistent.
    pub async fn get_counter(&self, key: &str) -> Result<Option<i64>> {
        let full_key = self.prefixed_key(key)?;
        #[allow(clippy::disallowed_methods)]
        let row: Option<(i64,)> = sqlx::query_as(
            r#"
            SELECT value
            FROM forge_kv_counters
            WHERE key = $1
              AND (expires_at IS NULL OR expires_at > NOW())
            "#,
        )
        .bind(&full_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(row.map(|(v,)| v))
    }

    /// Remove expired keys from both tables. Returns total rows cleaned up.
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let kv_deleted = sqlx::query!(
            "DELETE FROM forge_kv WHERE expires_at IS NOT NULL AND expires_at <= NOW()"
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?
        .rows_affected();

        let counter_deleted = sqlx::query!(
            "DELETE FROM forge_kv_counters WHERE expires_at IS NOT NULL AND expires_at <= NOW()"
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?
        .rows_affected();

        Ok(kv_deleted + counter_deleted)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prefixed_key_combines_namespace_and_key() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("connect_lazy never fails for a syntactically valid URL");

        let store = KvStore::new(pool, "ratelimit");
        // `:` in a key is now rejected — verify that and a couple of
        // representative happy cases.
        assert!(store.prefixed_key("user:42").is_err());
        assert_eq!(store.prefixed_key("user_42").unwrap(), "ratelimit:user_42");
        assert_eq!(store.prefixed_key("").unwrap(), "ratelimit:");
    }

    #[tokio::test]
    async fn prefixed_key_isolates_namespaces() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("connect_lazy never fails for a syntactically valid URL");

        // Same logical key under different namespaces produces distinct
        // physical keys — the property the namespace exists to guarantee.
        let a = KvStore::new(pool.clone(), "subsystem_a");
        let b = KvStore::new(pool, "subsystem_b");
        assert_ne!(
            a.prefixed_key("shared").unwrap(),
            b.prefixed_key("shared").unwrap()
        );
    }
}

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        let db = base
            .isolated(test_name)
            .await
            .expect("Failed to create isolated db");
        let system_sql = crate::pg::migration::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    #[tokio::test]
    async fn get_returns_none_for_missing_key() {
        let db = setup_db("kv_missing").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        assert!(kv.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn set_then_get_roundtrips_bytes() {
        let db = setup_db("kv_roundtrip").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        kv.set("greeting", b"hello, world", None).await.unwrap();
        let got = kv.get("greeting").await.unwrap();
        assert_eq!(got.as_deref(), Some(&b"hello, world"[..]));
    }

    #[tokio::test]
    async fn set_overwrites_existing_value() {
        let db = setup_db("kv_overwrite").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        kv.set("k", b"v1", None).await.unwrap();
        kv.set("k", b"v2", None).await.unwrap();
        assert_eq!(kv.get("k").await.unwrap().as_deref(), Some(&b"v2"[..]));
    }

    #[tokio::test]
    async fn expired_key_returns_none_before_cleanup() {
        let db = setup_db("kv_expired_read").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        // Set with a tiny TTL, sleep past it.
        kv.set("k", b"v", Some(Duration::from_millis(50)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            kv.get("k").await.unwrap().is_none(),
            "expired key must not be returned"
        );
    }

    #[tokio::test]
    async fn delete_returns_true_when_key_existed() {
        let db = setup_db("kv_delete_existing").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        kv.set("k", b"v", None).await.unwrap();
        assert!(kv.delete("k").await.unwrap());
        assert!(kv.get("k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_returns_false_when_key_missing() {
        let db = setup_db("kv_delete_missing").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        assert!(!kv.delete("never_existed").await.unwrap());
    }

    #[tokio::test]
    async fn set_if_absent_inserts_when_missing() {
        let db = setup_db("kv_sia_insert").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        let claimed = kv.set_if_absent("lock", b"owner", None).await.unwrap();
        assert!(claimed);
        assert_eq!(
            kv.get("lock").await.unwrap().as_deref(),
            Some(&b"owner"[..])
        );
    }

    #[tokio::test]
    async fn set_if_absent_refuses_when_present_and_fresh() {
        let db = setup_db("kv_sia_present").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        assert!(kv.set_if_absent("lock", b"alice", None).await.unwrap());
        let second = kv.set_if_absent("lock", b"bob", None).await.unwrap();
        assert!(!second, "second writer must lose the race");
        assert_eq!(
            kv.get("lock").await.unwrap().as_deref(),
            Some(&b"alice"[..]),
            "value must still belong to the first writer"
        );
    }

    #[tokio::test]
    async fn set_if_absent_succeeds_when_existing_value_expired() {
        let db = setup_db("kv_sia_expired").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        kv.set("lock", b"old", Some(Duration::from_millis(50)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            kv.set_if_absent("lock", b"new", None).await.unwrap(),
            "expired key must be reclaimable"
        );
        assert_eq!(kv.get("lock").await.unwrap().as_deref(), Some(&b"new"[..]));
    }

    #[tokio::test]
    async fn increment_creates_counter_at_delta() {
        let db = setup_db("kv_inc_create").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        let v = kv.increment("hits", 5, None).await.unwrap();
        assert_eq!(v, 5);
    }

    #[tokio::test]
    async fn increment_accumulates_across_calls() {
        let db = setup_db("kv_inc_accum").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        assert_eq!(kv.increment("hits", 3, None).await.unwrap(), 3);
        assert_eq!(kv.increment("hits", 7, None).await.unwrap(), 10);
        assert_eq!(kv.increment("hits", -4, None).await.unwrap(), 6);
    }

    #[tokio::test]
    async fn increment_preserves_existing_ttl_when_none_passed() {
        let db = setup_db("kv_inc_ttl_preserve").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        // Seed with a 1h TTL.
        kv.increment("hits", 1, Some(Duration::from_secs(3600)))
            .await
            .unwrap();
        // Increment without specifying TTL — must keep the existing one.
        kv.increment("hits", 1, None).await.unwrap();
        let row: (Option<chrono::DateTime<Utc>>,) =
            sqlx::query_as("SELECT expires_at FROM forge_kv_counters WHERE key = $1")
                .bind("test:hits")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(
            row.0.is_some(),
            "TTL must survive a None increment, got {:?}",
            row.0
        );
    }

    #[tokio::test]
    async fn increment_resets_when_existing_counter_expired() {
        let db = setup_db("kv_inc_reset_expired").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        kv.increment("hits", 100, Some(Duration::from_millis(50)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Expired counter should reset to delta, not accumulate.
        let v = kv.increment("hits", 5, None).await.unwrap();
        assert_eq!(v, 5, "expired counter must reset, not accumulate");
    }

    #[tokio::test]
    async fn cleanup_expired_removes_expired_keys_and_counters() {
        let db = setup_db("kv_cleanup").await;
        let kv = KvStore::new(db.pool().clone(), "test");
        kv.set("fresh", b"keep", Some(Duration::from_secs(3600)))
            .await
            .unwrap();
        kv.set("stale", b"drop", Some(Duration::from_millis(50)))
            .await
            .unwrap();
        kv.increment("counter_fresh", 1, Some(Duration::from_secs(3600)))
            .await
            .unwrap();
        kv.increment("counter_stale", 1, Some(Duration::from_millis(50)))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(150)).await;
        let removed = kv.cleanup_expired().await.unwrap();
        assert_eq!(removed, 2, "cleanup must touch both stale rows");

        assert!(kv.get("fresh").await.unwrap().is_some());
        assert!(kv.get("stale").await.unwrap().is_none());

        let fresh_counter: i64 =
            sqlx::query_scalar("SELECT value FROM forge_kv_counters WHERE key = $1")
                .bind("test:counter_fresh")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(fresh_counter, 1);

        let stale_exists: Option<i64> =
            sqlx::query_scalar("SELECT value FROM forge_kv_counters WHERE key = $1")
                .bind("test:counter_stale")
                .fetch_optional(db.pool())
                .await
                .unwrap();
        assert!(stale_exists.is_none(), "stale counter row must be deleted");
    }

    #[tokio::test]
    async fn namespaced_stores_do_not_see_each_others_keys() {
        let db = setup_db("kv_namespace_isolation").await;
        let a = KvStore::new(db.pool().clone(), "subsys_a");
        let b = KvStore::new(db.pool().clone(), "subsys_b");

        a.set("shared", b"only-a", None).await.unwrap();
        assert_eq!(
            a.get("shared").await.unwrap().as_deref(),
            Some(&b"only-a"[..])
        );
        assert!(
            b.get("shared").await.unwrap().is_none(),
            "namespace b must not see namespace a's key"
        );
    }
}

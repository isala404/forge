use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;

use forge_core::error::{ForgeError, Result};

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
    /// Create a new KV store backed by the given pool with a namespace prefix.
    pub fn new(pool: PgPool, namespace: &'static str) -> Self {
        Self { pool, namespace }
    }

    /// Build the full namespaced key.
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}:{}", self.namespace, key)
    }

    /// Get a value by key. Returns `None` if the key doesn't exist or is expired.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let full_key = self.prefixed_key(key);
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
        let full_key = self.prefixed_key(key);
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
        let full_key = self.prefixed_key(key);
        let expires_at = ttl.map(|d| Utc::now() + d);
        // Runtime query: rewritten to use ON CONFLICT WHERE for atomic expired-row
        // handling. Convert to query!() after next `cargo sqlx prepare`.
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
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?
        .rows_affected();

        Ok(rows > 0)
    }

    /// Delete a key. Returns `true` if the key existed.
    pub async fn delete(&self, key: &str) -> Result<bool> {
        let full_key = self.prefixed_key(key);
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
    pub async fn increment(&self, key: &str, delta: i64, ttl: Option<Duration>) -> Result<i64> {
        let full_key = self.prefixed_key(key);
        let expires_at = ttl.map(|d| Utc::now() + d);
        // Runtime query: rewritten to handle expired counters atomically.
        // Convert to query_scalar!() after next `cargo sqlx prepare`.
        #[allow(clippy::disallowed_methods)]
        let row: (i64,) = sqlx::query_as(
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
        .await
        .map_err(ForgeError::Database)?;

        Ok(row.0)
    }

    /// Remove expired keys from both tables. Returns total rows cleaned up.
    /// Called by the runtime's periodic leader-only maintenance loop.
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
    async fn test_kv_store_construction() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let store = KvStore::new(pool, "test");
        assert_eq!(store.prefixed_key("foo"), "test:foo");
    }
}

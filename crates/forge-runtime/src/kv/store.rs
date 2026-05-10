use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;

use forge_core::error::{ForgeError, Result};

/// PostgreSQL-backed key-value store.
///
/// Provides a simple get/set/delete/increment API over `forge_kv` and
/// `forge_kv_counters` tables. All operations are atomic. TTLs are
/// enforced both at read time (expired keys return `None`) and via
/// periodic cleanup.
pub struct KvStore {
    pool: PgPool,
}

impl KvStore {
    /// Create a new KV store backed by the given pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get a value by key. Returns `None` if the key doesn't exist or is expired.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_scalar!(
            r#"
            SELECT value
            FROM forge_kv
            WHERE key = $1
              AND (expires_at IS NULL OR expires_at > NOW())
            "#,
            key,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(row)
    }

    /// Get a value as a UTF-8 string.
    pub async fn get_string(&self, key: &str) -> Result<Option<String>> {
        match self.get(key).await? {
            Some(bytes) => {
                let s = String::from_utf8(bytes)
                    .map_err(|e| ForgeError::Deserialization(e.to_string()))?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    /// Get a value deserialized from JSON.
    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get(key).await? {
            Some(bytes) => {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|e| ForgeError::Deserialization(e.to_string()))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Set a key to a value. Overwrites any existing value.
    pub async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        let expires_at = ttl.map(|d| Utc::now() + d);
        sqlx::query!(
            r#"
            INSERT INTO forge_kv (key, value, expires_at, updated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (key)
            DO UPDATE SET value = $2, expires_at = $3, updated_at = NOW()
            "#,
            key,
            value,
            expires_at,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }

    /// Set a key to a UTF-8 string value.
    pub async fn set_string(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<()> {
        self.set(key, value.as_bytes(), ttl).await
    }

    /// Set a key to a JSON-serialized value.
    pub async fn set_json<T: serde::Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<()> {
        let bytes =
            serde_json::to_vec(value).map_err(|e| ForgeError::Serialization(e.to_string()))?;
        self.set(key, &bytes, ttl).await
    }

    /// Set a key only if it doesn't already exist (or is expired).
    /// Returns `true` if the key was set, `false` if it already existed.
    pub async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool> {
        let expires_at = ttl.map(|d| Utc::now() + d);
        // Atomic: clear expired then insert in a single statement.
        let rows = sqlx::query!(
            r#"
            WITH cleared AS (
                DELETE FROM forge_kv
                WHERE key = $1 AND expires_at IS NOT NULL AND expires_at <= NOW()
            )
            INSERT INTO forge_kv (key, value, expires_at, updated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (key) DO NOTHING
            "#,
            key,
            value,
            expires_at,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?
        .rows_affected();

        Ok(rows > 0)
    }

    /// Delete a key. Returns `true` if the key existed.
    pub async fn delete(&self, key: &str) -> Result<bool> {
        let result = sqlx::query!("DELETE FROM forge_kv WHERE key = $1", key)
            .execute(&self.pool)
            .await
            .map_err(ForgeError::Database)?;

        Ok(result.rows_affected() > 0)
    }

    /// Atomically increment a counter by `delta`. Creates the counter at 0 if
    /// it doesn't exist. Returns the new value. When `ttl` is `None`, an
    /// existing counter's TTL is preserved (pass `Some` to override it).
    /// Expired counters are reset to 0 before incrementing.
    pub async fn increment(&self, key: &str, delta: i64, ttl: Option<Duration>) -> Result<i64> {
        let expires_at = ttl.map(|d| Utc::now() + d);
        let new_value = sqlx::query_scalar!(
            r#"
            WITH cleared AS (
                DELETE FROM forge_kv_counters
                WHERE key = $1 AND expires_at IS NOT NULL AND expires_at <= NOW()
            )
            INSERT INTO forge_kv_counters (key, value, expires_at, updated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (key)
            DO UPDATE SET
                value = forge_kv_counters.value + $2,
                expires_at = COALESCE($3, forge_kv_counters.expires_at),
                updated_at = NOW()
            RETURNING value
            "#,
            key,
            delta,
            expires_at,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(new_value)
    }

    /// Get a counter value. Returns `None` if the counter doesn't exist or is expired.
    pub async fn get_counter(&self, key: &str) -> Result<Option<i64>> {
        let row = sqlx::query_scalar!(
            r#"
            SELECT value
            FROM forge_kv_counters
            WHERE key = $1
              AND (expires_at IS NULL OR expires_at > NOW())
            "#,
            key,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(row)
    }

    /// Reset a counter to zero. Returns `true` if the counter existed.
    pub async fn reset_counter(&self, key: &str) -> Result<bool> {
        let result = sqlx::query!("DELETE FROM forge_kv_counters WHERE key = $1", key,)
            .execute(&self.pool)
            .await
            .map_err(ForgeError::Database)?;

        Ok(result.rows_affected() > 0)
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

    /// Delete all keys matching a prefix.
    pub async fn delete_prefix(&self, prefix: &str) -> Result<u64> {
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("{escaped}%");
        let kv_deleted = sqlx::query!("DELETE FROM forge_kv WHERE key LIKE $1", pattern,)
            .execute(&self.pool)
            .await
            .map_err(ForgeError::Database)?
            .rows_affected();

        let counter_deleted =
            sqlx::query!("DELETE FROM forge_kv_counters WHERE key LIKE $1", pattern,)
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

        let _store = KvStore::new(pool);
    }
}

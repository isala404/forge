use sqlx::{PgPool, Row};
use chrono::{DateTime, Utc};

use super::generator::Migration;
use forge_core::error::{ForgeError, Result};

/// Executes migrations against a database.
pub struct MigrationExecutor {
    pool: PgPool,
}

impl MigrationExecutor {
    /// Create a new migration executor.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Initialize the migrations table.
    pub async fn init(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS forge_migrations (
                id SERIAL PRIMARY KEY,
                version VARCHAR(255) UNIQUE NOT NULL,
                name VARCHAR(255),
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                checksum VARCHAR(64),
                execution_time_ms INTEGER
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to init migrations table: {}", e)))?;

        Ok(())
    }

    /// Get all applied migrations.
    pub async fn applied_migrations(&self) -> Result<Vec<AppliedMigration>> {
        let rows = sqlx::query(
            r#"
            SELECT version, name, applied_at, checksum, execution_time_ms
            FROM forge_migrations
            ORDER BY version ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to fetch migrations: {}", e)))?;

        let migrations = rows.iter().map(|row| {
            AppliedMigration {
                version: row.get("version"),
                name: row.get("name"),
                applied_at: row.get("applied_at"),
                checksum: row.get("checksum"),
                execution_time_ms: row.get("execution_time_ms"),
            }
        }).collect();

        Ok(migrations)
    }

    /// Check if a migration has been applied.
    pub async fn is_applied(&self, version: &str) -> Result<bool> {
        let row = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM forge_migrations WHERE version = $1",
        )
        .bind(version)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to check migration: {}", e)))?;

        Ok(row > 0)
    }

    /// Apply a migration.
    pub async fn apply(&self, migration: &Migration) -> Result<()> {
        let start = std::time::Instant::now();

        // Execute the migration SQL
        sqlx::query(&migration.sql)
            .execute(&self.pool)
            .await
            .map_err(|e| ForgeError::Database(format!(
                "Failed to apply migration {}: {}",
                migration.version, e
            )))?;

        let elapsed = start.elapsed();

        // Calculate checksum
        let checksum = calculate_checksum(&migration.sql);

        // Record the migration
        sqlx::query(
            r#"
            INSERT INTO forge_migrations (version, name, checksum, execution_time_ms)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(&migration.version)
        .bind(&migration.name)
        .bind(&checksum)
        .bind(elapsed.as_millis() as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!(
            "Failed to record migration {}: {}",
            migration.version, e
        )))?;

        Ok(())
    }

    /// Rollback the last migration.
    pub async fn rollback(&self) -> Result<Option<String>> {
        // Get the last applied migration
        let last = sqlx::query_scalar::<_, String>(
            "SELECT version FROM forge_migrations ORDER BY version DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to get last migration: {}", e)))?;

        match last {
            Some(version) => {
                // Remove from migrations table
                sqlx::query("DELETE FROM forge_migrations WHERE version = $1")
                    .bind(&version)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| ForgeError::Database(format!(
                        "Failed to remove migration record: {}",
                        e
                    )))?;

                Ok(Some(version))
            }
            None => Ok(None),
        }
    }
}

/// A migration that has been applied.
#[derive(Debug, Clone)]
pub struct AppliedMigration {
    pub version: String,
    pub name: Option<String>,
    pub applied_at: DateTime<Utc>,
    pub checksum: Option<String>,
    pub execution_time_ms: Option<i32>,
}

/// Calculate a SHA256 checksum of the migration content.
fn calculate_checksum(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum() {
        let checksum = calculate_checksum("CREATE TABLE users (id UUID);");
        assert_eq!(checksum.len(), 16);

        // Same content should produce same checksum
        let checksum2 = calculate_checksum("CREATE TABLE users (id UUID);");
        assert_eq!(checksum, checksum2);

        // Different content should produce different checksum
        let checksum3 = calculate_checksum("CREATE TABLE posts (id UUID);");
        assert_ne!(checksum, checksum3);
    }
}

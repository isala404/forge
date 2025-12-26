//! Migration runner with mesh-safe locking.
//!
//! Ensures only one node runs migrations at a time using PostgreSQL advisory locks.

use forge_core::error::{ForgeError, Result};
use sqlx::PgPool;
use std::collections::HashSet;
use std::path::Path;
use tracing::{debug, info, warn};

/// Lock ID for migration advisory lock (arbitrary but consistent).
/// Using a fixed value derived from "FORGE" ascii values.
const MIGRATION_LOCK_ID: i64 = 0x464F524745; // "FORGE" in hex

/// A single migration.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Unique name/identifier (e.g., "0001_forge_internal" or "0002_create_users").
    pub name: String,
    /// SQL to execute.
    pub sql: String,
}

impl Migration {
    pub fn new(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql: sql.into(),
        }
    }
}

/// Migration runner that handles both built-in and user migrations.
pub struct MigrationRunner {
    pool: PgPool,
}

impl MigrationRunner {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run all pending migrations with mesh-safe locking.
    ///
    /// This acquires an exclusive advisory lock before running migrations,
    /// ensuring only one node in the cluster runs migrations at a time.
    pub async fn run(&self, user_migrations: Vec<Migration>) -> Result<()> {
        // Acquire exclusive lock (blocks until acquired)
        self.acquire_lock().await?;

        let result = self.run_migrations_inner(user_migrations).await;

        // Always release lock, even on error
        if let Err(e) = self.release_lock().await {
            warn!("Failed to release migration lock: {}", e);
        }

        result
    }

    async fn run_migrations_inner(&self, user_migrations: Vec<Migration>) -> Result<()> {
        // Ensure migration tracking table exists
        self.ensure_migrations_table().await?;

        // Get already-applied migrations
        let applied = self.get_applied_migrations().await?;
        debug!("Already applied migrations: {:?}", applied);

        // Run built-in FORGE migrations first
        let builtin = super::builtin::get_builtin_migrations();
        for migration in builtin {
            if !applied.contains(&migration.name) {
                self.apply_migration(&migration).await?;
            }
        }

        // Then run user migrations
        for migration in user_migrations {
            if !applied.contains(&migration.name) {
                self.apply_migration(&migration).await?;
            }
        }

        Ok(())
    }

    async fn acquire_lock(&self) -> Result<()> {
        debug!("Acquiring migration lock...");
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK_ID)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                ForgeError::Database(format!("Failed to acquire migration lock: {}", e))
            })?;
        debug!("Migration lock acquired");
        Ok(())
    }

    async fn release_lock(&self) -> Result<()> {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_ID)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                ForgeError::Database(format!("Failed to release migration lock: {}", e))
            })?;
        debug!("Migration lock released");
        Ok(())
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS forge_migrations (
                id SERIAL PRIMARY KEY,
                name VARCHAR(255) UNIQUE NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to create migrations table: {}", e)))?;
        Ok(())
    }

    async fn get_applied_migrations(&self) -> Result<HashSet<String>> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM forge_migrations")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                ForgeError::Database(format!("Failed to get applied migrations: {}", e))
            })?;

        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    async fn apply_migration(&self, migration: &Migration) -> Result<()> {
        info!("Applying migration: {}", migration.name);

        // Split migration into individual statements, respecting dollar-quoted strings
        let statements = split_sql_statements(&migration.sql);

        for statement in statements {
            let statement = statement.trim();

            // Skip empty statements or comment-only blocks
            if statement.is_empty()
                || statement.lines().all(|l| {
                    let l = l.trim();
                    l.is_empty() || l.starts_with("--")
                })
            {
                continue;
            }

            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .map_err(|e| {
                    ForgeError::Database(format!(
                        "Failed to apply migration '{}': {}",
                        migration.name, e
                    ))
                })?;
        }

        // Record it as applied
        sqlx::query("INSERT INTO forge_migrations (name) VALUES ($1)")
            .bind(&migration.name)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                ForgeError::Database(format!(
                    "Failed to record migration '{}': {}",
                    migration.name, e
                ))
            })?;

        info!("Migration applied: {}", migration.name);
        Ok(())
    }
}

/// Split SQL into individual statements, respecting dollar-quoted strings.
/// This handles PL/pgSQL functions that contain semicolons inside $$ delimiters.
fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut dollar_tag = String::new();
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        current.push(c);

        // Check for dollar-quoting start/end
        if c == '$' {
            // Look for a dollar-quote tag like $$ or $tag$
            let mut potential_tag = String::from("$");

            // Collect characters until we hit another $ or non-identifier char
            while let Some(&next_c) = chars.peek() {
                if next_c == '$' {
                    potential_tag.push(chars.next().unwrap());
                    current.push('$');
                    break;
                } else if next_c.is_alphanumeric() || next_c == '_' {
                    potential_tag.push(chars.next().unwrap());
                    current.push(potential_tag.chars().last().unwrap());
                } else {
                    break;
                }
            }

            // Check if this is a valid dollar-quote delimiter (ends with $)
            if potential_tag.len() >= 2 && potential_tag.ends_with('$') {
                if in_dollar_quote && potential_tag == dollar_tag {
                    // End of dollar-quoted string
                    in_dollar_quote = false;
                    dollar_tag.clear();
                } else if !in_dollar_quote {
                    // Start of dollar-quoted string
                    in_dollar_quote = true;
                    dollar_tag = potential_tag;
                }
            }
        }

        // Split on semicolon only if not inside a dollar-quoted string
        if c == ';' && !in_dollar_quote {
            let stmt = current.trim().trim_end_matches(';').trim().to_string();
            if !stmt.is_empty() {
                statements.push(stmt);
            }
            current.clear();
        }
    }

    // Don't forget the last statement (might not end with ;)
    let stmt = current.trim().trim_end_matches(';').trim().to_string();
    if !stmt.is_empty() {
        statements.push(stmt);
    }

    statements
}

/// Load user migrations from a directory.
///
/// Migrations should be named like:
/// - `0001_create_users.sql`
/// - `0002_add_posts.sql`
///
/// They are sorted alphabetically and executed in order.
pub fn load_migrations_from_dir(dir: &Path) -> Result<Vec<Migration>> {
    if !dir.exists() {
        debug!("Migrations directory does not exist: {:?}", dir);
        return Ok(Vec::new());
    }

    let mut migrations = Vec::new();

    let entries = std::fs::read_dir(dir).map_err(|e| ForgeError::Io(e))?;

    for entry in entries {
        let entry = entry.map_err(|e| ForgeError::Io(e))?;
        let path = entry.path();

        if path.extension().map(|e| e == "sql").unwrap_or(false) {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| ForgeError::Config("Invalid migration filename".into()))?
                .to_string();

            let sql = std::fs::read_to_string(&path).map_err(|e| ForgeError::Io(e))?;

            migrations.push(Migration::new(name, sql));
        }
    }

    // Sort by name (which includes the numeric prefix)
    migrations.sort_by(|a, b| a.name.cmp(&b.name));

    debug!("Loaded {} user migrations", migrations.len());
    Ok(migrations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_load_migrations_from_empty_dir() {
        let dir = TempDir::new().unwrap();
        let migrations = load_migrations_from_dir(dir.path()).unwrap();
        assert!(migrations.is_empty());
    }

    #[test]
    fn test_load_migrations_from_nonexistent_dir() {
        let migrations = load_migrations_from_dir(Path::new("/nonexistent/path")).unwrap();
        assert!(migrations.is_empty());
    }

    #[test]
    fn test_load_migrations_sorted() {
        let dir = TempDir::new().unwrap();

        // Create migrations out of order
        fs::write(dir.path().join("0002_second.sql"), "SELECT 2;").unwrap();
        fs::write(dir.path().join("0001_first.sql"), "SELECT 1;").unwrap();
        fs::write(dir.path().join("0003_third.sql"), "SELECT 3;").unwrap();

        let migrations = load_migrations_from_dir(dir.path()).unwrap();
        assert_eq!(migrations.len(), 3);
        assert_eq!(migrations[0].name, "0001_first");
        assert_eq!(migrations[1].name, "0002_second");
        assert_eq!(migrations[2].name, "0003_third");
    }

    #[test]
    fn test_load_migrations_ignores_non_sql() {
        let dir = TempDir::new().unwrap();

        fs::write(dir.path().join("0001_migration.sql"), "SELECT 1;").unwrap();
        fs::write(dir.path().join("readme.txt"), "Not a migration").unwrap();
        fs::write(dir.path().join("backup.sql.bak"), "Backup").unwrap();

        let migrations = load_migrations_from_dir(dir.path()).unwrap();
        assert_eq!(migrations.len(), 1);
        assert_eq!(migrations[0].name, "0001_migration");
    }

    #[test]
    fn test_migration_new() {
        let m = Migration::new("test", "SELECT 1");
        assert_eq!(m.name, "test");
        assert_eq!(m.sql, "SELECT 1");
    }

    #[test]
    fn test_split_simple_statements() {
        let sql = "SELECT 1; SELECT 2; SELECT 3;";
        let stmts = super::split_sql_statements(sql);
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0], "SELECT 1");
        assert_eq!(stmts[1], "SELECT 2");
        assert_eq!(stmts[2], "SELECT 3");
    }

    #[test]
    fn test_split_with_dollar_quoted_function() {
        let sql = r#"
CREATE FUNCTION test() RETURNS void AS $$
BEGIN
    SELECT 1;
    SELECT 2;
END;
$$ LANGUAGE plpgsql;

SELECT 3;
"#;
        let stmts = super::split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("CREATE FUNCTION"));
        assert!(stmts[0].contains("$$ LANGUAGE plpgsql"));
        assert!(stmts[1].contains("SELECT 3"));
    }

    #[test]
    fn test_split_preserves_dollar_quote_content() {
        let sql = r#"
CREATE FUNCTION notify() RETURNS trigger AS $$
DECLARE
    row_id TEXT;
BEGIN
    row_id := NEW.id::TEXT;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
"#;
        let stmts = super::split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("row_id := NEW.id::TEXT"));
    }
}

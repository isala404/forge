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

/// A single migration with up and optional down SQL.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Unique name/identifier (e.g., "0001_forge_internal" or "0002_create_users").
    pub name: String,
    /// SQL to execute for upgrade (forward migration).
    pub up_sql: String,
    /// SQL to execute for rollback (optional).
    pub down_sql: Option<String>,
}

impl Migration {
    /// Create a migration with only up SQL (no rollback).
    pub fn new(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            up_sql: sql.into(),
            down_sql: None,
        }
    }

    /// Create a migration with both up and down SQL.
    pub fn with_down(
        name: impl Into<String>,
        up_sql: impl Into<String>,
        down_sql: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            up_sql: up_sql.into(),
            down_sql: Some(down_sql.into()),
        }
    }

    /// Parse migration content that may contain -- @up and -- @down markers.
    pub fn parse(name: impl Into<String>, content: &str) -> Self {
        let name = name.into();
        let (up_sql, down_sql) = parse_migration_content(content);
        Self {
            name,
            up_sql,
            down_sql,
        }
    }
}

/// Parse migration content, splitting on -- @down marker.
/// Returns (up_sql, Option<down_sql>).
fn parse_migration_content(content: &str) -> (String, Option<String>) {
    // Look for -- @down marker (case insensitive, with optional whitespace)
    let down_marker_patterns = ["-- @down", "--@down", "-- @DOWN", "--@DOWN"];

    for pattern in down_marker_patterns {
        if let Some(idx) = content.find(pattern) {
            let up_part = &content[..idx];
            let down_part = &content[idx + pattern.len()..];

            // Clean up the up part (remove -- @up marker if present)
            let up_sql = up_part
                .replace("-- @up", "")
                .replace("--@up", "")
                .replace("-- @UP", "")
                .replace("--@UP", "")
                .trim()
                .to_string();

            let down_sql = down_part.trim().to_string();

            if down_sql.is_empty() {
                return (up_sql, None);
            }
            return (up_sql, Some(down_sql));
        }
    }

    // No @down marker found - treat entire content as up SQL
    let up_sql = content
        .replace("-- @up", "")
        .replace("--@up", "")
        .replace("-- @UP", "")
        .replace("--@UP", "")
        .trim()
        .to_string();

    (up_sql, None)
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
        // Create table if not exists
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS forge_migrations (
                id SERIAL PRIMARY KEY,
                name VARCHAR(255) UNIQUE NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                down_sql TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to create migrations table: {}", e)))?;

        // Add down_sql column if it doesn't exist (for existing installations)
        sqlx::query(
            r#"
            ALTER TABLE forge_migrations
            ADD COLUMN IF NOT EXISTS down_sql TEXT
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to add down_sql column: {}", e)))?;

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
        let statements = split_sql_statements(&migration.up_sql);

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

        // Record it as applied (with down_sql for potential rollback)
        sqlx::query("INSERT INTO forge_migrations (name, down_sql) VALUES ($1, $2)")
            .bind(&migration.name)
            .bind(&migration.down_sql)
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

    /// Rollback N migrations (most recent first).
    pub async fn rollback(&self, count: usize) -> Result<Vec<String>> {
        if count == 0 {
            return Ok(Vec::new());
        }

        // Acquire exclusive lock
        self.acquire_lock().await?;

        let result = self.rollback_inner(count).await;

        // Always release lock
        if let Err(e) = self.release_lock().await {
            warn!("Failed to release migration lock: {}", e);
        }

        result
    }

    async fn rollback_inner(&self, count: usize) -> Result<Vec<String>> {
        self.ensure_migrations_table().await?;

        // Get the N most recent migrations with their down_sql
        let rows: Vec<(i32, String, Option<String>)> = sqlx::query_as(
            "SELECT id, name, down_sql FROM forge_migrations ORDER BY id DESC LIMIT $1",
        )
        .bind(count as i32)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(format!("Failed to get migrations: {}", e)))?;

        if rows.is_empty() {
            info!("No migrations to rollback");
            return Ok(Vec::new());
        }

        let mut rolled_back = Vec::new();

        for (id, name, down_sql) in rows {
            info!("Rolling back migration: {}", name);

            if let Some(down) = down_sql {
                // Execute down SQL
                let statements = split_sql_statements(&down);
                for statement in statements {
                    let statement = statement.trim();
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
                                "Failed to rollback migration '{}': {}",
                                name, e
                            ))
                        })?;
                }
            } else {
                warn!("Migration '{}' has no down SQL, removing record only", name);
            }

            // Remove from migrations table
            sqlx::query("DELETE FROM forge_migrations WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| {
                    ForgeError::Database(format!(
                        "Failed to remove migration record '{}': {}",
                        name, e
                    ))
                })?;

            info!("Rolled back migration: {}", name);
            rolled_back.push(name);
        }

        Ok(rolled_back)
    }

    /// Get the status of all migrations.
    pub async fn status(&self, available: &[Migration]) -> Result<MigrationStatus> {
        self.ensure_migrations_table().await?;

        let applied = self.get_applied_migrations().await?;

        let applied_list: Vec<AppliedMigration> = {
            let rows: Vec<(String, chrono::DateTime<chrono::Utc>, Option<String>)> =
                sqlx::query_as(
                    "SELECT name, applied_at, down_sql FROM forge_migrations ORDER BY id ASC",
                )
                .fetch_all(&self.pool)
                .await
                .map_err(|e| ForgeError::Database(format!("Failed to get migrations: {}", e)))?;

            rows.into_iter()
                .map(|(name, applied_at, down_sql)| AppliedMigration {
                    name,
                    applied_at,
                    has_down: down_sql.is_some(),
                })
                .collect()
        };

        let pending: Vec<String> = available
            .iter()
            .filter(|m| !applied.contains(&m.name))
            .map(|m| m.name.clone())
            .collect();

        Ok(MigrationStatus {
            applied: applied_list,
            pending,
        })
    }
}

/// Information about an applied migration.
#[derive(Debug, Clone)]
pub struct AppliedMigration {
    pub name: String,
    pub applied_at: chrono::DateTime<chrono::Utc>,
    pub has_down: bool,
}

/// Status of migrations.
#[derive(Debug, Clone)]
pub struct MigrationStatus {
    pub applied: Vec<AppliedMigration>,
    pub pending: Vec<String>,
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

    let entries = std::fs::read_dir(dir).map_err(ForgeError::Io)?;

    for entry in entries {
        let entry = entry.map_err(ForgeError::Io)?;
        let path = entry.path();

        if path.extension().map(|e| e == "sql").unwrap_or(false) {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| ForgeError::Config("Invalid migration filename".into()))?
                .to_string();

            let content = std::fs::read_to_string(&path).map_err(ForgeError::Io)?;

            migrations.push(Migration::parse(name, &content));
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
        assert_eq!(m.up_sql, "SELECT 1");
        assert!(m.down_sql.is_none());
    }

    #[test]
    fn test_migration_with_down() {
        let m = Migration::with_down("test", "CREATE TABLE t()", "DROP TABLE t");
        assert_eq!(m.name, "test");
        assert_eq!(m.up_sql, "CREATE TABLE t()");
        assert_eq!(m.down_sql, Some("DROP TABLE t".to_string()));
    }

    #[test]
    fn test_migration_parse_up_only() {
        let content = "CREATE TABLE users (id INT);";
        let m = Migration::parse("0001_test", content);
        assert_eq!(m.name, "0001_test");
        assert_eq!(m.up_sql, "CREATE TABLE users (id INT);");
        assert!(m.down_sql.is_none());
    }

    #[test]
    fn test_migration_parse_with_markers() {
        let content = r#"
-- @up
CREATE TABLE users (
    id UUID PRIMARY KEY,
    email VARCHAR(255)
);

-- @down
DROP TABLE users;
"#;
        let m = Migration::parse("0001_users", content);
        assert_eq!(m.name, "0001_users");
        assert!(m.up_sql.contains("CREATE TABLE users"));
        assert!(!m.up_sql.contains("@up"));
        assert!(!m.up_sql.contains("DROP TABLE"));
        assert_eq!(m.down_sql, Some("DROP TABLE users;".to_string()));
    }

    #[test]
    fn test_migration_parse_complex() {
        let content = r#"
-- @up
CREATE TABLE posts (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL
);
CREATE INDEX idx_posts_title ON posts(title);

-- @down
DROP INDEX idx_posts_title;
DROP TABLE posts;
"#;
        let m = Migration::parse("0002_posts", content);
        assert!(m.up_sql.contains("CREATE TABLE posts"));
        assert!(m.up_sql.contains("CREATE INDEX"));
        let down = m.down_sql.unwrap();
        assert!(down.contains("DROP INDEX"));
        assert!(down.contains("DROP TABLE posts"));
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

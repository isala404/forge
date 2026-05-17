//! Database provisioning for tests.
//!
//! Deliberately avoids reading DATABASE_URL to prevent accidental production use.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_methods
)]
// Test harness runs dynamic DDL (CREATE/DROP DATABASE, schema reset) where the
// query macros can't see the table at compile time.

use sqlx::PgPool;
use std::path::Path;
#[cfg(feature = "testcontainers")]
use std::sync::Arc;
use tracing::{debug, info};

use crate::error::{ForgeError, Result};

#[cfg(feature = "testcontainers")]
type PgContainer =
    Arc<Option<testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>>>;

/// Database access for tests.
///
/// # Examples
///
/// ```ignore
/// let db = TestDatabase::from_url("postgres://localhost/test_db").await?;
/// let db = TestDatabase::from_env().await?;
/// ```
pub struct TestDatabase {
    pool: PgPool,
    url: String,
    #[cfg(feature = "testcontainers")]
    _container: PgContainer,
}

impl TestDatabase {
    /// Connect to database at the given URL.
    pub async fn from_url(url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await
            .map_err(ForgeError::Database)?;

        Ok(Self {
            pool,
            url: url.to_string(),
            #[cfg(feature = "testcontainers")]
            _container: Arc::new(None),
        })
    }

    /// Connect using `TEST_DATABASE_URL`, or start a container if the
    /// `testcontainers` feature is enabled and the var is unset.
    pub async fn from_env() -> Result<Self> {
        match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => Self::from_url(&url).await,
            Err(_) => {
                #[cfg(feature = "testcontainers")]
                {
                    Self::from_container().await
                }
                #[cfg(not(feature = "testcontainers"))]
                {
                    Err(ForgeError::Internal(
                        "TEST_DATABASE_URL not set. Set it explicitly for database tests, \
                         or enable the `testcontainers` feature for automatic provisioning."
                            .to_string(),
                    ))
                }
            }
        }
    }

    #[cfg(feature = "testcontainers")]
    async fn from_container() -> Result<Self> {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        // PG 13+ required for gen_random_uuid() without pgcrypto
        let container = Postgres::default()
            .with_tag("18-alpine")
            .start()
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to start PG container: {e}")))?;

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to get container port: {e}")))?;

        let url = format!("postgres://postgres:postgres@localhost:{port}/postgres");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect(&url)
            .await
            .map_err(ForgeError::Database)?;

        Ok(Self {
            pool,
            url,
            _container: Arc::new(Some(container)),
        })
    }

    /// Get the connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get the database URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Run raw SQL to set up test data or schema.
    pub async fn execute(&self, sql: &str) -> Result<()> {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map_err(ForgeError::Database)?;
        Ok(())
    }

    /// Creates a dedicated database for a single test, providing full isolation.
    ///
    /// Each call creates a new database with a unique name. Use this when tests
    /// modify data and could interfere with each other.
    pub async fn isolated(&self, test_name: &str) -> Result<IsolatedTestDb> {
        let base_url = self.url.clone();
        // UUID suffix prevents collisions when tests run in parallel
        let db_name = format!(
            "forge_test_{}_{}",
            sanitize_db_name(test_name),
            uuid::Uuid::new_v4().simple()
        );

        // Connect to default database to create the test database
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&base_url)
            .await
            .map_err(ForgeError::Database)?;

        // Double-quoted identifier handles special characters in generated name
        sqlx::query(&format!("CREATE DATABASE \"{}\"", db_name))
            .execute(&pool)
            .await
            .map_err(ForgeError::Database)?;

        // Build URL for the new database by replacing the database name component
        let test_url = replace_db_name(&base_url, &db_name);

        let test_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&test_url)
            .await
            .map_err(ForgeError::Database)?;

        Ok(IsolatedTestDb {
            pool: test_pool,
            db_name,
            base_url,
            #[cfg(feature = "testcontainers")]
            _container: self._container.clone(),
        })
    }
}

/// A test database that exists for the lifetime of a single test.
///
/// The database is automatically created on construction. Cleanup happens
/// when `cleanup()` is called or when the database is reused in subsequent
/// test runs (orphaned databases are cleaned up automatically).
pub struct IsolatedTestDb {
    pool: PgPool,
    db_name: String,
    base_url: String,
    #[cfg(feature = "testcontainers")]
    _container: PgContainer,
}

impl IsolatedTestDb {
    /// Create a fully initialized test database in one call.
    ///
    /// Combines `TestDatabase::from_env()`, `isolated()`, `run_sql(internal_sql)`,
    /// and `migrate()` into a single convenience method.
    ///
    /// ```ignore
    /// let db = IsolatedTestDb::setup(
    ///     "my_test",
    ///     &forge::get_internal_sql(),
    ///     Path::new("migrations"),
    /// ).await?;
    /// let pool = db.pool().clone();
    /// ```
    pub async fn setup(test_name: &str, internal_sql: &str, migrations_dir: &Path) -> Result<Self> {
        let base = TestDatabase::from_env().await?;
        let db = base.isolated(test_name).await?;
        db.run_sql(internal_sql).await?;
        db.migrate(migrations_dir).await?;
        Ok(db)
    }

    /// Get the connection pool for this isolated database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get the database name.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Run raw SQL to set up test data or schema.
    pub async fn execute(&self, sql: &str) -> Result<()> {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map_err(ForgeError::Database)?;
        Ok(())
    }

    /// Run multi-statement SQL for setup.
    ///
    /// This handles SQL with multiple statements separated by semicolons,
    /// including PL/pgSQL functions with dollar-quoted strings.
    pub async fn run_sql(&self, sql: &str) -> Result<()> {
        for stmt in split_sql_statements(sql) {
            let stmt = stmt.trim();
            if is_blank_sql(stmt) {
                continue;
            }
            sqlx::query(stmt)
                .execute(&self.pool)
                .await
                .map_err(|e| ForgeError::Internal(format!("Failed to execute SQL: {e}")))?;
        }
        Ok(())
    }

    /// Cleanup the test database by dropping it.
    ///
    /// Call this at the end of your test if you want immediate cleanup.
    /// Otherwise, orphaned databases will be cleaned up on subsequent test runs.
    pub async fn cleanup(self) -> Result<()> {
        // Close all connections first
        self.pool.close().await;

        // Connect to default database to drop the test database
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.base_url)
            .await
            .map_err(ForgeError::Database)?;

        // Force disconnect other connections and drop
        if let Err(e) = sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1",
        )
        .bind(&self.db_name)
        .execute(&pool)
        .await
        {
            tracing::warn!(db = %self.db_name, error = %e, "failed to terminate backend connections during test cleanup");
        }

        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{}\"", self.db_name))
            .execute(&pool)
            .await
            .map_err(ForgeError::Database)?;

        Ok(())
    }

    /// Run migrations from a directory.
    ///
    /// Loads all `.sql` files from the directory, sorts them alphabetically,
    /// and executes them in order. This is intended for test setup.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let base = TestDatabase::from_env().await?;
    /// let db = base.isolated("my_test").await?;
    /// db.migrate(Path::new("migrations")).await?;
    /// ```
    pub async fn migrate(&self, migrations_dir: &Path) -> Result<()> {
        if !migrations_dir.exists() {
            debug!("Migrations directory does not exist: {:?}", migrations_dir);
            return Ok(());
        }

        let mut migrations = Vec::new();

        let entries = std::fs::read_dir(migrations_dir).map_err(ForgeError::Io)?;

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
                migrations.push((name, content));
            }
        }

        // Sort by name (which includes the numeric prefix)
        migrations.sort_by(|a, b| a.0.cmp(&b.0));

        debug!("Running {} migrations for test", migrations.len());

        for (name, content) in migrations {
            info!("Applying test migration: {}", name);

            let up_sql = strip_up_markers(&content);

            for stmt in split_sql_statements(&up_sql) {
                let stmt = stmt.trim();
                if is_blank_sql(stmt) {
                    continue;
                }
                sqlx::query(stmt).execute(&self.pool).await.map_err(|e| {
                    ForgeError::Internal(format!("Failed to apply migration '{name}': {e}"))
                })?;
            }
        }

        Ok(())
    }
}

fn is_blank_sql(sql: &str) -> bool {
    sql.is_empty()
        || sql
            .lines()
            .all(|l| l.trim().is_empty() || l.trim().starts_with("--"))
}

/// Sanitize a test name for use in a database name.
fn sanitize_db_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .take(32)
        .collect()
}

/// Replace the database name in a connection URL.
fn replace_db_name(url: &str, new_db: &str) -> String {
    // Handle both postgres://.../ and postgres://...? formats
    if let Some(idx) = url.rfind('/') {
        let base = &url[..=idx];
        // Check if there are query params
        if let Some(query_idx) = url[idx + 1..].find('?') {
            let query = &url[idx + 1 + query_idx..];
            format!("{}{}{}", base, new_db, query)
        } else {
            format!("{}{}", base, new_db)
        }
    } else {
        format!("{}/{}", url, new_db)
    }
}

fn strip_up_markers(sql: &str) -> String {
    sql.replace("-- @up", "")
        .replace("--@up", "")
        .replace("-- @UP", "")
        .replace("--@UP", "")
        .trim()
        .to_string()
}

/// Split SQL into individual statements, respecting dollar-quoted strings,
/// line comments, block comments, and string literals.
fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut dollar_tag = String::new();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string_literal = false;
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        current.push(c);

        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }

        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                current.push(chars.next().expect("peeked char"));
                in_block_comment = false;
            }
            continue;
        }

        if in_string_literal {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    current.push(chars.next().expect("peeked char"));
                } else {
                    in_string_literal = false;
                }
            }
            continue;
        }

        if in_dollar_quote {
            if c == '$' {
                let mut potential_tag = String::from("$");
                while let Some(&next_c) = chars.peek() {
                    if next_c == '$' {
                        potential_tag.push(chars.next().expect("peeked char"));
                        current.push('$');
                        break;
                    } else if next_c.is_alphanumeric() || next_c == '_' {
                        let ch = chars.next().expect("peeked char");
                        potential_tag.push(ch);
                        current.push(ch);
                    } else {
                        break;
                    }
                }
                if potential_tag.len() >= 2
                    && potential_tag.ends_with('$')
                    && potential_tag == dollar_tag
                {
                    in_dollar_quote = false;
                    dollar_tag.clear();
                }
            }
            continue;
        }

        if c == '-' && chars.peek() == Some(&'-') {
            current.push(chars.next().expect("peeked char"));
            in_line_comment = true;
            continue;
        }

        if c == '/' && chars.peek() == Some(&'*') {
            current.push(chars.next().expect("peeked char"));
            in_block_comment = true;
            continue;
        }

        if c == '\'' {
            in_string_literal = true;
            continue;
        }

        if c == '$' {
            let mut potential_tag = String::from("$");
            while let Some(&next_c) = chars.peek() {
                if next_c == '$' {
                    potential_tag.push(chars.next().expect("peeked char"));
                    current.push('$');
                    break;
                } else if next_c.is_alphanumeric() || next_c == '_' {
                    let ch = chars.next().expect("peeked char");
                    potential_tag.push(ch);
                    current.push(ch);
                } else {
                    break;
                }
            }
            if potential_tag.len() >= 2 && potential_tag.ends_with('$') {
                in_dollar_quote = true;
                dollar_tag = potential_tag;
            }
            continue;
        }

        if c == ';' {
            let stmt = current.trim().trim_end_matches(';').trim().to_string();
            if !stmt.is_empty() {
                statements.push(stmt);
            }
            current.clear();
        }
    }

    let stmt = current.trim().trim_end_matches(';').trim().to_string();
    if !stmt.is_empty() {
        statements.push(stmt);
    }

    statements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_db_name() {
        assert_eq!(sanitize_db_name("my_test"), "my_test");
        assert_eq!(sanitize_db_name("my-test"), "my_test");
        assert_eq!(sanitize_db_name("my test"), "my_test");
        assert_eq!(sanitize_db_name("test::function"), "test__function");
    }

    #[test]
    fn test_replace_db_name() {
        assert_eq!(
            replace_db_name("postgres://localhost/olddb", "newdb"),
            "postgres://localhost/newdb"
        );
        assert_eq!(
            replace_db_name("postgres://user:pass@localhost:5432/olddb", "newdb"),
            "postgres://user:pass@localhost:5432/newdb"
        );
        assert_eq!(
            replace_db_name("postgres://localhost/olddb?sslmode=disable", "newdb"),
            "postgres://localhost/newdb?sslmode=disable"
        );
    }

    // --- SQL statement splitting ---

    #[test]
    fn split_simple_statements() {
        let stmts = split_sql_statements("CREATE TABLE a (id int); CREATE TABLE b (id int);");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].starts_with("CREATE TABLE a"));
        assert!(stmts[1].starts_with("CREATE TABLE b"));
    }

    #[test]
    fn split_preserves_dollar_quoted_content() {
        let sql = r#"
            CREATE FUNCTION test() RETURNS void AS $$
            BEGIN
                INSERT INTO logs (msg) VALUES ('hello; world');
            END;
            $$ LANGUAGE plpgsql;
            SELECT 1;
        "#;
        let stmts = split_sql_statements(sql);
        assert_eq!(
            stmts.len(),
            2,
            "Should split into function + SELECT, not more"
        );
        assert!(
            stmts[0].contains("$$"),
            "Function body must include dollar quotes"
        );
    }

    #[test]
    fn split_handles_empty_input() {
        let stmts = split_sql_statements("");
        assert!(stmts.is_empty());
    }

    #[test]
    fn split_handles_no_trailing_semicolon() {
        let stmts = split_sql_statements("SELECT 1");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "SELECT 1");
    }

    #[test]
    fn split_skips_blank_statements() {
        let stmts = split_sql_statements("; ; SELECT 1; ;");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "SELECT 1");
    }

    #[test]
    fn split_ignores_semicolons_in_line_comments() {
        let sql = "CREATE TABLE t (\n    id INT,\n    -- this; has a semicolon\n    name TEXT\n);\nSELECT 1;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("name TEXT"));
    }

    #[test]
    fn split_ignores_semicolons_in_block_comments() {
        let sql = "CREATE TABLE t (id INT /* ; */ );\nSELECT 1;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn split_ignores_semicolons_in_string_literals() {
        let sql = "INSERT INTO t VALUES ('a;b');\nSELECT 1;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("'a;b'"));
    }

    #[test]
    fn strip_up_markers_drops_marker() {
        let content = "-- @up\nCREATE TABLE a (id int);";
        let up = strip_up_markers(content);
        assert!(!up.contains("@up"), "Up marker should be stripped");
        assert!(up.contains("CREATE TABLE"));
    }

    // --- is_blank_sql ---

    #[test]
    fn blank_sql_detection() {
        assert!(is_blank_sql(""));
        assert!(is_blank_sql("   "));
        assert!(is_blank_sql("-- just a comment"));
        assert!(is_blank_sql("-- comment\n-- another"));
        assert!(!is_blank_sql("SELECT 1"));
        assert!(!is_blank_sql("-- comment\nSELECT 1"));
    }

    // --- sanitize edge cases ---

    #[test]
    fn sanitize_truncates_long_names() {
        let long_name = "a".repeat(100);
        let sanitized = sanitize_db_name(&long_name);
        assert_eq!(sanitized.len(), 32);
    }

    #[test]
    fn sanitize_handles_special_characters() {
        assert_eq!(
            sanitize_db_name("test/with:special!chars"),
            "test_with_special_chars"
        );
        assert_eq!(sanitize_db_name(""), "");
    }
}

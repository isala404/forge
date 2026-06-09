//! Embedded migration runner with mesh-safe locking. One advisory lock serializes
//! nodes; checksums are re-verified each boot, and a schema ahead of the binary
//! refuses to start.

// Embedded DDL runs via runtime `sqlx::query` (can't be macro-typechecked); all
// `forge_system_migrations` bookkeeping uses the checked macros.
#![allow(clippy::disallowed_methods)]

use crate::error::{ForgeError, Result};
use crate::util::sha256_hex;
use sqlx::{PgPool, Postgres};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Advisory lock id, derived from "FORGE" in ASCII.
const MIGRATION_LOCK_ID: i64 = 0x0046_4F52_4745;

/// Creates `forge_system_migrations`. Applied idempotently before anything else.
const BOOTSTRAP_SQL: &str = include_str!("../migrations/v000_bootstrap.sql");

/// Tracked migrations, in apply order. Adding a migration appends here.
fn embedded_migrations() -> Vec<Migration> {
    vec![
        Migration::parse("v001_kv", include_str!("../migrations/v001_kv.sql")),
        Migration::parse("v002_queue", include_str!("../migrations/v002_queue.sql")),
        Migration::parse("v003_config", include_str!("../migrations/v003_config.sql")),
        Migration::parse(
            "v004_ratelimit",
            include_str!("../migrations/v004_ratelimit.sql"),
        ),
        Migration::parse("v005_blob", include_str!("../migrations/v005_blob.sql")),
        Migration::parse("v006_auth", include_str!("../migrations/v006_auth.sql")),
        Migration::parse(
            "v007_schedule",
            include_str!("../migrations/v007_schedule.sql"),
        ),
    ]
}

#[derive(Debug, Clone)]
struct Migration {
    version: String,
    up_sql: String,
    /// Wrap in a transaction (default). `-- @transactional false` opts out for
    /// statements PG refuses inside a tx (e.g. `CREATE INDEX CONCURRENTLY`).
    transactional: bool,
}

impl Migration {
    fn parse(version: impl Into<String>, content: &str) -> Self {
        let mut transactional = true;
        for line in content.lines().take(20) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if !line.starts_with("--") {
                break;
            }
            let body = line.trim_start_matches("--").trim();
            if let Some(rest) = body.strip_prefix("@transactional") {
                let val = rest
                    .trim()
                    .trim_start_matches('=')
                    .trim()
                    .to_ascii_lowercase();
                transactional = !matches!(val.as_str(), "false" | "no" | "0");
            }
        }
        Self {
            version: version.into(),
            up_sql: content.trim().to_string(),
            transactional,
        }
    }
}

/// Runner knobs. Defaults bound the lock wait so a stuck lock fails loud.
#[derive(Debug, Clone)]
pub(crate) struct MigrationConfig {
    pub lock_acquire_timeout: Duration,
    pub lock_poll_interval: Duration,
    pub lock_warn_interval: Duration,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            lock_acquire_timeout: Duration::from_secs(300),
            lock_poll_interval: Duration::from_secs(2),
            lock_warn_interval: Duration::from_secs(30),
        }
    }
}

pub(crate) struct MigrationRunner {
    pool: PgPool,
    config: MigrationConfig,
}

impl MigrationRunner {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            pool,
            config: MigrationConfig::default(),
        }
    }

    /// Apply all pending embedded migrations under the advisory lock.
    pub(crate) async fn run(&self) -> Result<()> {
        let mut lock_conn = self.acquire_lock_connection().await?;
        let result = self.run_inner().await;
        if let Err(e) = self.release_lock(&mut lock_conn).await {
            warn!(error = %e, "failed to release migration lock (non-fatal)");
        }
        result
    }

    /// Verify the schema is present and at-or-behind this binary without applying
    /// anything. Used when `run_migrations` is disabled.
    pub(crate) async fn verify_only(&self) -> Result<()> {
        self.bootstrap().await?;
        let applied = self.applied_versions().await?;
        let migrations = embedded_migrations();
        self.guard_not_ahead(&applied, &migrations)?;
        let missing: Vec<String> = migrations
            .into_iter()
            .filter(|m| !applied.contains_key(&m.version))
            .map(|m| m.version)
            .collect();
        if !missing.is_empty() {
            return Err(ForgeError::config(format!(
                "migrations are disabled but the schema is missing: [{}]. \
                 Apply them, or enable run_migrations.",
                missing.join(", ")
            )));
        }
        Ok(())
    }

    async fn run_inner(&self) -> Result<()> {
        self.bootstrap().await?;
        let applied = self.applied_versions().await?;
        let migrations = embedded_migrations();
        self.guard_not_ahead(&applied, &migrations)?;

        for migration in &migrations {
            if let Some(recorded) = applied.get(&migration.version) {
                verify_checksum(migration, recorded)?;
                debug!(version = %migration.version, "migration already applied (checksum verified)");
                continue;
            }
            self.apply(migration).await?;
        }
        Ok(())
    }

    /// Refuse to start if the database carries unknown migrations; an older binary
    /// on a newer schema risks data loss.
    fn guard_not_ahead(
        &self,
        applied: &HashMap<String, String>,
        migrations: &[Migration],
    ) -> Result<()> {
        let known: HashSet<&str> = migrations.iter().map(|m| m.version.as_str()).collect();
        let mut unknown: Vec<&str> = applied
            .keys()
            .map(String::as_str)
            .filter(|v| !known.contains(v))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(ForgeError::config(format!(
                "database has migration(s) this binary does not know about: [{}]. \
                 Refusing to start — the schema is ahead of this binary; deploy the latest version.",
                unknown.join(", ")
            )));
        }
        Ok(())
    }

    async fn bootstrap(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        for stmt in split_sql_statements(BOOTSTRAP_SQL) {
            if is_empty_or_comment_only(&stmt) {
                continue;
            }
            sqlx::query(&stmt).execute(&mut *conn).await?;
        }
        Ok(())
    }

    async fn applied_versions(&self) -> Result<HashMap<String, String>> {
        let rows = sqlx::query!("SELECT version, checksum FROM forge_system_migrations")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| (r.version, r.checksum)).collect())
    }

    async fn apply(&self, migration: &Migration) -> Result<()> {
        info!(version = %migration.version, "applying migration");
        let start = Instant::now();
        let checksum = sha256_hex(migration.up_sql.as_bytes());

        if migration.transactional {
            let mut tx = self.pool.begin().await?;
            sqlx::query("SET LOCAL lock_timeout = '5s'")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SET LOCAL statement_timeout = '5min'")
                .execute(&mut *tx)
                .await?;
            for stmt in split_sql_statements(&migration.up_sql) {
                if is_empty_or_comment_only(&stmt) {
                    continue;
                }
                sqlx::query(&stmt).execute(&mut *tx).await.map_err(|e| {
                    ForgeError::config(format!(
                        "migration '{}' failed: {}",
                        migration.version,
                        sql_cause(&e)
                    ))
                })?;
            }
            sqlx::query!(
                "INSERT INTO forge_system_migrations (version, checksum) VALUES ($1, $2)",
                migration.version,
                checksum,
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        } else {
            // Same connection for DDL + bookkeeping shrinks the crash window between
            // them. These session-level SETs (no tx to scope them) MUST be RESET
            // before the connection returns to the pool, or the hot path inherits a
            // 30-min statement_timeout.
            let mut conn = self.pool.acquire().await?;
            sqlx::query("SET lock_timeout = '5s'")
                .execute(&mut *conn)
                .await?;
            sqlx::query("SET statement_timeout = '30min'")
                .execute(&mut *conn)
                .await?;

            let applied: Result<()> = async {
                for stmt in split_sql_statements(&migration.up_sql) {
                    if is_empty_or_comment_only(&stmt) {
                        continue;
                    }
                    sqlx::query(&stmt).execute(&mut *conn).await.map_err(|e| {
                        ForgeError::config(format!(
                            "migration '{}' failed: {}",
                            migration.version,
                            sql_cause(&e)
                        ))
                    })?;
                }
                sqlx::query!(
                    "INSERT INTO forge_system_migrations (version, checksum) VALUES ($1, $2)",
                    migration.version,
                    checksum,
                )
                .execute(&mut *conn)
                .await?;
                Ok(())
            }
            .await;

            // Reset even on error, before the connection returns to the pool.
            if let Err(e) = sqlx::query("RESET statement_timeout")
                .execute(&mut *conn)
                .await
            {
                warn!(error = %e, "failed to RESET statement_timeout after non-tx migration");
            }
            if let Err(e) = sqlx::query("RESET lock_timeout").execute(&mut *conn).await {
                warn!(error = %e, "failed to RESET lock_timeout after non-tx migration");
            }
            drop(conn);
            applied?;
        }

        info!(version = %migration.version, elapsed = ?start.elapsed(), "migration applied");
        Ok(())
    }

    async fn acquire_lock_connection(&self) -> Result<sqlx::pool::PoolConnection<Postgres>> {
        let mut conn = self.pool.acquire().await?;
        let deadline = Instant::now() + self.config.lock_acquire_timeout;
        let mut last_warn = Instant::now()
            .checked_sub(self.config.lock_warn_interval)
            .unwrap_or_else(Instant::now);

        loop {
            let acquired = sqlx::query_scalar!(
                r#"SELECT pg_try_advisory_lock($1) AS "acquired!""#,
                MIGRATION_LOCK_ID
            )
            .fetch_one(&mut *conn)
            .await?;
            if acquired {
                debug!("migration lock acquired");
                return Ok(conn);
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(ForgeError::config(format!(
                    "timed out after {:?} waiting for the migration advisory lock — \
                     another node is migrating or stalled holding it",
                    self.config.lock_acquire_timeout
                )));
            }
            if now.duration_since(last_warn) >= self.config.lock_warn_interval {
                warn!("still waiting for the migration lock — another node is holding it");
                last_warn = now;
            }
            tokio::time::sleep(self.config.lock_poll_interval).await;
        }
    }

    async fn release_lock(&self, conn: &mut sqlx::pool::PoolConnection<Postgres>) -> Result<()> {
        let released: Option<bool> =
            sqlx::query_scalar!("SELECT pg_advisory_unlock($1)", MIGRATION_LOCK_ID)
                .fetch_one(&mut **conn)
                .await?;
        debug!(released = ?released, "migration lock released");
        Ok(())
    }
}

/// Detect drift: an applied migration whose source no longer matches the recorded
/// checksum. Migrations are immutable once applied.
fn verify_checksum(migration: &Migration, recorded: &str) -> Result<()> {
    let computed = sha256_hex(migration.up_sql.as_bytes());
    if computed != recorded {
        return Err(ForgeError::config(format!(
            "migration '{}' has changed since it was applied (recorded {recorded}, now {computed}). \
             Migrations are immutable once applied — revert the file or add a new migration.",
            migration.version
        )));
    }
    Ok(())
}

/// Pull the underlying Postgres message out of a sqlx error (migration errors are
/// config errors, not secrets, so surfacing them is safe).
fn sql_cause(err: &sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db) => db.message().to_string(),
        other => other.to_string(),
    }
}

/// True for blank statements or statements made only of `--` comment lines.
fn is_empty_or_comment_only(stmt: &str) -> bool {
    let s = stmt.trim();
    s.is_empty()
        || s.lines().all(|l| {
            let l = l.trim();
            l.is_empty() || l.starts_with("--")
        })
}

/// Consume a `$...$` dollar-quote tag whose opening `$` was just pushed to
/// `current`. Appends every consumed char to `current` and returns the full tag
/// (e.g. `$$` or `$body$`). A return that is not `len >= 2 && ends_with('$')`
/// means no valid tag started here. Shared by the opening and closing scans so
/// the two stay in lockstep.
fn scan_dollar_tag(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    current: &mut String,
) -> String {
    let mut tag = String::from("$");
    while let Some(&next_c) = chars.peek() {
        if next_c == '$' {
            if let Some(n) = chars.next() {
                current.push(n);
            }
            tag.push('$');
            break;
        } else if next_c.is_alphanumeric() || next_c == '_' {
            if let Some(ch) = chars.next() {
                tag.push(ch);
                current.push(ch);
            }
        } else {
            break;
        }
    }
    tag
}

/// Split SQL into statements, respecting dollar-quoted strings, single-quoted
/// literals, and `--`/block comments — so semicolons inside `$$` PL/pgSQL bodies
/// don't split.
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
                if let Some(n) = chars.next() {
                    current.push(n);
                }
                in_block_comment = false;
            }
            continue;
        }
        if in_string_literal {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    if let Some(n) = chars.next() {
                        current.push(n);
                    }
                } else {
                    in_string_literal = false;
                }
            }
            continue;
        }
        if in_dollar_quote {
            if c == '$' {
                let tag = scan_dollar_tag(&mut chars, &mut current);
                if tag.len() >= 2 && tag.ends_with('$') && tag == dollar_tag {
                    in_dollar_quote = false;
                    dollar_tag.clear();
                }
            }
            continue;
        }

        if c == '-' && chars.peek() == Some(&'-') {
            if let Some(n) = chars.next() {
                current.push(n);
            }
            in_line_comment = true;
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            if let Some(n) = chars.next() {
                current.push(n);
            }
            in_block_comment = true;
            continue;
        }
        if c == '\'' {
            in_string_literal = true;
            continue;
        }
        if c == '$' {
            let tag = scan_dollar_tag(&mut chars, &mut current);
            if tag.len() >= 2 && tag.ends_with('$') {
                in_dollar_quote = true;
                dollar_tag = tag;
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn split_simple_statements() {
        let stmts = split_sql_statements("SELECT 1; SELECT 2; SELECT 3;");
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0], "SELECT 1");
    }

    #[test]
    fn split_respects_dollar_quoted_function() {
        let sql = r#"
CREATE FUNCTION t() RETURNS void AS $$
BEGIN
    SELECT 1;
    SELECT 2;
END;
$$ LANGUAGE plpgsql;
SELECT 3;
"#;
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("CREATE FUNCTION"));
        assert!(stmts[1].contains("SELECT 3"));
    }

    #[test]
    fn embedded_migrations_parse_and_are_ordered() {
        let m = embedded_migrations();
        assert_eq!(m[0].version, "v001_kv");
        assert_eq!(m[1].version, "v002_queue");
        assert!(m.iter().all(|m| m.transactional));
    }

    #[test]
    fn verify_checksum_catches_drift() {
        let m = Migration::parse("v001_kv", "CREATE TABLE t (id INT);");
        let good = sha256_hex(m.up_sql.as_bytes());
        assert!(verify_checksum(&m, &good).is_ok());
        let err = verify_checksum(&m, "stale").unwrap_err();
        assert!(matches!(err, ForgeError::Config(_)));
    }

    #[test]
    fn is_empty_or_comment_only_detects_noise() {
        assert!(is_empty_or_comment_only("   "));
        assert!(is_empty_or_comment_only("-- just a comment"));
        assert!(!is_empty_or_comment_only("SELECT 1"));
    }
}

// Embedded DDL runs via runtime `sqlx::query` (can't be macro-typechecked); all
// `forge_system_migrations` bookkeeping uses the checked macros.
#![allow(clippy::disallowed_methods)]

use crate::error::{ForgeError, Result};
use crate::util::sha256_hex;
use sqlx::{PgPool, Postgres, Row};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Advisory lock id, derived from "FORGE" in ASCII.
const MIGRATION_LOCK_ID: i64 = 0x0046_4F52_4745;

/// Applied idempotently before everything else.
const BOOTSTRAP_SQL: &str = include_str!("../migrations/bootstrap.sql");

/// Tracked migrations, in apply order. The v1 schema is one consolidated baseline; a
/// future schema change appends a new migration here rather than editing this one.
fn embedded_migrations() -> Vec<Migration> {
    vec![Migration::parse(
        "v001_schema",
        include_str!("../migrations/v001_schema.sql"),
    )]
}

/// Machine-readable outcome of a migration operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationState {
    Pending,
    Applied,
    Locked,
    Incompatible,
    Failed,
}

impl MigrationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Locked => "locked",
            Self::Incompatible => "incompatible",
            Self::Failed => "failed",
        }
    }
}

/// Structured status returned by `migrate`, `migration_status`, and `validate_schema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub target: String,
    pub state: MigrationState,
    pub current_version: Option<String>,
    pub target_version: String,
    pub applied: Vec<String>,
    pub pending: Vec<String>,
    pub lock_holder: Option<String>,
    pub message: String,
}

impl MigrationReport {
    pub fn is_compatible(&self) -> bool {
        self.state == MigrationState::Applied
    }
}

#[derive(Debug, Clone)]
struct Migration {
    version: String,
    up_sql: String,
}

impl Migration {
    fn parse(version: impl Into<String>, content: &str) -> Self {
        Self {
            version: version.into(),
            up_sql: content.trim().to_string(),
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
    target: String,
}

impl MigrationRunner {
    pub(crate) fn new(pool: PgPool, target: impl Into<String>, lock_timeout: Duration) -> Self {
        let config = MigrationConfig {
            lock_acquire_timeout: lock_timeout,
            ..MigrationConfig::default()
        };
        Self {
            pool,
            config,
            target: target.into(),
        }
    }

    pub(crate) async fn run(&self) -> Result<MigrationReport> {
        let Some(mut lock_conn) = self.acquire_lock_connection().await? else {
            let mut report = self.inspect().await?;
            report.state = MigrationState::Locked;
            report.lock_holder = self.lock_holder().await?;
            report.message = format!(
                "migration lock was not acquired within {:?}",
                self.config.lock_acquire_timeout
            );
            return Ok(report);
        };
        let result = self.run_inner().await;
        if let Err(error) = self.release_lock(&mut lock_conn).await {
            warn!(%error, "failed to release migration lock (non-fatal)");
        }
        result
    }

    pub(crate) async fn status(&self) -> Result<MigrationReport> {
        let mut conn = self.pool.acquire().await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MIGRATION_LOCK_ID)
            .fetch_one(&mut *conn)
            .await?;
        if acquired {
            let _released: Option<bool> = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
                .bind(MIGRATION_LOCK_ID)
                .fetch_one(&mut *conn)
                .await?;
            drop(conn);
            return self.inspect().await;
        }
        drop(conn);
        let mut report = self.inspect().await?;
        report.state = MigrationState::Locked;
        report.lock_holder = self.lock_holder().await?;
        report.message = "another process owns the migration lock".to_string();
        Ok(report)
    }

    pub(crate) async fn validate(&self) -> Result<MigrationReport> {
        self.inspect().await
    }

    async fn run_inner(&self) -> Result<MigrationReport> {
        self.bootstrap().await?;
        let status = self.inspect().await?;
        if status.state == MigrationState::Incompatible {
            return Ok(status);
        }
        let pending: HashSet<&str> = status.pending.iter().map(String::as_str).collect();
        for migration in embedded_migrations()
            .iter()
            .filter(|migration| pending.contains(migration.version.as_str()))
        {
            if let Err(error) = self.apply(migration).await {
                let mut report = self.inspect().await.unwrap_or(status);
                report.state = MigrationState::Failed;
                report.message = format!("migration {} failed: {}", migration.version, error);
                return Ok(report);
            }
        }
        self.inspect().await
    }

    async fn inspect(&self) -> Result<MigrationReport> {
        let migrations = embedded_migrations();
        let target_version = migrations
            .last()
            .map(|migration| migration.version.clone())
            .unwrap_or_default();
        if !self.tracking_table_exists().await? {
            return Ok(MigrationReport {
                target: self.target.clone(),
                state: MigrationState::Pending,
                current_version: None,
                target_version,
                applied: Vec::new(),
                pending: migrations
                    .into_iter()
                    .map(|migration| migration.version)
                    .collect(),
                lock_holder: None,
                message: "Forge schema has not been migrated".to_string(),
            });
        }
        let applied = self.applied_versions().await?;
        let mut applied_names: Vec<String> = applied.keys().cloned().collect();
        applied_names.sort();
        let current_version = applied_names.last().cloned();
        let known: HashSet<&str> = migrations
            .iter()
            .map(|item| item.version.as_str())
            .collect();
        let mut pending = Vec::new();
        let mut problems = Vec::new();
        let mut saw_applied_after_gap = false;
        let mut gap = false;
        for migration in &migrations {
            if let Some(recorded) = applied.get(&migration.version) {
                if gap {
                    saw_applied_after_gap = true;
                }
                if let Err(error) = verify_checksum(migration, recorded) {
                    problems.push(error.to_string());
                }
            } else {
                gap = true;
                pending.push(migration.version.clone());
            }
        }
        let mut unknown: Vec<&String> = applied
            .keys()
            .filter(|version| !known.contains(version.as_str()))
            .collect();
        unknown.sort();
        if !unknown.is_empty() {
            problems.push(format!(
                "unknown migration history: {}",
                unknown
                    .iter()
                    .map(|version| version.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if saw_applied_after_gap {
            problems.push("migration history contains a gap".to_string());
        }
        let state = if problems.is_empty() && pending.is_empty() {
            MigrationState::Applied
        } else if problems.is_empty() {
            MigrationState::Pending
        } else {
            MigrationState::Incompatible
        };
        let message = match state {
            MigrationState::Applied => "schema is current".to_string(),
            MigrationState::Pending => format!("{} migration(s) pending", pending.len()),
            MigrationState::Incompatible => problems.join("; "),
            _ => String::new(),
        };
        Ok(MigrationReport {
            target: self.target.clone(),
            state,
            current_version,
            target_version,
            applied: applied_names,
            pending,
            lock_holder: None,
            message,
        })
    }

    async fn tracking_table_exists(&self) -> Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('forge_system_migrations') IS NOT NULL")
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    async fn bootstrap(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        for stmt in split_sql_statements(BOOTSTRAP_SQL) {
            if is_empty_or_comment_only(&stmt) {
                continue;
            }
            // Migration SQL is embedded and contains no caller input.
            sqlx::query(sqlx::AssertSqlSafe(stmt))
                .execute(&mut *conn)
                .await?;
        }
        Ok(())
    }

    async fn applied_versions(&self) -> Result<HashMap<String, String>> {
        let rows = sqlx::query("SELECT version, checksum FROM forge_system_migrations")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get("version"), row.get("checksum")))
            .collect())
    }

    async fn apply(&self, migration: &Migration) -> Result<()> {
        info!(version = %migration.version, "applying migration");
        let start = Instant::now();
        let checksum = sha256_hex(migration.up_sql.as_bytes());

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
            sqlx::query(sqlx::AssertSqlSafe(stmt))
                .execute(&mut *tx)
                .await
                .map_err(|e| {
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

        info!(version = %migration.version, elapsed = ?start.elapsed(), "migration applied");
        Ok(())
    }

    async fn acquire_lock_connection(
        &self,
    ) -> Result<Option<sqlx::pool::PoolConnection<Postgres>>> {
        let mut conn = self.pool.acquire().await?;
        let deadline = Instant::now() + self.config.lock_acquire_timeout;
        let mut last_warn = Instant::now()
            .checked_sub(self.config.lock_warn_interval)
            .unwrap_or_else(Instant::now);

        loop {
            let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(MIGRATION_LOCK_ID)
                .fetch_one(&mut *conn)
                .await?;
            if acquired {
                debug!("migration lock acquired");
                return Ok(Some(conn));
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            if now.duration_since(last_warn) >= self.config.lock_warn_interval {
                warn!("still waiting for the migration lock, another node is holding it");
                last_warn = now;
            }
            tokio::time::sleep(
                self.config
                    .lock_poll_interval
                    .min(deadline.saturating_duration_since(now)),
            )
            .await;
        }
    }

    async fn release_lock(&self, conn: &mut sqlx::pool::PoolConnection<Postgres>) -> Result<()> {
        let released: Option<bool> = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_ID)
            .fetch_one(&mut **conn)
            .await?;
        debug!(released = ?released, "migration lock released");
        Ok(())
    }

    async fn lock_holder(&self) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT a.pid, a.application_name, COALESCE(a.client_addr::text, 'local') AS client \
             FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
             WHERE l.locktype = 'advisory' AND l.granted \
               AND l.classid::bigint = ($1 >> 32) \
               AND l.objid::bigint = ($1 & 4294967295) LIMIT 1",
        )
        .bind(MIGRATION_LOCK_ID)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| {
            format!(
                "pid={} application={} client={}",
                row.get::<i32, _>("pid"),
                row.get::<String, _>("application_name"),
                row.get::<String, _>("client")
            )
        }))
    }
}

/// Detect drift: an applied migration whose source no longer matches the recorded
/// checksum. Migrations are immutable once applied.
fn verify_checksum(migration: &Migration, recorded: &str) -> Result<()> {
    let computed = sha256_hex(migration.up_sql.as_bytes());
    if computed != recorded {
        return Err(ForgeError::config(format!(
            "migration '{}' has changed since it was applied (recorded {recorded}, now {computed}). \
             Migrations are immutable once applied. Revert the file or add a new migration.",
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

fn is_empty_or_comment_only(stmt: &str) -> bool {
    let s = stmt.trim();
    s.is_empty()
        || s.lines().all(|l| {
            let l = l.trim();
            l.is_empty() || l.starts_with("--")
        })
}

/// Consume a `$...$` dollar-quote tag whose opening `$` was just pushed to `current`,
/// appending consumed chars to `current` and returning the full tag (e.g. `$$` or
/// `$body$`). A return that isn't `len >= 2 && ends_with('$')` means no valid tag started
/// here. Shared by the opening and closing scans so they stay in lockstep.
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

/// Split SQL into statements, respecting dollar-quoted strings, single-quoted literals,
/// and `--`/block comments, so semicolons inside `$$` PL/pgSQL bodies don't split.
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
    fn embedded_migrations_contain_the_baseline() {
        let m = embedded_migrations();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].version, "v001_schema");
    }

    #[test]
    fn verify_checksum_catches_drift() {
        let m = Migration::parse("v001_schema", "CREATE TABLE t (id INT);");
        let good = sha256_hex(m.up_sql.as_bytes());
        assert!(verify_checksum(&m, &good).is_ok());
        let err = verify_checksum(&m, "stale").unwrap_err();
        assert!(matches!(err, ForgeError::Config(_)));
    }

    #[test]
    fn migration_states_are_stable_machine_values() {
        assert_eq!(MigrationState::Pending.as_str(), "pending");
        assert_eq!(MigrationState::Applied.as_str(), "applied");
        assert_eq!(MigrationState::Locked.as_str(), "locked");
        assert_eq!(MigrationState::Incompatible.as_str(), "incompatible");
        assert_eq!(MigrationState::Failed.as_str(), "failed");
    }

    #[test]
    fn is_empty_or_comment_only_detects_noise() {
        assert!(is_empty_or_comment_only("   "));
        assert!(is_empty_or_comment_only("-- just a comment"));
        assert!(!is_empty_or_comment_only("SELECT 1"));
    }
}

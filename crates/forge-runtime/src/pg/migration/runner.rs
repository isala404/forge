//! Migration runner with mesh-safe locking.
//!
//! Ensures only one node runs migrations at a time using PostgreSQL advisory locks.
//!
//! # Migration Types
//!
//! 1. **System migrations** (`__forge_vXXX`): Internal FORGE schema changes.
//!    Versioned numerically and always run before user migrations.
//!
//! 2. **User migrations** (`XXXX_name.sql`): Application-specific schema changes.
//!    Sorted alphabetically by name.

// Migrations execute user-supplied DDL/DML strings, so the compile-time query
// macros can't see the schema for those statements. Runtime sqlx::query is
// required for the user-SQL execution path only; the runner's own bookkeeping
// (forge_system_migrations) uses compile-time macros.
#![allow(clippy::disallowed_methods)]

use forge_core::error::{ForgeError, Result};
use sqlx::{PgPool, Postgres};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::builtin::extract_version;

/// Lock ID for migration advisory lock (arbitrary but consistent).
/// Using a fixed value derived from "FORGE" ascii values.
const MIGRATION_LOCK_ID: i64 = 0x464F524745; // "FORGE" in hex

/// Bootstrap SQL embedded at compile time. Creates `forge_system_migrations`.
/// Runs idempotently on every startup before any tracked migration applies.
const BOOTSTRAP_SQL: &str = include_str!("../../../migrations/system/v000_bootstrap.sql");

/// Tunable knobs for the migration runner. Defaults match the deploy-friendly
/// values: bounded lock-acquire wait so a stuck lock fails CI loudly instead of
/// hanging silently.
#[derive(Debug, Clone)]
pub struct MigrationConfig {
    /// Overall deadline for acquiring the migration advisory lock.
    pub lock_acquire_timeout: Duration,
    /// Polling interval between `pg_try_advisory_lock` attempts.
    pub lock_poll_interval: Duration,
    /// Cadence for emitting WARN logs identifying the current lock holder.
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

/// A single migration with up SQL.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Stable version identifier — for system migrations `__forge_vNNN`,
    /// for user migrations the file stem like `0001_create_users`.
    pub version: String,
    /// SQL to execute for upgrade.
    pub up_sql: String,
    /// Wrap the migration in a transaction. Defaults to true.
    /// Set to false for statements PostgreSQL refuses to run inside a tx
    /// (`CREATE INDEX CONCURRENTLY`, `ALTER TYPE ... ADD VALUE`, `VACUUM`,
    /// `REINDEX CONCURRENTLY`) via the `-- @transactional false` directive.
    pub transactional: bool,
}

impl Migration {
    /// Create a migration.
    pub fn new(version: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            up_sql: sql.into(),
            transactional: true,
        }
    }

    /// Parse migration content.
    ///
    /// Recognized directives in the leading comment block (first 20 lines):
    /// - `-- @up` (legacy marker, stripped if present)
    /// - `-- @transactional false` opts out of the wrapping transaction
    ///
    /// The `@` prefix is required so plain English comments
    /// (`-- transactional design ...`) don't accidentally trigger directives.
    pub fn parse(version: impl Into<String>, content: &str) -> Self {
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
            let Some(directive) = body.strip_prefix('@') else {
                continue;
            };
            if let Some(rest) = directive.strip_prefix("transactional") {
                let val = rest
                    .trim()
                    .trim_start_matches('=')
                    .trim()
                    .to_ascii_lowercase();
                transactional = !matches!(val.as_str(), "false" | "no" | "0");
            }
        }

        let up_sql = content
            .replace("-- @up", "")
            .replace("--@up", "")
            .replace("-- @UP", "")
            .replace("--@UP", "")
            .trim()
            .to_string();
        Self {
            version: version.into(),
            up_sql,
            transactional,
        }
    }
}

/// Migration runner that handles both built-in and user migrations.
pub struct MigrationRunner {
    pool: PgPool,
    config: MigrationConfig,
}

impl MigrationRunner {
    pub fn new(pool: PgPool) -> Self {
        Self::with_config(pool, MigrationConfig::default())
    }

    pub fn with_config(pool: PgPool, config: MigrationConfig) -> Self {
        Self { pool, config }
    }

    /// Run all pending migrations with mesh-safe locking.
    pub async fn run(&self, user_migrations: Vec<Migration>) -> Result<()> {
        let mut lock_conn = self.acquire_lock_connection().await?;

        let result = self.run_migrations_inner(user_migrations).await;

        if let Err(e) = self.release_lock_connection(&mut lock_conn).await {
            warn!("Failed to release migration lock: {}", e);
        }

        result
    }

    async fn run_migrations_inner(&self, user_migrations: Vec<Migration>) -> Result<()> {
        self.bootstrap_tracking_table().await?;

        let applied = self.applied_versions().await?;
        debug!(
            "Already applied migrations: {:?}",
            applied.keys().collect::<Vec<_>>()
        );

        let max_applied_version = self.get_max_system_version(&applied);
        debug!("Max applied system version: {:?}", max_applied_version);

        let system_migrations = super::builtin::get_system_migrations();

        let max_known_version = system_migrations.iter().map(|m| m.version).max();
        if let (Some(applied_max), Some(known_max)) = (max_applied_version, max_known_version)
            && applied_max > known_max
        {
            return Err(ForgeError::internal(format!(
                "Database is at system migration v{applied_max} but this binary only knows up to v{known_max}. \
                 Refusing to start — running an older binary on a newer schema risks data loss. \
                 Upgrade the binary or restore the database to a compatible version."
            )));
        }

        // Check for user migrations the DB has applied that this binary does not
        // know about. This catches the rolling-deploy case where a newer binary
        // ran user migrations and an older binary is now starting up against the
        // updated schema. The binary's known set is the `user_migrations` slice
        // passed by the caller; any DB row whose version is not in that set (and
        // is not a system migration) is evidence of an ahead-of-binary schema.
        let known_user_versions: std::collections::HashSet<&str> =
            user_migrations.iter().map(|m| m.version.as_str()).collect();
        let mut unknown_applied: Vec<&str> = applied
            .keys()
            .filter(|v| {
                !super::builtin::is_system_migration(v) && !known_user_versions.contains(v.as_str())
            })
            .map(|v| v.as_str())
            .collect();
        if !unknown_applied.is_empty() {
            unknown_applied.sort_unstable();
            return Err(ForgeError::internal(format!(
                "Database has {} user migration(s) this binary does not know about: [{}]. \
                 Refusing to start — the database schema is ahead of this binary. \
                 Deploy the latest binary version.",
                unknown_applied.len(),
                unknown_applied.join(", "),
            )));
        }

        let mut new_migrations_applied = false;

        for sys_migration in system_migrations {
            let migration = sys_migration.to_migration();
            if let Some(recorded) = applied.get(&migration.version) {
                verify_checksum(&migration, recorded)?;
                debug!(
                    "Skipping system migration {} (already applied, checksum verified)",
                    migration.version
                );
                continue;
            }
            info!(
                "Applying system migration: {} ({})",
                migration.version, sys_migration.description
            );
            self.apply_migration(&migration).await?;
            new_migrations_applied = true;
        }

        for migration in user_migrations {
            if let Some(recorded) = applied.get(&migration.version) {
                verify_checksum(&migration, recorded)?;
                debug!(
                    "Skipping user migration {} (already applied, checksum verified)",
                    migration.version
                );
                continue;
            }
            self.apply_migration(&migration).await?;
            new_migrations_applied = true;
        }

        if new_migrations_applied {
            self.notify_schema_changed().await;
        }

        Ok(())
    }

    /// Broadcast a schema-changed notification over `forge_schema_changed` so
    /// any node listening (monitoring, ops tooling, or a future auto-restart
    /// mechanism) can react without polling.
    ///
    /// This is best-effort: a failure to notify must never abort the migration
    /// sequence — the schema changes already committed are the authoritative
    /// state. We log the error and move on.
    async fn notify_schema_changed(&self) {
        match sqlx::query("SELECT pg_notify('forge_schema_changed', 'migrations_applied')")
            .execute(&self.pool)
            .await
        {
            Ok(_) => debug!("Schema change notification sent"),
            Err(e) => warn!(error = %e, "Failed to send schema change notification (non-fatal)"),
        }
    }

    fn get_max_system_version(&self, applied: &HashMap<String, String>) -> Option<u32> {
        applied.keys().filter_map(|v| extract_version(v)).max()
    }

    async fn acquire_lock_connection(&self) -> Result<sqlx::pool::PoolConnection<Postgres>> {
        debug!(
            timeout_secs = self.config.lock_acquire_timeout.as_secs(),
            "Acquiring migration lock..."
        );
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| ForgeError::internal_with("Failed to acquire lock connection", e))?;

        // Decompose the i64 lock id into the (classid, objid) split that
        // pg_locks exposes so we can look up the holder PID for diagnostics.
        let classid = (MIGRATION_LOCK_ID >> 32) as i32;
        let objid = (MIGRATION_LOCK_ID & 0xFFFF_FFFF) as i32;

        let deadline = Instant::now() + self.config.lock_acquire_timeout;
        // Backdate the warn timestamp so the *first* contended iteration emits
        // a WARN immediately — operators see "lock busy, holder X" right away
        // instead of staring at silence for the full warn_interval.
        let mut last_warn = Instant::now()
            .checked_sub(self.config.lock_warn_interval)
            .unwrap_or_else(Instant::now);

        // Invariant: this loop must either return `Ok(conn)` *with* the lock
        // held or return `Err` *without* the lock held. The lock is acquired
        // atomically by `pg_try_advisory_lock`, and we only ever look up the
        // holder PID *after* the try returned false, so there is no path
        // where we hold the lock and then leak it via a follow-up query.
        loop {
            let acquired = sqlx::query_scalar!(
                r#"SELECT pg_try_advisory_lock($1) AS "acquired!""#,
                MIGRATION_LOCK_ID
            )
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to attempt migration lock", e))?;

            if acquired {
                debug!("Migration lock acquired");
                return Ok(conn);
            }

            let now = Instant::now();
            if now >= deadline {
                let holder = lookup_lock_holder(&mut conn, classid, objid).await;
                return Err(ForgeError::internal(format!(
                    "Timed out after {:?} waiting for migration lock (holder pid: {:?}). \
                     Another node is likely running migrations or stalled holding the lock.",
                    self.config.lock_acquire_timeout, holder
                )));
            }

            if now.duration_since(last_warn) >= self.config.lock_warn_interval {
                let holder = lookup_lock_holder(&mut conn, classid, objid).await;
                warn!(
                    holder_pid = ?holder,
                    "Still waiting for migration lock — another node is holding it"
                );
                last_warn = now;
            }

            tokio::time::sleep(self.config.lock_poll_interval).await;
        }
    }

    async fn release_lock_connection(
        &self,
        conn: &mut sqlx::pool::PoolConnection<Postgres>,
    ) -> Result<()> {
        sqlx::query_scalar!("SELECT pg_advisory_unlock($1)", MIGRATION_LOCK_ID)
            .fetch_one(&mut **conn)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to release migration lock", e))?;
        debug!("Migration lock released");
        Ok(())
    }

    /// Idempotent bootstrap: runs the `forge_system_migrations` table DDL.
    /// Statements are split because the bootstrap file may contain multiple.
    async fn bootstrap_tracking_table(&self) -> Result<()> {
        let mut conn =
            self.pool.acquire().await.map_err(|e| {
                ForgeError::internal_with("Failed to acquire bootstrap connection", e)
            })?;
        for statement in split_sql_statements(BOOTSTRAP_SQL) {
            let stmt = statement.trim();
            if is_empty_or_comment_only(stmt) {
                continue;
            }
            sqlx::query(stmt)
                .execute(&mut *conn)
                .await
                .map_err(|e| ForgeError::internal_with("Bootstrap failed", e))?;
        }
        Ok(())
    }

    async fn applied_versions(&self) -> Result<HashMap<String, String>> {
        let rows = sqlx::query!("SELECT version, checksum FROM forge_system_migrations")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to get applied migrations", e))?;

        Ok(rows
            .into_iter()
            .map(|row| (row.version, row.checksum))
            .collect())
    }

    async fn apply_migration(&self, migration: &Migration) -> Result<()> {
        if migration.transactional {
            self.apply_transactional(migration).await
        } else {
            self.apply_non_transactional(migration).await
        }
    }

    async fn apply_transactional(&self, migration: &Migration) -> Result<()> {
        info!("Applying migration: {}", migration.version);
        let start = Instant::now();

        let mut tx = self.pool.begin().await.map_err(|e| {
            ForgeError::internal_with(
                format!(
                    "Failed to begin migration transaction for '{}'",
                    migration.version
                ),
                e,
            )
        })?;

        // SET LOCAL only takes effect inside a transaction; it scopes these
        // timeouts to this migration and never leaks to other pool connections.
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut *tx)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to set lock_timeout", e))?;
        sqlx::query("SET LOCAL statement_timeout = '5min'")
            .execute(&mut *tx)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to set statement_timeout", e))?;

        for statement in split_sql_statements(&migration.up_sql) {
            let statement = statement.trim();
            if is_empty_or_comment_only(statement) {
                continue;
            }

            sqlx::query(statement)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    ForgeError::internal_with(
                        format!("Failed to apply migration '{}'", migration.version),
                        e,
                    )
                })?;
        }

        let checksum = crate::stable_hash::sha256_hex(migration.up_sql.as_bytes());
        sqlx::query!(
            "INSERT INTO forge_system_migrations (version, checksum) VALUES ($1, $2)",
            migration.version,
            checksum,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            ForgeError::internal_with(
                format!("Failed to record migration '{}'", migration.version),
                e,
            )
        })?;

        tx.commit().await.map_err(|e| {
            ForgeError::internal_with(
                format!("Failed to commit migration '{}'", migration.version),
                e,
            )
        })?;

        info!(
            "Migration applied: {} ({:?})",
            migration.version,
            start.elapsed()
        );
        Ok(())
    }

    /// Run a migration without wrapping it in a transaction. PostgreSQL
    /// rejects statements like `CREATE INDEX CONCURRENTLY`,
    /// `ALTER TYPE ... ADD VALUE`, `VACUUM`, and `REINDEX CONCURRENTLY`
    /// inside a transaction block, so opt-in migrations skip the BEGIN.
    ///
    /// Tradeoffs the migration author must accept:
    /// - A partial failure leaves the schema half-applied and the
    ///   bookkeeping row missing, so the next run will retry from the top.
    /// - Even if all DDL succeeds, the bookkeeping `INSERT` runs on a
    ///   *fresh* pool connection — if that insert fails, the migration is
    ///   re-run on the next startup despite already having taken effect.
    ///
    /// Migrations using this mode must be authored idempotently
    /// (`IF NOT EXISTS`, `ADD VALUE IF NOT EXISTS`, and so on).
    async fn apply_non_transactional(&self, migration: &Migration) -> Result<()> {
        info!(
            "Applying non-transactional migration: {}",
            migration.version
        );
        let start = Instant::now();

        let mut conn = self.pool.acquire().await.map_err(|e| {
            ForgeError::internal_with(
                format!(
                    "Failed to acquire connection for migration '{}'",
                    migration.version
                ),
                e,
            )
        })?;

        // Session-level SET (no LOCAL) since there's no surrounding tx; the
        // RESETs at the end keep the connection neutral for the pool.
        // Statement timeout is longer here than the transactional path
        // (5min) because the canonical use cases — CREATE INDEX
        // CONCURRENTLY on real-world tables, REINDEX CONCURRENTLY,
        // VACUUM — routinely run 10+ minutes on production data sets.
        sqlx::query("SET lock_timeout = '5s'")
            .execute(&mut *conn)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to set lock_timeout", e))?;
        sqlx::query("SET statement_timeout = '30min'")
            .execute(&mut *conn)
            .await
            .map_err(|e| ForgeError::internal_with("Failed to set statement_timeout", e))?;

        let exec_result: Result<()> = async {
            for statement in split_sql_statements(&migration.up_sql) {
                let statement = statement.trim();
                if is_empty_or_comment_only(statement) {
                    continue;
                }
                sqlx::query(statement)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| {
                        ForgeError::internal_with(
                            format!("Failed to apply migration '{}'", migration.version),
                            e,
                        )
                    })?;
            }
            Ok(())
        }
        .await;

        // Always neutralise the session settings before returning the
        // connection to the pool, even on failure. Errors are demoted to
        // WARN: a failed RESET is rare (PG accepts RESET even after errors),
        // but if it ever happens the connection still goes back into the
        // pool with non-default timeouts, which is something operators
        // need visibility into.
        if let Err(e) = sqlx::query("RESET lock_timeout").execute(&mut *conn).await {
            warn!(error = %e, "Failed to RESET lock_timeout after non-tx migration");
        }
        if let Err(e) = sqlx::query("RESET statement_timeout")
            .execute(&mut *conn)
            .await
        {
            warn!(error = %e, "Failed to RESET statement_timeout after non-tx migration");
        }
        drop(conn);

        exec_result?;

        let checksum = crate::stable_hash::sha256_hex(migration.up_sql.as_bytes());
        sqlx::query!(
            "INSERT INTO forge_system_migrations (version, checksum) VALUES ($1, $2)",
            migration.version,
            checksum,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| {
            ForgeError::internal_with(
                format!("Failed to record migration '{}'", migration.version),
                e,
            )
        })?;

        info!(
            "Non-transactional migration applied: {} ({:?})",
            migration.version,
            start.elapsed()
        );
        Ok(())
    }

    /// Get the status of all migrations. Each applied row is annotated with a
    /// `DriftStatus` so `forge migrate status` can distinguish three cases
    /// without triggering a full apply:
    ///
    /// - source still matches the recorded checksum,
    /// - source has been edited (drifted),
    /// - source file is gone entirely (the version is no longer in
    ///   `available` — usually a deleted migration file).
    pub async fn status(&self, available: &[Migration]) -> Result<MigrationStatus> {
        self.bootstrap_tracking_table().await?;

        let applied = self.applied_versions().await?;

        let available_by_version: HashMap<&str, &Migration> =
            available.iter().map(|m| (m.version.as_str(), m)).collect();

        let applied_list: Vec<AppliedMigration> = sqlx::query!(
            "SELECT version, applied_at, checksum FROM forge_system_migrations ORDER BY applied_at ASC"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ForgeError::internal_with("Failed to get migrations", e))?
        .into_iter()
        .map(|row| {
            let drift = match available_by_version.get(row.version.as_str()) {
                None => DriftStatus::SourceMissing,
                Some(m) => {
                    let computed = crate::stable_hash::sha256_hex(m.up_sql.as_bytes());
                    if computed == row.checksum {
                        DriftStatus::Unchanged
                    } else {
                        DriftStatus::Drifted {
                            current_checksum: computed,
                        }
                    }
                }
            };
            AppliedMigration {
                version: row.version,
                applied_at: row.applied_at,
                checksum: row.checksum,
                drift,
            }
        })
        .collect();

        let pending: Vec<String> = available
            .iter()
            .filter(|m| !applied.contains_key(&m.version))
            .map(|m| m.version.clone())
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
    pub version: String,
    pub applied_at: chrono::DateTime<chrono::Utc>,
    pub checksum: String,
    /// Relationship between the recorded checksum and the on-disk source.
    /// `forge migrate status` uses this to flag tampering or missing files
    /// without attempting a full apply.
    pub drift: DriftStatus,
}

/// Whether the on-disk source for an already-applied migration still matches
/// the checksum recorded at apply time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftStatus {
    /// Source file is present and its checksum matches the bookkeeping row.
    Unchanged,
    /// Source file is present but its checksum differs from the bookkeeping
    /// row — the file was edited after being applied.
    Drifted {
        /// SHA-256 of the on-disk source. Different from
        /// `AppliedMigration::checksum`, which holds the originally-recorded
        /// value.
        current_checksum: String,
    },
    /// Bookkeeping row exists but no migration with this version was supplied
    /// to `status()` — typically the file has been deleted from the
    /// migrations directory.
    SourceMissing,
}

/// Status of migrations.
#[derive(Debug, Clone)]
pub struct MigrationStatus {
    pub applied: Vec<AppliedMigration>,
    pub pending: Vec<String>,
}

/// Verify that a migration's source SQL matches the checksum recorded
/// when it was originally applied. This catches the silent-drift case
/// where someone edits an already-applied migration file: without the
/// check, `MigrationRunner` would happily skip the migration based on
/// version alone, and the file's new contents would never run.
fn verify_checksum(migration: &Migration, recorded: &str) -> Result<()> {
    let computed = crate::stable_hash::sha256_hex(migration.up_sql.as_bytes());
    if computed != recorded {
        return Err(ForgeError::internal(format!(
            "Migration '{}' has changed since it was applied. \
             Recorded checksum: {recorded}, but current file checksum: {computed}. \
             Migrations are immutable once applied — revert the file or create a new migration.",
            migration.version
        )));
    }
    Ok(())
}

/// Look up the PID currently holding the migration advisory lock, if any.
/// Returns `None` if the holder released between the failed try-lock and
/// this lookup, or if the SELECT itself failed — both are best-effort
/// diagnostics, never used as a correctness signal.
async fn lookup_lock_holder(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
    classid: i32,
    objid: i32,
) -> Option<i32> {
    sqlx::query_scalar!(
        r#"SELECT pid AS "pid!" FROM pg_locks
           WHERE locktype = 'advisory'
             AND classid::int = $1
             AND objid::int = $2
             AND granted
           LIMIT 1"#,
        classid,
        objid
    )
    .fetch_optional(&mut **conn)
    .await
    .ok()
    .flatten()
}

/// True for statements that are blank or made up entirely of `--` comment
/// lines. Such "statements" come out of `split_sql_statements` when the
/// migration ends with a trailing comment and would otherwise be sent to
/// PostgreSQL as an empty query.
fn is_empty_or_comment_only(stmt: &str) -> bool {
    stmt.is_empty()
        || stmt.lines().all(|l| {
            let l = l.trim();
            l.is_empty() || l.starts_with("--")
        })
}

/// Split SQL into individual statements, respecting dollar-quoted strings.
/// Handles PL/pgSQL functions that contain semicolons inside $$ delimiters.
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

/// Load user migrations from a directory.
///
/// Migrations must be named with a zero-padded numeric prefix followed by
/// an underscore and a slug, e.g. `0001_create_users.sql`. All migrations
/// in the directory must use the same prefix width, otherwise alphabetic
/// ordering silently desynchronizes from numeric ordering.
pub fn load_migrations_from_dir(dir: &Path) -> Result<Vec<Migration>> {
    if !dir.exists() {
        debug!("Migrations directory does not exist: {:?}", dir);
        return Ok(Vec::new());
    }

    let mut migrations = Vec::new();
    let mut prefix_width: Option<usize> = None;
    let mut seen_versions: std::collections::HashSet<u64> = std::collections::HashSet::new();

    let entries = std::fs::read_dir(dir).map_err(ForgeError::Io)?;

    for entry in entries {
        let entry = entry.map_err(ForgeError::Io)?;
        let path = entry.path();

        if path.extension().map(|e| e == "sql").unwrap_or(false) {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| ForgeError::config("Invalid migration filename"))?
                .to_string();

            let (digits, version) = parse_migration_prefix(&name)?;

            match prefix_width {
                Some(w) if w != digits.len() => {
                    return Err(ForgeError::config(format!(
                        "Inconsistent migration prefix width: {} uses {} digits but earlier migrations use {}. \
                         Pad all migration filenames to the same width (e.g. 0001_*.sql).",
                        name,
                        digits.len(),
                        w,
                    )));
                }
                None => prefix_width = Some(digits.len()),
                _ => {}
            }

            if !seen_versions.insert(version) {
                return Err(ForgeError::config(format!(
                    "Duplicate migration version {} for {}",
                    version, name
                )));
            }

            let content = std::fs::read_to_string(&path).map_err(ForgeError::Io)?;

            migrations.push((version, Migration::parse(name, &content)));
        }
    }

    migrations.sort_by_key(|(v, _)| *v);

    debug!("Loaded {} user migrations", migrations.len());
    Ok(migrations.into_iter().map(|(_, m)| m).collect())
}

/// Split a migration filename like `0001_create_users` into its digit prefix
/// and parsed numeric version. Errors out for missing or non-numeric prefixes
/// rather than letting them sort lexically and silently run out of order.
fn parse_migration_prefix(name: &str) -> Result<(&str, u64)> {
    let digits_end = name
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(name.len());
    if digits_end == 0 {
        return Err(ForgeError::config(format!(
            "Migration {} is missing a numeric prefix (expected NNNN_name.sql)",
            name
        )));
    }
    let digits = name.get(..digits_end).unwrap_or("");
    let version: u64 = digits.parse().map_err(|_| {
        ForgeError::config(format!(
            "Migration {} has an unparseable numeric prefix",
            name
        ))
    })?;
    Ok((digits, version))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;
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

        fs::write(dir.path().join("0002_second.sql"), "SELECT 2;").unwrap();
        fs::write(dir.path().join("0001_first.sql"), "SELECT 1;").unwrap();
        fs::write(dir.path().join("0003_third.sql"), "SELECT 3;").unwrap();

        let migrations = load_migrations_from_dir(dir.path()).unwrap();
        assert_eq!(migrations.len(), 3);
        assert_eq!(migrations[0].version, "0001_first");
        assert_eq!(migrations[1].version, "0002_second");
        assert_eq!(migrations[2].version, "0003_third");
    }

    #[test]
    fn test_load_migrations_rejects_mixed_prefix_widths() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("0001_first.sql"), "SELECT 1;").unwrap();
        fs::write(dir.path().join("2_second.sql"), "SELECT 2;").unwrap();

        let err = load_migrations_from_dir(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Inconsistent migration prefix width") || msg.contains("digits"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_load_migrations_rejects_missing_prefix() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("create_users.sql"), "SELECT 1;").unwrap();

        let err = load_migrations_from_dir(dir.path()).unwrap_err();
        assert!(err.to_string().contains("missing a numeric prefix"));
    }

    #[test]
    fn test_load_migrations_rejects_duplicate_versions() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("0001_a.sql"), "SELECT 1;").unwrap();
        fs::write(dir.path().join("0001_b.sql"), "SELECT 2;").unwrap();

        let err = load_migrations_from_dir(dir.path()).unwrap_err();
        assert!(err.to_string().contains("Duplicate migration version"));
    }

    #[test]
    fn test_load_migrations_ignores_non_sql() {
        let dir = TempDir::new().unwrap();

        fs::write(dir.path().join("0001_migration.sql"), "SELECT 1;").unwrap();
        fs::write(dir.path().join("readme.txt"), "Not a migration").unwrap();
        fs::write(dir.path().join("backup.sql.bak"), "Backup").unwrap();

        let migrations = load_migrations_from_dir(dir.path()).unwrap();
        assert_eq!(migrations.len(), 1);
        assert_eq!(migrations[0].version, "0001_migration");
    }

    #[test]
    fn test_migration_new() {
        let m = Migration::new("test", "SELECT 1");
        assert_eq!(m.version, "test");
        assert_eq!(m.up_sql, "SELECT 1");
    }

    #[test]
    fn test_migration_parse_strips_up_marker() {
        let content = "-- @up\nCREATE TABLE users (id INT);";
        let m = Migration::parse("0001_test", content);
        assert_eq!(m.version, "0001_test");
        assert_eq!(m.up_sql, "CREATE TABLE users (id INT);");
        assert!(m.transactional, "default should be transactional=true");
    }

    #[test]
    fn test_migration_parse_no_directive_defaults_transactional() {
        let m = Migration::parse("0001_test", "CREATE TABLE x (id INT);");
        assert!(m.transactional);
    }

    #[test]
    fn test_migration_parse_transactional_false_directive() {
        let content = "-- @transactional false\nCREATE INDEX CONCURRENTLY idx_x ON t(c);";
        let m = Migration::parse("0001_idx", content);
        assert!(!m.transactional);
        assert!(m.up_sql.contains("CREATE INDEX CONCURRENTLY"));
    }

    #[test]
    fn test_migration_parse_transactional_no_space_directive() {
        let content = "--@transactional false\nCREATE INDEX CONCURRENTLY idx_x ON t(c);";
        let m = Migration::parse("0001_idx", content);
        assert!(!m.transactional);
    }

    #[test]
    fn test_migration_parse_transactional_equals_form() {
        let content = "-- @transactional = false\nVACUUM ANALYZE;";
        let m = Migration::parse("0001_vac", content);
        assert!(!m.transactional);
    }

    #[test]
    fn test_migration_parse_transactional_uppercase_value() {
        let content = "-- @transactional FALSE\nVACUUM;";
        let m = Migration::parse("0001_vac", content);
        assert!(!m.transactional);
    }

    #[test]
    fn test_migration_parse_transactional_true_explicit() {
        let content = "-- @transactional true\nCREATE TABLE t (id INT);";
        let m = Migration::parse("0001_t", content);
        assert!(m.transactional);
    }

    #[test]
    fn test_migration_parse_directive_only_in_leading_block() {
        // A `-- @transactional false` after real SQL must not flip the flag —
        // the directive lives in the file header, not buried mid-file.
        let content = "CREATE TABLE t (id INT);\n-- @transactional false\nCREATE INDEX i ON t(id);";
        let m = Migration::parse("0001_t", content);
        assert!(m.transactional);
    }

    #[test]
    fn test_migration_parse_requires_at_prefix() {
        // Without `@`, the line is plain prose and must not flip the flag —
        // even when it spells out "transactional false".
        let content = "-- transactional false (this is just prose)\nCREATE TABLE t (id INT);";
        let m = Migration::parse("0001_t", content);
        assert!(m.transactional);
    }

    #[test]
    fn test_migration_parse_prose_only_no_directive() {
        let content = "-- This migration creates a transactional ledger\nCREATE TABLE t (id INT);";
        let m = Migration::parse("0001_t", content);
        assert!(m.transactional);
    }

    #[test]
    fn test_migration_parse_directive_after_blank_lines_in_header() {
        let content =
            "\n\n-- file header\n-- @transactional false\nCREATE INDEX CONCURRENTLY i ON t(id);";
        let m = Migration::parse("0001_t", content);
        assert!(!m.transactional);
    }

    #[test]
    fn test_migration_new_defaults_transactional() {
        let m = Migration::new("v", "SELECT 1");
        assert!(m.transactional);
    }

    #[test]
    fn test_is_empty_or_comment_only() {
        assert!(is_empty_or_comment_only(""));
        assert!(is_empty_or_comment_only("-- just a comment"));
        assert!(is_empty_or_comment_only("-- one\n-- two\n   "));
        assert!(!is_empty_or_comment_only("SELECT 1"));
        assert!(!is_empty_or_comment_only("-- header\nSELECT 1"));
    }

    #[tokio::test]
    async fn test_get_max_system_version_prefers_highest_applied_version() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("lazy pool must build");
        let runner = MigrationRunner::new(pool);

        let applied = HashMap::from([
            ("__forge_v003".to_string(), "checksum-3".to_string()),
            ("__forge_v001".to_string(), "checksum-1".to_string()),
            ("0001_user_schema".to_string(), "checksum-u".to_string()),
        ]);

        assert_eq!(runner.get_max_system_version(&applied), Some(3));
    }

    #[test]
    fn test_verify_checksum_matches() {
        let m = Migration::new("0001_test", "CREATE TABLE t (id INT);");
        let computed = crate::stable_hash::sha256_hex(m.up_sql.as_bytes());
        verify_checksum(&m, &computed).expect("matching checksum should pass");
    }

    #[test]
    fn test_verify_checksum_catches_system_migration_drift() {
        // The system-migration loop runs the same verify_checksum logic as
        // the user path, but pinning it directly here guards against future
        // refactors that might accidentally skip the check for built-ins.
        let migrations = super::super::builtin::get_system_migrations();
        let sys = migrations
            .first()
            .expect("at least one system migration is bundled");
        let migration = sys.to_migration();
        let real_checksum = crate::stable_hash::sha256_hex(migration.up_sql.as_bytes());
        verify_checksum(&migration, &real_checksum)
            .expect("matching checksum must pass for system migrations");

        let err = verify_checksum(&migration, "stale-checksum").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&migration.version),
            "drift error must name the system migration: {msg}"
        );
    }

    #[test]
    fn test_verify_checksum_mismatch_reports_versions() {
        let m = Migration::new("0001_test", "CREATE TABLE t (id INT);");
        let err = verify_checksum(&m, "deadbeef-old-checksum").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("0001_test"),
            "error should name the migration: {msg}"
        );
        assert!(
            msg.contains("deadbeef-old-checksum"),
            "error should include recorded checksum: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("changed") || msg.to_lowercase().contains("immutable"),
            "error should explain the drift policy: {msg}"
        );
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
        base.isolated(test_name)
            .await
            .expect("Failed to create isolated db")
    }

    /// CREATE INDEX CONCURRENTLY is rejected by PG inside a transaction
    /// block. The non-transactional path opens no BEGIN, so it must succeed.
    #[tokio::test]
    async fn non_transactional_migration_runs_create_index_concurrently() {
        let db = setup_db("mig_non_tx_create_index").await;
        let runner = MigrationRunner::new(db.pool().clone());

        let setup = Migration::new(
            "0001_setup",
            "CREATE TABLE items (id INT PRIMARY KEY, name TEXT);",
        );
        let concurrent = Migration::parse(
            "0002_index",
            "-- @transactional false\nCREATE INDEX CONCURRENTLY items_name_idx ON items(name);",
        );
        assert!(!concurrent.transactional);

        runner
            .run(vec![setup, concurrent])
            .await
            .expect("migrations apply cleanly");

        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                SELECT 1 FROM pg_indexes
                WHERE schemaname='public' AND tablename='items' AND indexname='items_name_idx'
            ) AS "exists!""#
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(exists, "index should be created");

        let recorded = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!" FROM forge_system_migrations WHERE version='0002_index'"#
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(recorded, 1, "non-tx migration must record bookkeeping row");
    }

    /// Trying to run CREATE INDEX CONCURRENTLY in the default (transactional)
    /// path must fail with PG's "cannot run inside a transaction block" error
    /// — proves we actually need the opt-out and aren't silently transacting.
    #[tokio::test]
    async fn transactional_migration_rejects_create_index_concurrently() {
        let db = setup_db("mig_tx_rejects_concurrent").await;
        let runner = MigrationRunner::new(db.pool().clone());

        let setup = Migration::new(
            "0001_setup",
            "CREATE TABLE items (id INT PRIMARY KEY, name TEXT);",
        );
        // No directive, so transactional defaults to true.
        let concurrent = Migration::new(
            "0002_index",
            "CREATE INDEX CONCURRENTLY items_name_idx ON items(name);",
        );
        assert!(concurrent.transactional);

        let err = runner.run(vec![setup, concurrent]).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("CONCURRENTLY") || msg.to_lowercase().contains("transaction"),
            "expected PG to reject concurrent index in tx, got: {msg}"
        );
    }

    /// Editing a migration after it has been applied must be caught by
    /// checksum verification. Without this check, the runner would silently
    /// skip the edited migration based on version alone, leaving the file
    /// out of sync with the database.
    #[tokio::test]
    async fn rerun_with_modified_sql_errors_with_checksum_drift() {
        let db = setup_db("mig_checksum_drift").await;
        let runner = MigrationRunner::new(db.pool().clone());

        let original = Migration::new("0001_users", "CREATE TABLE users (id INT PRIMARY KEY);");
        runner
            .run(vec![original])
            .await
            .expect("first run applies cleanly");

        // Same version, different SQL — simulates someone editing the
        // migration file after deploying.
        let tampered = Migration::new(
            "0001_users",
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);",
        );
        let err = runner.run(vec![tampered]).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("0001_users") && msg.to_lowercase().contains("changed"),
            "expected drift error mentioning the migration, got: {msg}"
        );
    }

    /// `status()` must surface checksum drift via the `drift` field. Without
    /// this, `forge migrate status` silently reports a tampered migration as
    /// "applied" — operators learn about it only on the next `migrate up`.
    /// This drives the user-facing UX of the audit fix.
    #[tokio::test]
    async fn status_surfaces_checksum_drift_on_modified_migration() {
        let db = setup_db("mig_status_drift").await;
        let runner = MigrationRunner::new(db.pool().clone());

        let original = Migration::new("0001_users", "CREATE TABLE users (id INT PRIMARY KEY);");
        runner
            .run(vec![original])
            .await
            .expect("first run applies cleanly");

        let tampered = Migration::new(
            "0001_users",
            "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
        );
        let status = runner
            .status(std::slice::from_ref(&tampered))
            .await
            .expect("status must succeed even with drift");

        let row = status
            .applied
            .iter()
            .find(|a| a.version == "0001_users")
            .expect("applied row must exist");
        let expected = crate::stable_hash::sha256_hex(tampered.up_sql.as_bytes());
        match &row.drift {
            DriftStatus::Drifted { current_checksum } => {
                assert_eq!(
                    current_checksum, &expected,
                    "current_checksum must equal the *new* on-disk checksum",
                );
                assert_ne!(
                    current_checksum, &row.checksum,
                    "current_checksum must differ from the recorded checksum",
                );
            }
            other => panic!("expected DriftStatus::Drifted, got {other:?}"),
        }
    }

    /// `status()` must report `SourceMissing` for an applied migration whose
    /// file no longer exists on disk — that's a louder signal than "looks
    /// fine", which is what a single Option<String> conflated. Operators need
    /// to see deleted migrations as a distinct condition.
    #[tokio::test]
    async fn status_reports_source_missing_when_file_gone() {
        let db = setup_db("mig_status_missing").await;
        let runner = MigrationRunner::new(db.pool().clone());

        let m = Migration::new("0001_users", "CREATE TABLE users (id INT PRIMARY KEY);");
        runner
            .run(vec![m])
            .await
            .expect("first run applies cleanly");

        // Simulate the migration file being deleted: pass an empty slice.
        let status = runner
            .status(&[])
            .await
            .expect("status must succeed with missing source");

        let row = status
            .applied
            .iter()
            .find(|a| a.version == "0001_users")
            .expect("applied row must exist");
        assert_eq!(row.drift, DriftStatus::SourceMissing);
    }

    /// Sanity: when the on-disk source matches the recorded checksum,
    /// `drift` is `Unchanged` (not the same as `SourceMissing`).
    #[tokio::test]
    async fn status_reports_unchanged_when_source_matches() {
        let db = setup_db("mig_status_unchanged").await;
        let runner = MigrationRunner::new(db.pool().clone());

        let m = Migration::new("0001_users", "CREATE TABLE users (id INT PRIMARY KEY);");
        runner
            .run(vec![m.clone()])
            .await
            .expect("first run applies cleanly");

        let status = runner
            .status(std::slice::from_ref(&m))
            .await
            .expect("status must succeed for clean source");

        let row = status
            .applied
            .iter()
            .find(|a| a.version == "0001_users")
            .expect("applied row must exist");
        assert_eq!(row.drift, DriftStatus::Unchanged);
    }

    /// When another connection is already holding the migration advisory lock,
    /// the runner must time out with a helpful message rather than block
    /// forever. We grab the lock manually then run a runner with a tiny
    /// timeout and assert the error mentions the holder PID.
    #[tokio::test]
    async fn lock_acquire_times_out_when_another_holder_present() {
        let db = setup_db("mig_lock_timeout").await;

        // Hold the migration lock from a separate connection.
        let mut blocker = db.pool().acquire().await.unwrap();
        let acquired = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) AS "ok!""#,
            MIGRATION_LOCK_ID
        )
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
        assert!(acquired, "blocker must acquire the lock first");

        let config = MigrationConfig {
            lock_acquire_timeout: Duration::from_millis(500),
            lock_poll_interval: Duration::from_millis(50),
            lock_warn_interval: Duration::from_secs(60),
        };
        let runner = MigrationRunner::with_config(db.pool().clone(), config);

        let err = runner.run(vec![]).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Timed out") && msg.contains("migration lock"),
            "expected timeout error, got: {msg}"
        );
        assert!(
            msg.contains("holder pid"),
            "expected holder pid in error: {msg}"
        );

        // Release on the blocker side so test cleanup is clean.
        sqlx::query_scalar!("SELECT pg_advisory_unlock($1)", MIGRATION_LOCK_ID)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
    }

    /// Bring up the system schema (so `forge_validate_identifier` and the
    /// reactivity helpers exist) and return the pool.
    async fn db_with_system_schema(name: &str) -> IsolatedTestDb {
        let db = setup_db(name).await;
        MigrationRunner::new(db.pool().clone())
            .run(vec![])
            .await
            .expect("system migrations apply cleanly");
        db
    }

    /// Pull the message out of a Postgres error so assertions don't depend
    /// on the surrounding sqlx error wrapping.
    fn pg_message(err: &sqlx::Error) -> String {
        match err {
            sqlx::Error::Database(db_err) => db_err.message().to_string(),
            other => other.to_string(),
        }
    }

    #[tokio::test]
    async fn validate_identifier_rejects_empty() {
        let db = db_with_system_schema("vid_empty").await;
        let err = sqlx::query("SELECT forge_validate_identifier('')")
            .execute(db.pool())
            .await
            .unwrap_err();
        let msg = pg_message(&err);
        assert!(
            msg.contains("empty"),
            "expected empty-name error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn validate_identifier_rejects_overlong_name() {
        let db = db_with_system_schema("vid_overlong").await;
        // 64 ASCII bytes — one over PG's 63-byte budget.
        let name = "a".repeat(64);
        let err = sqlx::query(&format!("SELECT forge_validate_identifier('{name}')"))
            .execute(db.pool())
            .await
            .unwrap_err();
        let msg = pg_message(&err);
        assert!(
            msg.contains("63 bytes"),
            "expected 63-byte limit in error, got: {msg}",
        );
    }

    #[tokio::test]
    async fn validate_identifier_rejects_pg_prefix() {
        let db = db_with_system_schema("vid_pgprefix").await;
        let err = sqlx::query("SELECT forge_validate_identifier('pg_my_table')")
            .execute(db.pool())
            .await
            .unwrap_err();
        let msg = pg_message(&err);
        assert!(
            msg.contains("pg_") || msg.to_lowercase().contains("reserved"),
            "expected pg_ reservation error, got: {msg}",
        );
    }

    #[tokio::test]
    async fn validate_identifier_accepts_valid_name() {
        let db = db_with_system_schema("vid_ok").await;
        sqlx::query("SELECT forge_validate_identifier('orders_2026')")
            .execute(db.pool())
            .await
            .expect("normal identifier must be accepted");
    }

    /// Starting with migrations the binary does not know about must fail.
    /// This simulates the rolling-deploy case: a newer binary ran user migration
    /// `0002_extra`; the older binary then starts up and must refuse rather than
    /// silently ignoring the unknown row.
    #[tokio::test]
    async fn startup_rejects_schema_ahead_of_binary() {
        let db = setup_db("mig_schema_ahead").await;
        let runner = MigrationRunner::new(db.pool().clone());

        // Newer binary ran both migrations.
        let m1 = Migration::new("0001_users", "CREATE TABLE users (id INT PRIMARY KEY);");
        let m2 = Migration::new("0002_extra", "CREATE TABLE extra (id INT PRIMARY KEY);");
        runner
            .run(vec![m1.clone(), m2])
            .await
            .expect("newer binary applies both cleanly");

        // Older binary only knows about 0001 — must refuse to start.
        let err = runner
            .run(vec![m1])
            .await
            .expect_err("older binary must refuse to start against a newer schema");
        let msg = err.to_string();
        assert!(
            msg.contains("0002_extra"),
            "error must name the unknown migration: {msg}",
        );
        assert!(
            msg.to_lowercase().contains("ahead") || msg.to_lowercase().contains("does not know"),
            "error must explain the schema-ahead condition: {msg}",
        );
    }

    /// `forge_enable_reactivity` must catch the case where the input passes
    /// (≤ 63 bytes) but the derived `forge_notify_<name>` trigger overflows.
    /// 51 bytes of input pushes the prefixed trigger name to 64.
    #[tokio::test]
    async fn enable_reactivity_rejects_derived_trigger_overflow() {
        let db = db_with_system_schema("enrx_overflow").await;
        let name = "a".repeat(51);
        let err = sqlx::query(&format!("SELECT forge_enable_reactivity('{name}')"))
            .execute(db.pool())
            .await
            .unwrap_err();
        let msg = pg_message(&err);
        assert!(
            msg.contains("63 bytes"),
            "expected derived-name overflow error, got: {msg}",
        );
    }
}

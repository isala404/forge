//! Structural schema verification, run after migrations (and in verify-only mode).
//!
//! Every migration uses `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`.
//! That is safe for re-runs, but it means a pre-existing `forge_*` table with an
//! *incompatible* shape would be left untouched and the migration still recorded as
//! applied — Forge would then issue queries against a table that does not match what
//! it expects, failing confusingly at runtime instead of loudly at init.
//!
//! This check closes that gap: it reads `information_schema.columns` and confirms every
//! table Forge owns has the required columns with the expected types. A mismatch is a
//! [`ForgeError::Config`] at `Forge::init`, per the principle that misconfiguration
//! fails at init, never lazily. Forge reserves all `forge_*` objects; apps must not
//! create their own table with one of these names.

use crate::error::{ForgeError, Result};
use sqlx::PgPool;
use std::collections::HashMap;

/// A required column: name + its expected `information_schema.columns.data_type`.
struct Col(&'static str, &'static str);

/// A required table and the columns Forge depends on.
struct TableSpec {
    name: &'static str,
    cols: &'static [Col],
}

// information_schema `data_type` spellings: TEXT->"text", BYTEA->"bytea",
// TIMESTAMPTZ->"timestamp with time zone", UUID->"uuid", INT->"integer",
// BIGINT->"bigint", DOUBLE PRECISION->"double precision", JSONB->"jsonb",
// VARCHAR->"character varying".
const TSTZ: &str = "timestamp with time zone";

/// The schema every Forge backend expects after migrations. Derived from
/// `src/migrations/*.sql`; keep in lockstep when a migration adds or changes a column.
fn expected_tables() -> &'static [TableSpec] {
    &[
        TableSpec {
            name: "forge_system_migrations",
            cols: &[
                Col("version", "character varying"),
                Col("applied_at", TSTZ),
                Col("checksum", "character varying"),
            ],
        },
        TableSpec {
            name: "forge_kv",
            cols: &[
                Col("key", "text"),
                Col("value", "bytea"),
                Col("expires_at", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_jobs",
            cols: &[
                Col("id", "uuid"),
                Col("queue", "text"),
                Col("payload", "bytea"),
                Col("status", "text"),
                Col("attempts", "integer"),
                Col("max_attempts", "integer"),
                Col("backoff", "jsonb"),
                Col("available_at", TSTZ),
                Col("leased_until", TSTZ),
                Col("lease_token", "uuid"),
                Col("lease_secs", "double precision"),
                Col("enqueued_at", TSTZ),
                Col("completed_at", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_job_dedup",
            cols: &[
                Col("queue", "text"),
                Col("dedup_id", "text"),
                Col("job_id", "uuid"),
                Col("expires_at", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_config",
            cols: &[Col("key", "text"), Col("value", "text")],
        },
        TableSpec {
            name: "forge_flags",
            cols: &[Col("key", "text"), Col("rule", "jsonb")],
        },
        TableSpec {
            name: "forge_ratelimit",
            cols: &[
                Col("bucket", "text"),
                Col("subject", "text"),
                Col("tokens", "double precision"),
                Col("window_start", "double precision"),
                Col("cur_count", "integer"),
                Col("prev_count", "integer"),
                Col("updated_at", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_blobs",
            cols: &[
                Col("key", "text"),
                Col("data", "bytea"),
                Col("content_type", "text"),
                Col("etag", "text"),
                Col("metadata", "jsonb"),
                Col("size", "bigint"),
                Col("last_modified", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_sessions",
            cols: &[
                Col("token_hash", "text"),
                Col("user_id", "text"),
                Col("idle_secs", "double precision"),
                Col("created_at", TSTZ),
                Col("idle_deadline", TSTZ),
                Col("abs_deadline", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_api_keys",
            cols: &[
                Col("id", "text"),
                Col("key_hash", "text"),
                Col("owner_id", "text"),
                Col("label", "text"),
                Col("created_at", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_fs_blobs",
            cols: &[
                Col("key", "text"),
                Col("data_ref", "text"),
                Col("content_type", "text"),
                Col("etag", "text"),
                Col("metadata", "jsonb"),
                Col("size", "bigint"),
                Col("last_modified", TSTZ),
            ],
        },
        TableSpec {
            name: "forge_schedules",
            cols: &[
                Col("name", "text"),
                Col("kind", "text"),
                Col("cron_expr", "text"),
                Col("target_queue", "text"),
                Col("payload", "bytea"),
                Col("job_id", "uuid"),
                Col("next_run", TSTZ),
                Col("last_run", TSTZ),
                Col("created_at", TSTZ),
            ],
        },
    ]
}

/// Verify every Forge-owned table exists in the current schema with the required
/// columns and types. `Config` on the first mismatch, with a precise message.
pub(crate) async fn verify_schema(pool: &PgPool) -> Result<()> {
    for table in expected_tables() {
        let actual = columns_of(pool, table.name).await?;
        if actual.is_empty() {
            return Err(ForgeError::config(format!(
                "required table '{}' is missing from the current schema after migrations \
                 (Forge reserves all forge_* objects)",
                table.name
            )));
        }
        for Col(col, expected) in table.cols {
            match actual.get(*col) {
                None => {
                    return Err(ForgeError::config(format!(
                        "table '{}' is missing required column '{}' — a pre-existing table with an \
                         incompatible shape? Forge reserves forge_* objects; rename or drop yours.",
                        table.name, col
                    )));
                }
                Some(found) if found != expected => {
                    return Err(ForgeError::config(format!(
                        "table '{}' column '{}' has type '{}', but Forge expects '{}' — \
                         incompatible pre-existing schema for a reserved forge_* table",
                        table.name, col, found, expected
                    )));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

/// Map of column_name -> data_type for `table` in the current schema (empty if absent).
async fn columns_of(pool: &PgPool, table: &str) -> Result<HashMap<String, String>> {
    let rows = sqlx::query!(
        "SELECT column_name, data_type FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = $1",
        table
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.column_name?, r.data_type?)))
        .collect())
}

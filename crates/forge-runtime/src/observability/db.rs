use opentelemetry::global;
use opentelemetry::metrics::{Gauge, Histogram};
use sqlx::PgPool;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tracing::{Instrument, Level, debug_span, enabled, info_span};

const DB_SYSTEM: &str = "db.system";
const DB_OPERATION_NAME: &str = "db.operation.name";
const DB_SYSTEM_POSTGRESQL: &str = "postgresql";

// Catch performance regressions before they hit production
const SLOW_QUERY_THRESHOLD: Duration = Duration::from_millis(500);

static DB_METRICS: OnceLock<DbMetrics> = OnceLock::new();

struct DbMetrics {
    query_duration: Histogram<f64>,
    pool_connections_active: Gauge<u64>,
    pool_connections_idle: Gauge<u64>,
    pool_connections_max: Gauge<u64>,
}

fn get_metrics() -> &'static DbMetrics {
    DB_METRICS.get_or_init(|| {
        let meter = global::meter("forge.db");

        DbMetrics {
            query_duration: meter
                .f64_histogram("db.client.operation.duration")
                .with_description("Duration of database operations")
                .with_unit("s")
                .build(),
            pool_connections_active: meter
                .u64_gauge("db.client.connection.count")
                .with_description("Number of active database connections")
                .build(),
            pool_connections_idle: meter
                .u64_gauge("db.client.connection.idle_count")
                .with_description("Number of idle database connections")
                .build(),
            pool_connections_max: meter
                .u64_gauge("db.client.connection.max")
                .with_description("Maximum number of database connections")
                .build(),
        }
    })
}

/// Record pool connection metrics from a PgPool.
pub fn record_pool_metrics(pool: &PgPool) {
    let metrics = get_metrics();
    let pool_size = pool.size();
    let idle_count = pool.num_idle();
    let max_connections = pool.options().get_max_connections();

    metrics.pool_connections_active.record(
        (pool_size - idle_count as u32) as u64,
        &[opentelemetry::KeyValue::new(
            DB_SYSTEM,
            DB_SYSTEM_POSTGRESQL,
        )],
    );
    metrics.pool_connections_idle.record(
        idle_count as u64,
        &[opentelemetry::KeyValue::new(
            DB_SYSTEM,
            DB_SYSTEM_POSTGRESQL,
        )],
    );
    metrics.pool_connections_max.record(
        max_connections as u64,
        &[opentelemetry::KeyValue::new(
            DB_SYSTEM,
            DB_SYSTEM_POSTGRESQL,
        )],
    );
}

/// Record a query execution with its duration.
pub fn record_query_duration(operation: &str, duration: Duration) {
    let metrics = get_metrics();
    metrics.query_duration.record(
        duration.as_secs_f64(),
        &[
            opentelemetry::KeyValue::new(DB_SYSTEM, DB_SYSTEM_POSTGRESQL),
            opentelemetry::KeyValue::new(DB_OPERATION_NAME, operation.to_string()),
        ],
    );
}

/// Extract the table name from a simple SQL query, or `None` for complex ones.
///
/// Walks the source by `char_indices` rather than fixed byte offsets so
/// non-ASCII identifiers (quoted Unicode columns/tables) can't panic the
/// slicer. `to_uppercase()` can change the byte length of a string, so we
/// can't reuse byte offsets discovered in the uppercased copy against the
/// original — locate keywords case-insensitively over the original instead.
pub fn extract_table_name(sql: &str) -> Option<&str> {
    let sql = sql.trim();

    if let Some(rest) = strip_keyword_prefix(sql, "INSERT INTO ")
        .or_else(|| strip_keyword_prefix(sql, "DELETE FROM "))
        .or_else(|| strip_keyword_prefix(sql, "CREATE TABLE IF NOT EXISTS "))
        .or_else(|| strip_keyword_prefix(sql, "CREATE TABLE "))
        .or_else(|| strip_keyword_prefix(sql, "UPDATE "))
    {
        return extract_first_identifier(rest.trim_start());
    }

    if strip_keyword_prefix(sql, "SELECT").is_some() {
        // Find " FROM " case-insensitively without re-allocating a full
        // uppercase copy whose byte length can diverge from the source.
        if let Some(from_byte) = find_ci(sql, " FROM ") {
            let after = sql.get(from_byte + " FROM ".len()..)?;
            return extract_first_identifier(after.trim_start());
        }
    }

    None
}

fn strip_keyword_prefix<'a>(sql: &'a str, keyword: &str) -> Option<&'a str> {
    if sql.len() < keyword.len() {
        return None;
    }
    let prefix = sql.get(..keyword.len())?;
    if prefix.eq_ignore_ascii_case(keyword) {
        sql.get(keyword.len()..)
    } else {
        None
    }
}

/// Case-insensitive search for an ASCII needle. Returns the byte offset of
/// the first match in the source.
fn find_ci(haystack: &str, needle_ascii_upper: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let n = needle_ascii_upper.as_bytes();
    if n.is_empty() || bytes.len() < n.len() {
        return None;
    }
    'outer: for start in 0..=bytes.len() - n.len() {
        for (i, nb) in n.iter().enumerate() {
            let hb = bytes.get(start + i)?;
            if !hb.eq_ignore_ascii_case(nb) {
                continue 'outer;
            }
        }
        // Confirm the match begins on a UTF-8 char boundary so the caller's
        // slice never bisects a multi-byte sequence.
        if haystack.is_char_boundary(start) && haystack.is_char_boundary(start + n.len()) {
            return Some(start);
        }
    }
    None
}

fn extract_first_identifier(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == ';')
        .unwrap_or(s.len());

    if end > 0 { s.get(..end) } else { None }
}

/// Execute a database operation with tracing and duration recording.
pub async fn instrumented_query<F, T, E>(operation: &str, table: Option<&str>, f: F) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    // Skip span allocation entirely when DEBUG isn't enabled — saves the
    // ~few-hundred-ns alloc per query when the operator runs at warn/info.
    let span = if !enabled!(Level::DEBUG) {
        debug_span!("db.query")
    } else if let Some(tbl) = table {
        info_span!(
            "db.query",
            db.system = DB_SYSTEM_POSTGRESQL,
            db.operation.name = operation,
            db.collection.name = tbl,
        )
    } else {
        info_span!(
            "db.query",
            db.system = DB_SYSTEM_POSTGRESQL,
            db.operation.name = operation,
        )
    };

    let start = Instant::now();
    let result = f.instrument(span).await;
    let elapsed = start.elapsed();
    record_query_duration(operation, elapsed);

    if elapsed > SLOW_QUERY_THRESHOLD {
        tracing::warn!(
            db.operation.name = operation,
            db.collection.name = table,
            duration_ms = elapsed.as_millis() as u64,
            "Slow query detected"
        );
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_table_select() {
        assert_eq!(
            extract_table_name("SELECT * FROM users WHERE id = 1"),
            Some("users")
        );
        assert_eq!(
            extract_table_name("SELECT id, name FROM accounts"),
            Some("accounts")
        );
        assert_eq!(extract_table_name("select * from Orders"), Some("Orders"));
    }

    #[test]
    fn test_extract_table_insert() {
        assert_eq!(
            extract_table_name("INSERT INTO users (id, name) VALUES (1, 'test')"),
            Some("users")
        );
    }

    #[test]
    fn test_extract_table_update() {
        assert_eq!(
            extract_table_name("UPDATE users SET name = 'test' WHERE id = 1"),
            Some("users")
        );
    }

    #[test]
    fn test_extract_table_delete() {
        assert_eq!(
            extract_table_name("DELETE FROM users WHERE id = 1"),
            Some("users")
        );
    }

    #[test]
    fn test_extract_table_create() {
        assert_eq!(
            extract_table_name("CREATE TABLE users (id UUID PRIMARY KEY)"),
            Some("users")
        );
        assert_eq!(
            extract_table_name("CREATE TABLE IF NOT EXISTS accounts (id INT)"),
            Some("accounts")
        );
    }

    #[test]
    fn test_extract_table_complex_query() {
        // Complex queries should still find the first table
        assert_eq!(
            extract_table_name("SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id"),
            Some("users")
        );
    }
}

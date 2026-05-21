use opentelemetry::global;
use opentelemetry::metrics::{Gauge, Histogram};
use sqlx::PgPool;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tracing::{Instrument, info_span};

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
pub fn extract_table_name(sql: &str) -> Option<&str> {
    let sql = sql.trim();
    let upper = sql.to_uppercase();

    if upper.starts_with("SELECT") {
        // SELECT ... FROM table_name ...
        if let Some(from_pos) = upper.find(" FROM ") {
            let after_from = &sql[from_pos + 6..];
            return extract_first_identifier(after_from.trim_start());
        }
    } else if upper.starts_with("INSERT INTO ") {
        let after_into = &sql[12..];
        return extract_first_identifier(after_into.trim_start());
    } else if upper.starts_with("UPDATE ") {
        let after_update = &sql[7..];
        return extract_first_identifier(after_update.trim_start());
    } else if upper.starts_with("DELETE FROM ") {
        let after_from = &sql[12..];
        return extract_first_identifier(after_from.trim_start());
    } else if upper.starts_with("CREATE TABLE ") {
        let after_table = if upper.starts_with("CREATE TABLE IF NOT EXISTS ") {
            &sql[27..]
        } else {
            &sql[13..]
        };
        return extract_first_identifier(after_table.trim_start());
    }

    None
}

fn extract_first_identifier(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == ';')
        .unwrap_or(s.len());

    if end > 0 { Some(&s[..end]) } else { None }
}

/// Execute a database operation with tracing and duration recording.
pub async fn instrumented_query<F, T, E>(operation: &str, table: Option<&str>, f: F) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    let span = if let Some(tbl) = table {
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

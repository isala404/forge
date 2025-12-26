use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::Row;
use tokio::sync::RwLock;

use forge_core::observability::{LogEntry, Metric, MetricKind, MetricValue, Span, SpanStatus};
use forge_core::LogLevel;

/// Maximum number of items to insert in a single batch.
const BATCH_SIZE: usize = 1000;

/// Metrics store for persisting metrics to PostgreSQL.
pub struct MetricsStore {
    pool: sqlx::PgPool,
    /// Metrics waiting to be written (buffer for batching).
    pending: Arc<RwLock<Vec<Metric>>>,
}

impl MetricsStore {
    /// Create a new metrics store.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            pending: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Store a batch of metrics to the database.
    pub async fn store(&self, metrics: Vec<Metric>) -> forge_core::Result<()> {
        if metrics.is_empty() {
            return Ok(());
        }

        // Process in batches to avoid hitting parameter limits
        for chunk in metrics.chunks(BATCH_SIZE) {
            self.insert_batch(chunk).await?;
        }

        Ok(())
    }

    /// Insert a batch of metrics using UNNEST.
    async fn insert_batch(&self, metrics: &[Metric]) -> forge_core::Result<()> {
        let names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        let kinds: Vec<String> = metrics.iter().map(|m| m.kind.to_string()).collect();
        let values: Vec<f64> = metrics
            .iter()
            .map(|m| m.value.as_value().unwrap_or(0.0))
            .collect();
        let labels: Vec<serde_json::Value> = metrics
            .iter()
            .map(|m| serde_json::to_value(&m.labels).unwrap_or(serde_json::Value::Null))
            .collect();
        let timestamps: Vec<DateTime<Utc>> = metrics.iter().map(|m| m.timestamp).collect();

        sqlx::query(
            r#"
            INSERT INTO forge_metrics (name, kind, value, labels, timestamp)
            SELECT * FROM UNNEST($1::TEXT[], $2::TEXT[], $3::FLOAT8[], $4::JSONB[], $5::TIMESTAMPTZ[])
            "#,
        )
        .bind(&names)
        .bind(&kinds)
        .bind(&values)
        .bind(&labels)
        .bind(&timestamps)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Query metrics by name and time range.
    pub async fn query(
        &self,
        name: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> forge_core::Result<Vec<Metric>> {
        let rows = sqlx::query(
            r#"
            SELECT name, kind, value, labels, timestamp
            FROM forge_metrics
            WHERE name = $1 AND timestamp >= $2 AND timestamp <= $3
            ORDER BY timestamp DESC
            LIMIT 1000
            "#,
        )
        .bind(name)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let metrics = rows
            .into_iter()
            .map(|row| {
                let name: String = row.get("name");
                let kind_str: String = row.get("kind");
                let value: f64 = row.get("value");
                let labels: serde_json::Value = row.get("labels");
                let timestamp: DateTime<Utc> = row.get("timestamp");

                let kind = match kind_str.as_str() {
                    "counter" => MetricKind::Counter,
                    "gauge" => MetricKind::Gauge,
                    "histogram" => MetricKind::Histogram,
                    "summary" => MetricKind::Summary,
                    _ => MetricKind::Gauge,
                };

                let labels_map: HashMap<String, String> =
                    serde_json::from_value(labels).unwrap_or_default();

                Metric {
                    name,
                    kind,
                    value: MetricValue::Value(value),
                    labels: labels_map,
                    timestamp,
                    description: None,
                }
            })
            .collect();

        Ok(metrics)
    }

    /// Get latest value for each unique metric name.
    pub async fn list_latest(&self) -> forge_core::Result<Vec<Metric>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT ON (name) name, kind, value, labels, timestamp
            FROM forge_metrics
            ORDER BY name, timestamp DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let metrics = rows
            .into_iter()
            .map(|row| {
                let name: String = row.get("name");
                let kind_str: String = row.get("kind");
                let value: f64 = row.get("value");
                let labels: serde_json::Value = row.get("labels");
                let timestamp: DateTime<Utc> = row.get("timestamp");

                let kind = match kind_str.as_str() {
                    "counter" => MetricKind::Counter,
                    "gauge" => MetricKind::Gauge,
                    "histogram" => MetricKind::Histogram,
                    "summary" => MetricKind::Summary,
                    _ => MetricKind::Gauge,
                };

                Metric {
                    name,
                    kind,
                    value: MetricValue::Value(value),
                    labels: serde_json::from_value(labels).unwrap_or_default(),
                    timestamp,
                    description: None,
                }
            })
            .collect();

        Ok(metrics)
    }

    /// Get pending count (items buffered but not yet flushed).
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Run cleanup to remove old metrics.
    pub async fn cleanup(&self, retention: Duration) -> forge_core::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::from_std(retention).unwrap();

        let result = sqlx::query("DELETE FROM forge_metrics WHERE timestamp < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}

/// Log store for persisting logs to PostgreSQL.
pub struct LogStore {
    pool: sqlx::PgPool,
    /// Logs waiting to be written.
    pending: Arc<RwLock<Vec<LogEntry>>>,
}

impl LogStore {
    /// Create a new log store.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            pending: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Store a batch of logs to the database.
    pub async fn store(&self, logs: Vec<LogEntry>) -> forge_core::Result<()> {
        if logs.is_empty() {
            return Ok(());
        }

        for chunk in logs.chunks(BATCH_SIZE) {
            self.insert_batch(chunk).await?;
        }

        Ok(())
    }

    /// Insert a batch of logs using UNNEST.
    async fn insert_batch(&self, logs: &[LogEntry]) -> forge_core::Result<()> {
        let levels: Vec<String> = logs.iter().map(|l| l.level.to_string()).collect();
        let messages: Vec<&str> = logs.iter().map(|l| l.message.as_str()).collect();
        let targets: Vec<Option<&str>> = logs.iter().map(|l| l.target.as_deref()).collect();
        let fields: Vec<serde_json::Value> = logs
            .iter()
            .map(|l| serde_json::to_value(&l.fields).unwrap_or(serde_json::Value::Null))
            .collect();
        let trace_ids: Vec<Option<String>> = logs.iter().map(|l| l.trace_id.clone()).collect();
        let span_ids: Vec<Option<String>> = logs.iter().map(|l| l.span_id.clone()).collect();
        let timestamps: Vec<DateTime<Utc>> = logs.iter().map(|l| l.timestamp).collect();

        sqlx::query(
            r#"
            INSERT INTO forge_logs (level, message, target, fields, trace_id, span_id, timestamp)
            SELECT * FROM UNNEST($1::TEXT[], $2::TEXT[], $3::TEXT[], $4::JSONB[], $5::TEXT[], $6::TEXT[], $7::TIMESTAMPTZ[])
            "#,
        )
        .bind(&levels)
        .bind(&messages)
        .bind(&targets)
        .bind(&fields)
        .bind(&trace_ids)
        .bind(&span_ids)
        .bind(&timestamps)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Query logs with filters.
    pub async fn query(
        &self,
        level: Option<LogLevel>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: usize,
    ) -> forge_core::Result<Vec<LogEntry>> {
        let level_filter = level.map(|l| l.to_string());

        let rows = sqlx::query(
            r#"
            SELECT id, level, message, target, fields, trace_id, span_id, timestamp
            FROM forge_logs
            WHERE ($1::TEXT IS NULL OR level = $1)
              AND ($2::TIMESTAMPTZ IS NULL OR timestamp >= $2)
              AND ($3::TIMESTAMPTZ IS NULL OR timestamp <= $3)
            ORDER BY timestamp DESC
            LIMIT $4
            "#,
        )
        .bind(&level_filter)
        .bind(from)
        .bind(to)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let logs = rows
            .into_iter()
            .map(|row| {
                let level_str: String = row.get("level");
                let message: String = row.get("message");
                let target: Option<String> = row.get("target");
                let fields: serde_json::Value = row.get("fields");
                let timestamp: DateTime<Utc> = row.get("timestamp");

                let level = match level_str.to_lowercase().as_str() {
                    "trace" => LogLevel::Trace,
                    "debug" => LogLevel::Debug,
                    "info" => LogLevel::Info,
                    "warn" => LogLevel::Warn,
                    "error" => LogLevel::Error,
                    _ => LogLevel::Info,
                };

                LogEntry {
                    level,
                    message,
                    target,
                    fields: serde_json::from_value(fields).unwrap_or_default(),
                    trace_id: None,
                    span_id: None,
                    timestamp,
                    node_id: None,
                }
            })
            .collect();

        Ok(logs)
    }

    /// Search logs by message content.
    pub async fn search(&self, query: &str, limit: usize) -> forge_core::Result<Vec<LogEntry>> {
        let search_pattern = format!("%{}%", query);

        let rows = sqlx::query(
            r#"
            SELECT id, level, message, target, fields, trace_id, span_id, timestamp
            FROM forge_logs
            WHERE message ILIKE $1
            ORDER BY timestamp DESC
            LIMIT $2
            "#,
        )
        .bind(&search_pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let logs = rows
            .into_iter()
            .map(|row| {
                let level_str: String = row.get("level");
                let message: String = row.get("message");
                let target: Option<String> = row.get("target");
                let fields: serde_json::Value = row.get("fields");
                let timestamp: DateTime<Utc> = row.get("timestamp");

                LogEntry {
                    level: LogLevel::from_str(&level_str).unwrap_or_default(),
                    message,
                    target,
                    fields: serde_json::from_value(fields).unwrap_or_default(),
                    trace_id: None,
                    span_id: None,
                    timestamp,
                    node_id: None,
                }
            })
            .collect();

        Ok(logs)
    }

    /// Get pending count.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Run cleanup to remove old logs.
    pub async fn cleanup(&self, retention: Duration) -> forge_core::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::from_std(retention).unwrap();

        let result = sqlx::query("DELETE FROM forge_logs WHERE timestamp < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}

/// Trace store for persisting traces to PostgreSQL.
pub struct TraceStore {
    pool: sqlx::PgPool,
    /// Traces indexed by trace ID (for in-flight spans).
    traces: Arc<RwLock<HashMap<String, Vec<Span>>>>,
}

impl TraceStore {
    /// Create a new trace store.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            traces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Store a batch of spans to the database.
    pub async fn store(&self, spans: Vec<Span>) -> forge_core::Result<()> {
        if spans.is_empty() {
            return Ok(());
        }

        for chunk in spans.chunks(BATCH_SIZE) {
            self.insert_batch(chunk).await?;
        }

        Ok(())
    }

    /// Insert a batch of spans using UNNEST.
    async fn insert_batch(&self, spans: &[Span]) -> forge_core::Result<()> {
        let ids: Vec<uuid::Uuid> = spans.iter().map(|_| uuid::Uuid::new_v4()).collect();
        let trace_ids: Vec<String> = spans
            .iter()
            .map(|s| s.context.trace_id.to_string())
            .collect();
        let span_ids: Vec<String> = spans
            .iter()
            .map(|s| s.context.span_id.to_string())
            .collect();
        let parent_ids: Vec<Option<String>> = spans
            .iter()
            .map(|s| s.context.parent_span_id.as_ref().map(|id| id.to_string()))
            .collect();
        let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();
        let kinds: Vec<String> = spans.iter().map(|s| s.kind.to_string()).collect();
        let statuses: Vec<String> = spans.iter().map(|s| s.status.to_string()).collect();
        let attributes: Vec<serde_json::Value> = spans
            .iter()
            .map(|s| serde_json::to_value(&s.attributes).unwrap_or(serde_json::Value::Null))
            .collect();
        let events: Vec<serde_json::Value> = spans
            .iter()
            .map(|s| serde_json::to_value(&s.events).unwrap_or(serde_json::Value::Null))
            .collect();
        let start_times: Vec<DateTime<Utc>> = spans.iter().map(|s| s.start_time).collect();
        let end_times: Vec<Option<DateTime<Utc>>> = spans.iter().map(|s| s.end_time).collect();
        let durations: Vec<Option<i32>> = spans
            .iter()
            .map(|s| s.duration_ms().map(|d| d as i32))
            .collect();

        sqlx::query(
            r#"
            INSERT INTO forge_traces (
                id, trace_id, span_id, parent_span_id, name, kind, status,
                attributes, events, started_at, ended_at, duration_ms
            )
            SELECT * FROM UNNEST(
                $1::UUID[], $2::TEXT[], $3::TEXT[], $4::TEXT[], $5::TEXT[], $6::TEXT[], $7::TEXT[],
                $8::JSONB[], $9::JSONB[], $10::TIMESTAMPTZ[], $11::TIMESTAMPTZ[], $12::INT[]
            )
            "#,
        )
        .bind(&ids)
        .bind(&trace_ids)
        .bind(&span_ids)
        .bind(&parent_ids)
        .bind(&names)
        .bind(&kinds)
        .bind(&statuses)
        .bind(&attributes)
        .bind(&events)
        .bind(&start_times)
        .bind(&end_times)
        .bind(&durations)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Get a trace by ID.
    pub async fn get_trace(&self, trace_id: &str) -> forge_core::Result<Vec<Span>> {
        let rows = sqlx::query(
            r#"
            SELECT trace_id, span_id, parent_span_id, name, kind, status,
                   attributes, events, started_at, ended_at, duration_ms
            FROM forge_traces
            WHERE trace_id = $1
            ORDER BY started_at ASC
            "#,
        )
        .bind(trace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let spans = rows
            .into_iter()
            .map(|row| {
                let name: String = row.get("name");
                let kind_str: String = row.get("kind");
                let status_str: String = row.get("status");
                let start_time: DateTime<Utc> = row.get("started_at");
                let end_time: Option<DateTime<Utc>> = row.get("ended_at");

                let mut span = Span::new(&name);
                span.start_time = start_time;
                span.end_time = end_time;
                span.status = match status_str.as_str() {
                    "ok" => SpanStatus::Ok,
                    "error" => SpanStatus::Error,
                    _ => SpanStatus::Unset,
                };
                span.attributes = row
                    .get::<serde_json::Value, _>("attributes")
                    .as_object()
                    .cloned()
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default();

                span
            })
            .collect();

        Ok(spans)
    }

    /// Query traces by time range.
    pub async fn query(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: usize,
    ) -> forge_core::Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT trace_id
            FROM forge_traces
            WHERE started_at >= $1 AND started_at <= $2
            ORDER BY trace_id
            LIMIT $3
            "#,
        )
        .bind(from)
        .bind(to)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.get("trace_id")).collect())
    }

    /// List recent traces with summary info.
    pub async fn list_recent(&self, limit: usize) -> forge_core::Result<Vec<TraceSummary>> {
        let rows = sqlx::query(
            r#"
            WITH trace_stats AS (
                SELECT
                    trace_id,
                    MIN(started_at) as started_at,
                    MAX(duration_ms) as duration_ms,
                    COUNT(*) as span_count,
                    BOOL_OR(status = 'error') as has_error,
                    (array_agg(name ORDER BY started_at ASC))[1] as root_span_name
                FROM forge_traces
                GROUP BY trace_id
                ORDER BY started_at DESC
                LIMIT $1
            )
            SELECT * FROM trace_stats
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let summaries = rows
            .into_iter()
            .map(|row| TraceSummary {
                trace_id: row.get("trace_id"),
                root_span_name: row.get("root_span_name"),
                started_at: row.get("started_at"),
                duration_ms: row.get::<Option<i32>, _>("duration_ms").map(|d| d as u64),
                span_count: row.get::<i64, _>("span_count") as u32,
                has_error: row.get("has_error"),
            })
            .collect();

        Ok(summaries)
    }

    /// Find traces with errors.
    pub async fn find_errors(&self, limit: usize) -> forge_core::Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT trace_id
            FROM forge_traces
            WHERE status = 'error'
            ORDER BY trace_id
            LIMIT $1
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.get("trace_id")).collect())
    }

    /// Get trace count.
    pub async fn trace_count(&self) -> usize {
        self.traces.read().await.len()
    }

    /// Get total span count.
    pub async fn span_count(&self) -> usize {
        self.traces.read().await.values().map(|v| v.len()).sum()
    }

    /// Run cleanup to remove old traces.
    pub async fn cleanup(&self, retention: Duration) -> forge_core::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::from_std(retention).unwrap();

        let result = sqlx::query("DELETE FROM forge_traces WHERE started_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}

/// Summary of a trace for listing.
#[derive(Debug, Clone)]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_span_name: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub span_count: u32,
    pub has_error: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::observability::MetricKind;

    #[tokio::test]
    async fn test_metrics_store_basic() {
        // Test with lazy pool (doesn't connect)
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = MetricsStore::new(pool);

        // pending_count should work even without real connection
        assert_eq!(store.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_log_store_basic() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = LogStore::new(pool);

        assert_eq!(store.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_trace_store_basic() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = TraceStore::new(pool);

        assert_eq!(store.trace_count().await, 0);
        assert_eq!(store.span_count().await, 0);
    }
}

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use forge_core::observability::{LogEntry, Metric, Span};
use forge_core::LogLevel;

/// Metrics store for persisting metrics to PostgreSQL.
pub struct MetricsStore {
    pool: sqlx::PgPool,
    /// Metrics waiting to be written.
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

    /// Store a batch of metrics.
    pub async fn store(&self, metrics: Vec<Metric>) -> forge_core::Result<()> {
        if metrics.is_empty() {
            return Ok(());
        }

        // In a real implementation, this would insert into forge_metrics table
        // For now, we just store in memory for testing
        let mut pending = self.pending.write().await;
        pending.extend(metrics);

        Ok(())
    }

    /// Query metrics by name and time range.
    pub async fn query(
        &self,
        name: &str,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> forge_core::Result<Vec<Metric>> {
        let pending = self.pending.read().await;
        Ok(pending
            .iter()
            .filter(|m| m.name == name && m.timestamp >= from && m.timestamp <= to)
            .cloned()
            .collect())
    }

    /// Get pending count.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Run cleanup to remove old metrics.
    pub async fn cleanup(&self, retention: Duration) -> forge_core::Result<u64> {
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(retention).unwrap();
        let mut pending = self.pending.write().await;
        let before = pending.len();
        pending.retain(|m| m.timestamp > cutoff);
        Ok((before - pending.len()) as u64)
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

    /// Store a batch of logs.
    pub async fn store(&self, logs: Vec<LogEntry>) -> forge_core::Result<()> {
        if logs.is_empty() {
            return Ok(());
        }

        let mut pending = self.pending.write().await;
        pending.extend(logs);

        Ok(())
    }

    /// Query logs with filters.
    pub async fn query(
        &self,
        level: Option<LogLevel>,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
        limit: usize,
    ) -> forge_core::Result<Vec<LogEntry>> {
        let pending = self.pending.read().await;
        Ok(pending
            .iter()
            .filter(|l| {
                let level_match = level.map_or(true, |lvl| l.level >= lvl);
                let from_match = from.map_or(true, |f| l.timestamp >= f);
                let to_match = to.map_or(true, |t| l.timestamp <= t);
                level_match && from_match && to_match
            })
            .take(limit)
            .cloned()
            .collect())
    }

    /// Search logs by message content.
    pub async fn search(&self, query: &str, limit: usize) -> forge_core::Result<Vec<LogEntry>> {
        let pending = self.pending.read().await;
        Ok(pending
            .iter()
            .filter(|l| l.message.contains(query))
            .take(limit)
            .cloned()
            .collect())
    }

    /// Get pending count.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Run cleanup to remove old logs.
    pub async fn cleanup(&self, retention: Duration) -> forge_core::Result<u64> {
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(retention).unwrap();
        let mut pending = self.pending.write().await;
        let before = pending.len();
        pending.retain(|l| l.timestamp > cutoff);
        Ok((before - pending.len()) as u64)
    }
}

/// Trace store for persisting traces to PostgreSQL.
pub struct TraceStore {
    pool: sqlx::PgPool,
    /// Traces indexed by trace ID.
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

    /// Store a batch of spans.
    pub async fn store(&self, spans: Vec<Span>) -> forge_core::Result<()> {
        if spans.is_empty() {
            return Ok(());
        }

        let mut traces = self.traces.write().await;
        for span in spans {
            traces
                .entry(span.context.trace_id.to_string())
                .or_default()
                .push(span);
        }

        Ok(())
    }

    /// Get a trace by ID.
    pub async fn get_trace(&self, trace_id: &str) -> forge_core::Result<Vec<Span>> {
        let traces = self.traces.read().await;
        Ok(traces.get(trace_id).cloned().unwrap_or_default())
    }

    /// Query traces by time range.
    pub async fn query(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> forge_core::Result<Vec<String>> {
        let traces = self.traces.read().await;

        // Get unique trace IDs that have spans in the time range
        let mut trace_ids: Vec<String> = traces
            .iter()
            .filter(|(_, spans)| {
                spans
                    .iter()
                    .any(|s| s.start_time >= from && s.start_time <= to)
            })
            .map(|(id, _)| id.clone())
            .take(limit)
            .collect();

        trace_ids.sort();
        Ok(trace_ids)
    }

    /// Find traces with errors.
    pub async fn find_errors(&self, limit: usize) -> forge_core::Result<Vec<String>> {
        let traces = self.traces.read().await;

        let trace_ids: Vec<String> = traces
            .iter()
            .filter(|(_, spans)| {
                spans
                    .iter()
                    .any(|s| s.status == forge_core::SpanStatus::Error)
            })
            .map(|(id, _)| id.clone())
            .take(limit)
            .collect();

        Ok(trace_ids)
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
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(retention).unwrap();
        let mut traces = self.traces.write().await;
        let before = traces.len();

        // Remove traces where all spans are older than cutoff
        traces.retain(|_, spans| spans.iter().any(|s| s.start_time > cutoff));

        Ok((before - traces.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::observability::{MetricKind, SpanContext};

    #[tokio::test]
    async fn test_metrics_store_basic() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = MetricsStore::new(pool);

        let metrics = vec![
            Metric::counter("test_counter", 1.0),
            Metric::gauge("test_gauge", 42.0),
        ];

        store.store(metrics).await.unwrap();
        assert_eq!(store.pending_count().await, 2);
    }

    #[tokio::test]
    async fn test_log_store_basic() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = LogStore::new(pool);

        let logs = vec![
            LogEntry::info("Test message 1"),
            LogEntry::error("Test error"),
        ];

        store.store(logs).await.unwrap();
        assert_eq!(store.pending_count().await, 2);
    }

    #[tokio::test]
    async fn test_log_store_search() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = LogStore::new(pool);

        let logs = vec![
            LogEntry::info("User login successful"),
            LogEntry::error("Database connection failed"),
            LogEntry::info("User logout"),
        ];

        store.store(logs).await.unwrap();

        let results = store.search("User", 10).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_trace_store_basic() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = TraceStore::new(pool);

        let span = Span::new("test_span");
        let trace_id = span.context.trace_id.to_string();

        store.store(vec![span]).await.unwrap();

        let trace = store.get_trace(&trace_id).await.unwrap();
        assert_eq!(trace.len(), 1);
    }

    #[tokio::test]
    async fn test_trace_store_find_errors() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let store = TraceStore::new(pool);

        let mut error_span = Span::new("error_span");
        error_span.end_error("Something went wrong");
        let error_trace_id = error_span.context.trace_id.to_string();

        let ok_span = Span::new("ok_span");

        store.store(vec![error_span, ok_span]).await.unwrap();

        let errors = store.find_errors(10).await.unwrap();
        assert!(errors.contains(&error_trace_id));
    }
}

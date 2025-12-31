use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;

use super::DashboardState;

/// Dashboard API handlers.
pub struct DashboardApi;

/// Query parameters for time range.
#[derive(Debug, Deserialize)]
pub struct TimeRangeQuery {
    /// Start time (ISO 8601).
    pub start: Option<DateTime<Utc>>,
    /// End time (ISO 8601).
    pub end: Option<DateTime<Utc>>,
    /// Period shorthand (1h, 24h, 7d, 30d).
    pub period: Option<String>,
}

impl TimeRangeQuery {
    /// Get the time range, defaulting to last hour.
    fn get_range(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let end = self.end.unwrap_or_else(Utc::now);
        let start = self.start.unwrap_or_else(|| {
            match self.period.as_deref() {
                Some("1h") => end - Duration::hours(1),
                Some("24h") => end - Duration::hours(24),
                Some("7d") => end - Duration::days(7),
                Some("30d") => end - Duration::days(30),
                _ => end - Duration::hours(1), // Default to 1 hour
            }
        });
        (start, end)
    }
}

/// Query parameters for pagination.
#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    /// Page number (1-indexed).
    pub page: Option<u32>,
    /// Items per page.
    pub limit: Option<u32>,
}

impl PaginationQuery {
    fn get_limit(&self) -> i64 {
        self.limit.unwrap_or(50).min(1000) as i64
    }

    fn get_offset(&self) -> i64 {
        let page = self.page.unwrap_or(1).max(1);
        ((page - 1) * self.limit.unwrap_or(50)) as i64
    }
}

/// Query parameters for log search.
#[derive(Debug, Deserialize)]
pub struct LogSearchQuery {
    /// Log level filter.
    pub level: Option<String>,
    /// Search query.
    pub q: Option<String>,
    /// Start time.
    pub start: Option<DateTime<Utc>>,
    /// End time.
    pub end: Option<DateTime<Utc>>,
    /// Limit.
    pub limit: Option<u32>,
}

/// Query parameters for trace search.
#[derive(Debug, Deserialize)]
pub struct TraceSearchQuery {
    /// Service filter.
    #[allow(dead_code)]
    pub service: Option<String>,
    /// Operation filter.
    #[allow(dead_code)]
    pub operation: Option<String>,
    /// Minimum duration in ms.
    #[allow(dead_code)]
    pub min_duration: Option<u64>,
    /// Only errors.
    pub errors_only: Option<bool>,
    /// Start time.
    pub start: Option<DateTime<Utc>>,
    /// End time.
    pub end: Option<DateTime<Utc>>,
    /// Limit.
    pub limit: Option<u32>,
}

/// Metric summary response.
#[derive(Debug, Serialize)]
pub struct MetricSummary {
    pub name: String,
    pub kind: String,
    pub description: Option<String>,
    pub current_value: f64,
    pub labels: HashMap<String, String>,
    pub last_updated: DateTime<Utc>,
}

/// Metric series point.
#[derive(Debug, Serialize)]
pub struct MetricPoint {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
}

/// Metric series response.
#[derive(Debug, Serialize)]
pub struct MetricSeries {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub points: Vec<MetricPoint>,
}

/// Log entry response.
#[derive(Debug, Serialize)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub fields: HashMap<String, serde_json::Value>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

/// Trace summary response.
#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_span_name: String,
    pub service: String,
    pub duration_ms: u64,
    pub span_count: u32,
    pub error: bool,
    pub started_at: DateTime<Utc>,
}

/// Trace detail response.
#[derive(Debug, Serialize)]
pub struct TraceDetail {
    pub trace_id: String,
    pub spans: Vec<SpanDetail>,
}

/// Span detail.
#[derive(Debug, Serialize)]
pub struct SpanDetail {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub service: String,
    pub kind: String,
    pub status: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub attributes: HashMap<String, serde_json::Value>,
    pub events: Vec<SpanEvent>,
}

/// Span event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    pub name: String,
    pub timestamp: DateTime<Utc>,
    pub attributes: HashMap<String, serde_json::Value>,
}

/// Alert summary.
#[derive(Debug, Serialize)]
pub struct AlertSummary {
    pub id: String,
    pub rule_id: String,
    pub name: String,
    pub severity: String,
    pub status: String,
    pub metric_value: f64,
    pub threshold: f64,
    pub fired_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
}

/// Alert rule summary.
#[derive(Debug, Serialize)]
pub struct AlertRuleSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub metric_name: String,
    pub condition: String,
    pub threshold: f64,
    pub severity: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

/// Alert rule creation request.
#[derive(Debug, Deserialize)]
pub struct CreateAlertRuleRequest {
    pub name: String,
    pub description: Option<String>,
    pub metric_name: String,
    pub condition: String,
    pub threshold: f64,
    pub duration_seconds: Option<i32>,
    pub severity: Option<String>,
    pub cooldown_seconds: Option<i32>,
}

/// Alert rule update request.
#[derive(Debug, Deserialize)]
pub struct UpdateAlertRuleRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub metric_name: Option<String>,
    pub condition: Option<String>,
    pub threshold: Option<f64>,
    pub duration_seconds: Option<i32>,
    pub severity: Option<String>,
    pub enabled: Option<bool>,
    pub cooldown_seconds: Option<i32>,
}

/// Alert acknowledge request.
#[derive(Debug, Deserialize)]
pub struct AcknowledgeAlertRequest {
    pub acknowledged_by: String,
}

/// Job stats.
#[derive(Debug, Serialize)]
pub struct JobStats {
    pub pending: u64,
    pub running: u64,
    pub completed: u64,
    pub failed: u64,
    pub retrying: u64,
    pub dead_letter: u64,
}

/// Workflow stats.
#[derive(Debug, Serialize)]
pub struct WorkflowStats {
    pub running: u64,
    pub completed: u64,
    pub waiting: u64,
    pub failed: u64,
    pub compensating: u64,
}

/// Workflow run summary.
#[derive(Debug, Serialize)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_name: String,
    pub version: Option<String>,
    pub status: String,
    pub current_step: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// Job detail with progress info.
#[derive(Debug, Serialize)]
pub struct JobDetail {
    pub id: String,
    pub job_type: String,
    pub status: String,
    pub priority: i32,
    pub attempts: i32,
    pub max_attempts: i32,
    pub progress_percent: Option<i32>,
    pub progress_message: Option<String>,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub scheduled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// Workflow detail with steps.
#[derive(Debug, Serialize)]
pub struct WorkflowDetail {
    pub id: String,
    pub workflow_name: String,
    pub version: Option<String>,
    pub status: String,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub current_step: Option<String>,
    pub steps: Vec<WorkflowStepDetail>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// Workflow step detail.
#[derive(Debug, Serialize)]
pub struct WorkflowStepDetail {
    pub name: String,
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// Node info.
#[derive(Debug, Serialize)]
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub roles: Vec<String>,
    pub status: String,
    pub last_heartbeat: DateTime<Utc>,
    pub version: String,
    pub started_at: DateTime<Utc>,
}

/// Cluster health.
#[derive(Debug, Serialize)]
pub struct ClusterHealth {
    pub status: String,
    pub node_count: u32,
    pub healthy_nodes: u32,
    pub leader_node: Option<String>,
    pub leaders: HashMap<String, String>,
}

/// System info.
#[derive(Debug, Serialize)]
pub struct SystemInfo {
    pub version: String,
    pub rust_version: String,
    pub started_at: DateTime<Utc>,
    pub uptime_seconds: u64,
}

/// System stats.
#[derive(Debug, Serialize)]
pub struct SystemStats {
    pub http_requests_total: u64,
    pub http_requests_per_second: f64,
    pub p99_latency_ms: Option<f64>,
    pub function_calls_total: u64,
    pub active_connections: u32,
    pub active_subscriptions: u32,
    pub jobs_pending: u64,
    pub memory_used_mb: u64,
    pub cpu_usage_percent: f64,
}

/// API response wrapper.
#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

// ============================================================================
// Metrics API
// ============================================================================

/// List all metrics with their latest values.
pub async fn list_metrics(
    State(state): State<DashboardState>,
    Query(_query): Query<TimeRangeQuery>,
) -> Json<ApiResponse<Vec<MetricSummary>>> {
    let result = sqlx::query(
        r#"
        SELECT DISTINCT ON (name) name, kind, value, labels, timestamp
        FROM forge_metrics
        ORDER BY name, timestamp DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let metrics: Vec<MetricSummary> = rows
                .into_iter()
                .map(|row| {
                    let labels: serde_json::Value = row.get("labels");
                    MetricSummary {
                        name: row.get("name"),
                        kind: row.get("kind"),
                        description: None,
                        current_value: row.get("value"),
                        labels: serde_json::from_value(labels).unwrap_or_default(),
                        last_updated: row.get("timestamp"),
                    }
                })
                .collect();
            Json(ApiResponse::success(metrics))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get a specific metric by name.
pub async fn get_metric(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
    Query(query): Query<TimeRangeQuery>,
) -> Json<ApiResponse<MetricSummary>> {
    let (start, end) = query.get_range();

    let result = sqlx::query(
        r#"
        SELECT name, kind, value, labels, timestamp
        FROM forge_metrics
        WHERE name = $1 AND timestamp >= $2 AND timestamp <= $3
        ORDER BY timestamp DESC
        LIMIT 1
        "#,
    )
    .bind(&name)
    .bind(start)
    .bind(end)
    .fetch_optional(&state.pool)
    .await;

    match result {
        Ok(Some(row)) => {
            let labels: serde_json::Value = row.get("labels");
            Json(ApiResponse::success(MetricSummary {
                name: row.get("name"),
                kind: row.get("kind"),
                description: None,
                current_value: row.get("value"),
                labels: serde_json::from_value(labels).unwrap_or_default(),
                last_updated: row.get("timestamp"),
            }))
        }
        Ok(None) => Json(ApiResponse::error(format!("Metric '{}' not found", name))),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get metric time series for charts.
/// Aggregates counter metrics by time bucket for meaningful visualization.
pub async fn get_metric_series(
    State(state): State<DashboardState>,
    Query(query): Query<TimeRangeQuery>,
) -> Json<ApiResponse<Vec<MetricSeries>>> {
    let (start, end) = query.get_range();

    // Determine bucket interval based on time range
    let duration = end.signed_duration_since(start);
    let bucket_interval = if duration.num_hours() <= 1 {
        "1 minute" // 1h range -> 1 min buckets (60 points max)
    } else if duration.num_hours() <= 24 {
        "5 minutes" // 24h range -> 5 min buckets (288 points max)
    } else if duration.num_days() <= 7 {
        "1 hour" // 7d range -> 1 hour buckets (168 points max)
    } else {
        "1 day" // longer -> 1 day buckets
    };

    // Aggregate metrics by time bucket
    // For counter metrics (like http_requests_total), SUM the values
    // For gauge/histogram, take the last value in each bucket
    let result = sqlx::query(
        r#"
        WITH bucketed AS (
            SELECT
                name,
                labels,
                kind,
                date_trunc($3, timestamp) as bucket,
                SUM(value) as sum_value,
                MAX(value) as max_value,
                COUNT(*) as cnt
            FROM forge_metrics
            WHERE timestamp >= $1 AND timestamp <= $2
            GROUP BY name, labels, kind, date_trunc($3, timestamp)
            ORDER BY name, bucket
        )
        SELECT
            name,
            labels,
            bucket as timestamp,
            CASE
                WHEN kind = 'counter' THEN sum_value
                ELSE max_value
            END as value
        FROM bucketed
        ORDER BY name, bucket
        "#,
    )
    .bind(start)
    .bind(end)
    .bind(bucket_interval)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let mut series_map: HashMap<String, MetricSeries> = HashMap::new();

            for row in rows {
                let name: String = row.get("name");
                let value: f64 = row.get("value");
                let timestamp: DateTime<Utc> = row.get("timestamp");
                let labels: serde_json::Value = row.get("labels");

                let series = series_map
                    .entry(name.clone())
                    .or_insert_with(|| MetricSeries {
                        name: name.clone(),
                        labels: serde_json::from_value(labels).unwrap_or_default(),
                        points: Vec::new(),
                    });

                series.points.push(MetricPoint { timestamp, value });
            }

            Json(ApiResponse::success(series_map.into_values().collect()))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Logs API
// ============================================================================

/// List recent logs.
pub async fn list_logs(
    State(state): State<DashboardState>,
    Query(query): Query<LogSearchQuery>,
) -> Json<ApiResponse<Vec<LogEntry>>> {
    let limit = query.limit.unwrap_or(100).min(1000) as i64;
    let level_filter = query.level.as_deref();

    let result = sqlx::query(
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
    .bind(level_filter)
    .bind(query.start)
    .bind(query.end)
    .bind(limit)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let logs: Vec<LogEntry> = rows
                .into_iter()
                .map(|row| {
                    let id: i64 = row.get("id");
                    let fields: serde_json::Value = row.get("fields");
                    LogEntry {
                        id: id.to_string(),
                        timestamp: row.get("timestamp"),
                        level: row.get("level"),
                        message: row.get("message"),
                        fields: serde_json::from_value(fields).unwrap_or_default(),
                        trace_id: row.get("trace_id"),
                        span_id: row.get("span_id"),
                    }
                })
                .collect();
            Json(ApiResponse::success(logs))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Search logs by message content.
pub async fn search_logs(
    State(state): State<DashboardState>,
    Query(query): Query<LogSearchQuery>,
) -> Json<ApiResponse<Vec<LogEntry>>> {
    let limit = query.limit.unwrap_or(100).min(1000) as i64;
    let search_pattern = query.q.as_ref().map(|q| format!("%{}%", q));

    let result = sqlx::query(
        r#"
        SELECT id, level, message, target, fields, trace_id, span_id, timestamp
        FROM forge_logs
        WHERE ($1::TEXT IS NULL OR message ILIKE $1)
          AND ($2::TEXT IS NULL OR level = $2)
        ORDER BY timestamp DESC
        LIMIT $3
        "#,
    )
    .bind(&search_pattern)
    .bind(&query.level)
    .bind(limit)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let logs: Vec<LogEntry> = rows
                .into_iter()
                .map(|row| {
                    let id: i64 = row.get("id");
                    let fields: serde_json::Value = row.get("fields");
                    LogEntry {
                        id: id.to_string(),
                        timestamp: row.get("timestamp"),
                        level: row.get("level"),
                        message: row.get("message"),
                        fields: serde_json::from_value(fields).unwrap_or_default(),
                        trace_id: row.get("trace_id"),
                        span_id: row.get("span_id"),
                    }
                })
                .collect();
            Json(ApiResponse::success(logs))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Traces API
// ============================================================================

/// List recent traces.
pub async fn list_traces(
    State(state): State<DashboardState>,
    Query(query): Query<TraceSearchQuery>,
) -> Json<ApiResponse<Vec<TraceSummary>>> {
    let limit = query.limit.unwrap_or(50).min(1000) as i64;
    let errors_only = query.errors_only.unwrap_or(false);

    let result = sqlx::query(
        r#"
        WITH trace_stats AS (
            SELECT
                trace_id,
                MIN(started_at) as started_at,
                MAX(duration_ms) as duration_ms,
                COUNT(*) as span_count,
                BOOL_OR(status = 'error') as has_error,
                (array_agg(name ORDER BY started_at ASC))[1] as root_span_name,
                (array_agg(attributes->>'service.name' ORDER BY started_at ASC) FILTER (WHERE attributes->>'service.name' IS NOT NULL))[1] as service_name
            FROM forge_traces
            WHERE ($1::TIMESTAMPTZ IS NULL OR started_at >= $1)
              AND ($2::TIMESTAMPTZ IS NULL OR started_at <= $2)
            GROUP BY trace_id
        )
        SELECT * FROM trace_stats
        WHERE ($3::BOOLEAN = FALSE OR has_error = TRUE)
        ORDER BY started_at DESC
        LIMIT $4
        "#,
    )
    .bind(query.start)
    .bind(query.end)
    .bind(errors_only)
    .bind(limit)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let traces: Vec<TraceSummary> = rows
                .into_iter()
                .map(|row| TraceSummary {
                    trace_id: row.get("trace_id"),
                    root_span_name: row
                        .get::<Option<String>, _>("root_span_name")
                        .unwrap_or_default(),
                    service: row
                        .get::<Option<String>, _>("service_name")
                        .unwrap_or_else(|| "unknown".to_string()),
                    duration_ms: row.get::<Option<i32>, _>("duration_ms").unwrap_or(0) as u64,
                    span_count: row.get::<i64, _>("span_count") as u32,
                    error: row.get("has_error"),
                    started_at: row.get("started_at"),
                })
                .collect();
            Json(ApiResponse::success(traces))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get trace details with all spans.
pub async fn get_trace(
    State(state): State<DashboardState>,
    Path(trace_id): Path<String>,
) -> Json<ApiResponse<TraceDetail>> {
    let result = sqlx::query(
        r#"
        SELECT trace_id, span_id, parent_span_id, name, kind, status,
               attributes, events, started_at, ended_at, duration_ms
        FROM forge_traces
        WHERE trace_id = $1
        ORDER BY started_at ASC
        "#,
    )
    .bind(&trace_id)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) if !rows.is_empty() => {
            let spans: Vec<SpanDetail> = rows
                .into_iter()
                .map(|row| {
                    let attributes: serde_json::Value = row.get("attributes");
                    let events: serde_json::Value = row.get("events");
                    let end_time: Option<DateTime<Utc>> = row.get("ended_at");
                    let duration: Option<i32> = row.get("duration_ms");

                    // Extract service name from attributes if present
                    let service = attributes
                        .get("service.name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    SpanDetail {
                        span_id: row.get("span_id"),
                        parent_span_id: row.get("parent_span_id"),
                        name: row.get("name"),
                        service,
                        kind: row.get("kind"),
                        status: row.get("status"),
                        start_time: row.get("started_at"),
                        end_time,
                        duration_ms: duration.map(|d| d as u64),
                        attributes: serde_json::from_value(attributes).unwrap_or_default(),
                        events: serde_json::from_value(events).unwrap_or_default(),
                    }
                })
                .collect();

            Json(ApiResponse::success(TraceDetail { trace_id, spans }))
        }
        Ok(_) => Json(ApiResponse::error(format!(
            "Trace '{}' not found",
            trace_id
        ))),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Alerts API
// ============================================================================

/// List alerts.
pub async fn list_alerts(
    State(state): State<DashboardState>,
    Query(query): Query<PaginationQuery>,
) -> Json<ApiResponse<Vec<AlertSummary>>> {
    let limit = query.get_limit();
    let offset = query.get_offset();

    let result = sqlx::query(
        r#"
        SELECT id, rule_id, rule_name, metric_value, threshold, severity, status,
               triggered_at, resolved_at, acknowledged_at, acknowledged_by
        FROM forge_alerts
        ORDER BY triggered_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let alerts: Vec<AlertSummary> = rows
                .into_iter()
                .map(|row| {
                    let id: uuid::Uuid = row.get("id");
                    let rule_id: uuid::Uuid = row.get("rule_id");
                    AlertSummary {
                        id: id.to_string(),
                        rule_id: rule_id.to_string(),
                        name: row.get("rule_name"),
                        severity: row.get("severity"),
                        status: row.get("status"),
                        metric_value: row.get("metric_value"),
                        threshold: row.get("threshold"),
                        fired_at: row.get("triggered_at"),
                        resolved_at: row.get("resolved_at"),
                        acknowledged_at: row.get("acknowledged_at"),
                        acknowledged_by: row.get("acknowledged_by"),
                    }
                })
                .collect();
            Json(ApiResponse::success(alerts))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get active alerts.
pub async fn get_active_alerts(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<Vec<AlertSummary>>> {
    let result = sqlx::query(
        r#"
        SELECT id, rule_id, rule_name, metric_value, threshold, severity, status,
               triggered_at, resolved_at, acknowledged_at, acknowledged_by
        FROM forge_alerts
        WHERE status = 'firing'
        ORDER BY triggered_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let alerts: Vec<AlertSummary> = rows
                .into_iter()
                .map(|row| {
                    let id: uuid::Uuid = row.get("id");
                    let rule_id: uuid::Uuid = row.get("rule_id");
                    AlertSummary {
                        id: id.to_string(),
                        rule_id: rule_id.to_string(),
                        name: row.get("rule_name"),
                        severity: row.get("severity"),
                        status: row.get("status"),
                        metric_value: row.get("metric_value"),
                        threshold: row.get("threshold"),
                        fired_at: row.get("triggered_at"),
                        resolved_at: row.get("resolved_at"),
                        acknowledged_at: row.get("acknowledged_at"),
                        acknowledged_by: row.get("acknowledged_by"),
                    }
                })
                .collect();
            Json(ApiResponse::success(alerts))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Alert Rules API
// ============================================================================

/// List alert rules.
pub async fn list_alert_rules(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<Vec<AlertRuleSummary>>> {
    let result = sqlx::query(
        r#"
        SELECT id, name, description, metric_name, condition, threshold, severity, enabled, created_at
        FROM forge_alert_rules
        ORDER BY name
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let rules: Vec<AlertRuleSummary> = rows
                .into_iter()
                .map(|row| {
                    let id: uuid::Uuid = row.get("id");
                    AlertRuleSummary {
                        id: id.to_string(),
                        name: row.get("name"),
                        description: row.get("description"),
                        metric_name: row.get("metric_name"),
                        condition: row.get("condition"),
                        threshold: row.get("threshold"),
                        severity: row.get("severity"),
                        enabled: row.get("enabled"),
                        created_at: row.get("created_at"),
                    }
                })
                .collect();
            Json(ApiResponse::success(rules))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get an alert rule by ID.
pub async fn get_alert_rule(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<AlertRuleSummary>> {
    let rule_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return Json(ApiResponse::error("Invalid rule ID")),
    };

    let result = sqlx::query(
        r#"
        SELECT id, name, description, metric_name, condition, threshold, severity, enabled, created_at
        FROM forge_alert_rules
        WHERE id = $1
        "#,
    )
    .bind(rule_id)
    .fetch_optional(&state.pool)
    .await;

    match result {
        Ok(Some(row)) => {
            let id: uuid::Uuid = row.get("id");
            Json(ApiResponse::success(AlertRuleSummary {
                id: id.to_string(),
                name: row.get("name"),
                description: row.get("description"),
                metric_name: row.get("metric_name"),
                condition: row.get("condition"),
                threshold: row.get("threshold"),
                severity: row.get("severity"),
                enabled: row.get("enabled"),
                created_at: row.get("created_at"),
            }))
        }
        Ok(None) => Json(ApiResponse::error(format!("Rule '{}' not found", id))),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Create an alert rule.
pub async fn create_alert_rule(
    State(state): State<DashboardState>,
    Json(req): Json<CreateAlertRuleRequest>,
) -> (StatusCode, Json<ApiResponse<AlertRuleSummary>>) {
    let id = uuid::Uuid::new_v4();
    let now = Utc::now();
    let severity = req.severity.as_deref().unwrap_or("warning");
    let duration_seconds = req.duration_seconds.unwrap_or(0);
    let cooldown_seconds = req.cooldown_seconds.unwrap_or(300);

    let result = sqlx::query(
        r#"
        INSERT INTO forge_alert_rules
        (id, name, description, metric_name, condition, threshold, duration_seconds, severity,
         enabled, labels, notification_channels, cooldown_seconds, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, '{}', '{}', $9, $10, $10)
        "#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.metric_name)
    .bind(&req.condition)
    .bind(req.threshold)
    .bind(duration_seconds)
    .bind(severity)
    .bind(cooldown_seconds)
    .bind(now)
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(ApiResponse::success(AlertRuleSummary {
                id: id.to_string(),
                name: req.name,
                description: req.description,
                metric_name: req.metric_name,
                condition: req.condition,
                threshold: req.threshold,
                severity: severity.to_string(),
                enabled: true,
                created_at: now,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(e.to_string())),
        ),
    }
}

/// Update an alert rule.
pub async fn update_alert_rule(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAlertRuleRequest>,
) -> Json<ApiResponse<AlertRuleSummary>> {
    let rule_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return Json(ApiResponse::error("Invalid rule ID")),
    };

    // Fetch existing rule first
    let existing = sqlx::query(
        r#"
        SELECT id, name, description, metric_name, condition, threshold, duration_seconds,
               severity, enabled, cooldown_seconds, created_at
        FROM forge_alert_rules
        WHERE id = $1
        "#,
    )
    .bind(rule_id)
    .fetch_optional(&state.pool)
    .await;

    let existing = match existing {
        Ok(Some(row)) => row,
        Ok(None) => return Json(ApiResponse::error(format!("Rule '{}' not found", id))),
        Err(e) => return Json(ApiResponse::error(e.to_string())),
    };

    // Merge with existing values
    let name: String = req.name.unwrap_or_else(|| existing.get("name"));
    let description: Option<String> = req.description.or_else(|| existing.get("description"));
    let metric_name: String = req
        .metric_name
        .unwrap_or_else(|| existing.get("metric_name"));
    let condition: String = req.condition.unwrap_or_else(|| existing.get("condition"));
    let threshold: f64 = req.threshold.unwrap_or_else(|| existing.get("threshold"));
    let duration_seconds: i32 = req
        .duration_seconds
        .unwrap_or_else(|| existing.get("duration_seconds"));
    let severity: String = req.severity.unwrap_or_else(|| existing.get("severity"));
    let enabled: bool = req.enabled.unwrap_or_else(|| existing.get("enabled"));
    let cooldown_seconds: i32 = req
        .cooldown_seconds
        .unwrap_or_else(|| existing.get("cooldown_seconds"));
    let created_at: DateTime<Utc> = existing.get("created_at");

    let result = sqlx::query(
        r#"
        UPDATE forge_alert_rules
        SET name = $2, description = $3, metric_name = $4, condition = $5, threshold = $6,
            duration_seconds = $7, severity = $8, enabled = $9, cooldown_seconds = $10,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(rule_id)
    .bind(&name)
    .bind(&description)
    .bind(&metric_name)
    .bind(&condition)
    .bind(threshold)
    .bind(duration_seconds)
    .bind(&severity)
    .bind(enabled)
    .bind(cooldown_seconds)
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => Json(ApiResponse::success(AlertRuleSummary {
            id: rule_id.to_string(),
            name,
            description,
            metric_name,
            condition,
            threshold,
            severity,
            enabled,
            created_at,
        })),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Delete an alert rule.
pub async fn delete_alert_rule(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let rule_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::error("Invalid rule ID")),
            )
        }
    };

    let result = sqlx::query("DELETE FROM forge_alert_rules WHERE id = $1")
        .bind(rule_id)
        .execute(&state.pool)
        .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => (StatusCode::OK, Json(ApiResponse::success(()))),
        Ok(_) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!("Rule '{}' not found", id))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(e.to_string())),
        ),
    }
}

/// Acknowledge an alert.
pub async fn acknowledge_alert(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    Json(req): Json<AcknowledgeAlertRequest>,
) -> Json<ApiResponse<()>> {
    let alert_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return Json(ApiResponse::error("Invalid alert ID")),
    };

    let result = sqlx::query(
        r#"
        UPDATE forge_alerts
        SET acknowledged_at = NOW(), acknowledged_by = $2
        WHERE id = $1
        "#,
    )
    .bind(alert_id)
    .bind(&req.acknowledged_by)
    .execute(&state.pool)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => Json(ApiResponse::success(())),
        Ok(_) => Json(ApiResponse::error(format!("Alert '{}' not found", id))),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Resolve an alert.
pub async fn resolve_alert(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    let alert_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return Json(ApiResponse::error("Invalid alert ID")),
    };

    let result = sqlx::query(
        r#"
        UPDATE forge_alerts
        SET status = 'resolved', resolved_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(alert_id)
    .execute(&state.pool)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => Json(ApiResponse::success(())),
        Ok(_) => Json(ApiResponse::error(format!("Alert '{}' not found", id))),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Jobs API
// ============================================================================

/// List jobs.
pub async fn list_jobs(
    State(state): State<DashboardState>,
    Query(query): Query<PaginationQuery>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    let limit = query.get_limit();
    let offset = query.get_offset();

    let result = sqlx::query(
        r#"
        SELECT id, job_type, status, priority, attempts, max_attempts,
               progress_percent, progress_message,
               scheduled_at, created_at, started_at, completed_at, last_error
        FROM forge_jobs
        ORDER BY created_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let jobs: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|row| {
                    let id: uuid::Uuid = row.get("id");
                    serde_json::json!({
                        "id": id.to_string(),
                        "job_type": row.get::<String, _>("job_type"),
                        "status": row.get::<String, _>("status"),
                        "priority": row.get::<i32, _>("priority"),
                        "attempts": row.get::<i32, _>("attempts"),
                        "max_attempts": row.get::<i32, _>("max_attempts"),
                        "progress_percent": row.get::<Option<i32>, _>("progress_percent"),
                        "progress_message": row.get::<Option<String>, _>("progress_message"),
                        "scheduled_at": row.get::<DateTime<Utc>, _>("scheduled_at"),
                        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
                        "started_at": row.get::<Option<DateTime<Utc>>, _>("started_at"),
                        "completed_at": row.get::<Option<DateTime<Utc>>, _>("completed_at"),
                        "last_error": row.get::<Option<String>, _>("last_error"),
                    })
                })
                .collect();
            Json(ApiResponse::success(jobs))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get a specific job by ID with full details.
pub async fn get_job(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<JobDetail>> {
    let job_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return Json(ApiResponse::error("Invalid job ID")),
    };

    let result = sqlx::query(
        r#"
        SELECT id, job_type, status, priority, attempts, max_attempts,
               progress_percent, progress_message, input, output,
               scheduled_at, created_at, started_at, completed_at, last_error
        FROM forge_jobs
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .fetch_optional(&state.pool)
    .await;

    match result {
        Ok(Some(row)) => {
            let id: uuid::Uuid = row.get("id");
            Json(ApiResponse::success(JobDetail {
                id: id.to_string(),
                job_type: row.get("job_type"),
                status: row.get("status"),
                priority: row.get("priority"),
                attempts: row.get("attempts"),
                max_attempts: row.get("max_attempts"),
                progress_percent: row.get("progress_percent"),
                progress_message: row.get("progress_message"),
                input: row.get("input"),
                output: row.get("output"),
                scheduled_at: row.get("scheduled_at"),
                created_at: row.get("created_at"),
                started_at: row.get("started_at"),
                completed_at: row.get("completed_at"),
                last_error: row.get("last_error"),
            }))
        }
        Ok(None) => Json(ApiResponse::error(format!("Job '{}' not found", id))),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get job stats.
pub async fn get_job_stats(State(state): State<DashboardState>) -> Json<ApiResponse<JobStats>> {
    let result = sqlx::query(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE status = 'pending') as pending,
            COUNT(*) FILTER (WHERE status = 'running') as running,
            COUNT(*) FILTER (WHERE status = 'completed') as completed,
            COUNT(*) FILTER (WHERE status = 'failed') as failed,
            COUNT(*) FILTER (WHERE status = 'retry') as retrying,
            COUNT(*) FILTER (WHERE status = 'dead_letter') as dead_letter
        FROM forge_jobs
        "#,
    )
    .fetch_one(&state.pool)
    .await;

    match result {
        Ok(row) => Json(ApiResponse::success(JobStats {
            pending: row.get::<Option<i64>, _>("pending").unwrap_or(0) as u64,
            running: row.get::<Option<i64>, _>("running").unwrap_or(0) as u64,
            completed: row.get::<Option<i64>, _>("completed").unwrap_or(0) as u64,
            failed: row.get::<Option<i64>, _>("failed").unwrap_or(0) as u64,
            retrying: row.get::<Option<i64>, _>("retrying").unwrap_or(0) as u64,
            dead_letter: row.get::<Option<i64>, _>("dead_letter").unwrap_or(0) as u64,
        })),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Workflows API
// ============================================================================

/// List workflow runs.
pub async fn list_workflows(
    State(state): State<DashboardState>,
    Query(query): Query<PaginationQuery>,
) -> Json<ApiResponse<Vec<WorkflowRun>>> {
    let limit = query.get_limit();
    let offset = query.get_offset();

    let result = sqlx::query(
        r#"
        SELECT id, workflow_name, version, status, current_step,
               started_at, completed_at, error
        FROM forge_workflow_runs
        ORDER BY started_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let workflows: Vec<WorkflowRun> = rows
                .into_iter()
                .map(|row| {
                    let id: uuid::Uuid = row.get("id");
                    WorkflowRun {
                        id: id.to_string(),
                        workflow_name: row.get("workflow_name"),
                        version: row.get("version"),
                        status: row.get("status"),
                        current_step: row.get("current_step"),
                        started_at: row.get("started_at"),
                        completed_at: row.get("completed_at"),
                        error: row.get("error"),
                    }
                })
                .collect();
            Json(ApiResponse::success(workflows))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get a specific workflow by ID with full details.
pub async fn get_workflow(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<WorkflowDetail>> {
    let workflow_id = match uuid::Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return Json(ApiResponse::error("Invalid workflow ID")),
    };

    // Get the workflow run
    let run_result = sqlx::query(
        r#"
        SELECT id, workflow_name, version, status, input, output,
               current_step, started_at, completed_at, error
        FROM forge_workflow_runs
        WHERE id = $1
        "#,
    )
    .bind(workflow_id)
    .fetch_optional(&state.pool)
    .await;

    let run = match run_result {
        Ok(Some(row)) => row,
        Ok(None) => return Json(ApiResponse::error(format!("Workflow '{}' not found", id))),
        Err(e) => return Json(ApiResponse::error(e.to_string())),
    };

    // Get the workflow steps
    let steps_result = sqlx::query(
        r#"
        SELECT step_name, status, result, started_at, completed_at, error
        FROM forge_workflow_steps
        WHERE workflow_run_id = $1
        ORDER BY started_at ASC NULLS LAST
        "#,
    )
    .bind(workflow_id)
    .fetch_all(&state.pool)
    .await;

    let steps = match steps_result {
        Ok(rows) => rows
            .into_iter()
            .map(|row| WorkflowStepDetail {
                name: row.get("step_name"),
                status: row.get("status"),
                result: row.get("result"),
                started_at: row.get("started_at"),
                completed_at: row.get("completed_at"),
                error: row.get("error"),
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    let run_id: uuid::Uuid = run.get("id");
    Json(ApiResponse::success(WorkflowDetail {
        id: run_id.to_string(),
        workflow_name: run.get("workflow_name"),
        version: run.get("version"),
        status: run.get("status"),
        input: run.get("input"),
        output: run.get("output"),
        current_step: run.get("current_step"),
        steps,
        started_at: run.get("started_at"),
        completed_at: run.get("completed_at"),
        error: run.get("error"),
    }))
}

/// Get workflow stats.
pub async fn get_workflow_stats(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<WorkflowStats>> {
    let result = sqlx::query(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE status = 'running') as running,
            COUNT(*) FILTER (WHERE status = 'completed') as completed,
            COUNT(*) FILTER (WHERE status = 'waiting') as waiting,
            COUNT(*) FILTER (WHERE status = 'failed') as failed,
            COUNT(*) FILTER (WHERE status = 'compensating') as compensating
        FROM forge_workflow_runs
        "#,
    )
    .fetch_one(&state.pool)
    .await;

    match result {
        Ok(row) => Json(ApiResponse::success(WorkflowStats {
            running: row.get::<Option<i64>, _>("running").unwrap_or(0) as u64,
            completed: row.get::<Option<i64>, _>("completed").unwrap_or(0) as u64,
            waiting: row.get::<Option<i64>, _>("waiting").unwrap_or(0) as u64,
            failed: row.get::<Option<i64>, _>("failed").unwrap_or(0) as u64,
            compensating: row.get::<Option<i64>, _>("compensating").unwrap_or(0) as u64,
        })),
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// Cluster API
// ============================================================================

/// List cluster nodes.
pub async fn list_nodes(State(state): State<DashboardState>) -> Json<ApiResponse<Vec<NodeInfo>>> {
    let result = sqlx::query(
        r#"
        SELECT id, hostname, roles, status, last_heartbeat, version, started_at
        FROM forge_nodes
        ORDER BY started_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let nodes: Vec<NodeInfo> = rows
                .into_iter()
                .map(|row| {
                    let id: uuid::Uuid = row.get("id");
                    let roles: Vec<String> = row.get("roles");
                    NodeInfo {
                        id: id.to_string(),
                        name: row.get("hostname"),
                        roles,
                        status: row.get("status"),
                        last_heartbeat: row.get("last_heartbeat"),
                        version: row.get::<Option<String>, _>("version").unwrap_or_default(),
                        started_at: row.get("started_at"),
                    }
                })
                .collect();
            Json(ApiResponse::success(nodes))
        }
        Err(e) => Json(ApiResponse::error(e.to_string())),
    }
}

/// Get cluster health.
pub async fn get_cluster_health(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<ClusterHealth>> {
    // Get node counts
    let nodes_result = sqlx::query(
        r#"
        SELECT
            COUNT(*) as total,
            COUNT(*) FILTER (WHERE status = 'active' AND last_heartbeat > NOW() - INTERVAL '30 seconds') as healthy
        FROM forge_nodes
        "#,
    )
    .fetch_one(&state.pool)
    .await;

    // Get leaders
    let leaders_result = sqlx::query(
        r#"
        SELECT role, node_id
        FROM forge_leaders
        WHERE lease_until > NOW()
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match (nodes_result, leaders_result) {
        (Ok(nodes_row), Ok(leader_rows)) => {
            let total: i64 = nodes_row.get("total");
            let healthy: i64 = nodes_row.get("healthy");

            let mut leaders: HashMap<String, String> = HashMap::new();
            let mut leader_node: Option<String> = None;

            for row in leader_rows {
                let role: String = row.get("role");
                let node_id: uuid::Uuid = row.get("node_id");
                if role == "scheduler" {
                    leader_node = Some(node_id.to_string());
                }
                leaders.insert(role, node_id.to_string());
            }

            let status = if healthy == total && total > 0 {
                "healthy"
            } else if healthy > 0 {
                "degraded"
            } else {
                "unhealthy"
            };

            Json(ApiResponse::success(ClusterHealth {
                status: status.to_string(),
                node_count: total as u32,
                healthy_nodes: healthy as u32,
                leader_node,
                leaders,
            }))
        }
        (Err(e), _) | (_, Err(e)) => Json(ApiResponse::error(e.to_string())),
    }
}

// ============================================================================
// System API
// ============================================================================

/// Get system info.
pub async fn get_system_info(State(state): State<DashboardState>) -> Json<ApiResponse<SystemInfo>> {
    // Query the earliest node start time as proxy for system start
    let started_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        "SELECT MIN(started_at) FROM forge_nodes WHERE status = 'active'",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(Utc::now);

    let uptime_seconds = (Utc::now() - started_at).num_seconds().max(0) as u64;

    Json(ApiResponse::success(SystemInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        rust_version: env!("CARGO_PKG_RUST_VERSION").to_string(),
        started_at,
        uptime_seconds,
    }))
}

/// Get system stats.
pub async fn get_system_stats(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<SystemStats>> {
    // Get job stats
    let jobs_pending =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM forge_jobs WHERE status = 'pending'")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0) as u64;

    // Get active sessions
    let active_sessions = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM forge_sessions WHERE status = 'connected'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0) as u32;

    // Get active subscriptions
    let active_subscriptions =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM forge_subscriptions")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0) as u32;

    // Get HTTP request metrics from forge_metrics table (sum all counter increments)
    let http_requests_total = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM(value), 0) FROM forge_metrics WHERE name = 'http_requests_total'",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0) as u64;

    // Calculate requests per second from recent metrics (last minute)
    let http_requests_per_second = sqlx::query_scalar::<_, f64>(
        r#"
        SELECT COALESCE(SUM(value) / 60.0, 0)
        FROM forge_metrics
        WHERE name = 'http_requests_total'
        AND timestamp > NOW() - INTERVAL '1 minute'
        "#,
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0);

    // Get function calls total
    let function_calls_total = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(value, 0) FROM forge_metrics WHERE name = 'forge_function_calls_total' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0) as u64;

    // Get CPU usage from system metrics
    let cpu_usage_percent = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(value, 0) FROM forge_metrics WHERE name = 'forge_system_cpu_usage_percent' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0);

    // Get memory usage from system metrics
    let memory_used_bytes = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(value, 0) FROM forge_metrics WHERE name = 'forge_system_memory_used_bytes' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0);
    let memory_used_mb = (memory_used_bytes / 1_048_576.0) as u64; // Convert bytes to MB

    // Calculate p99 latency from duration metrics (last hour to match default dashboard range)
    let p99_latency_ms: Option<f64> = sqlx::query_scalar::<_, f64>(
        r#"
        SELECT PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY value) * 1000
        FROM forge_metrics
        WHERE name = 'http_request_duration_seconds'
        AND timestamp > NOW() - INTERVAL '1 hour'
        "#,
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();

    Json(ApiResponse::success(SystemStats {
        http_requests_total,
        http_requests_per_second,
        p99_latency_ms,
        function_calls_total,
        active_connections: active_sessions,
        active_subscriptions,
        jobs_pending,
        memory_used_mb,
        cpu_usage_percent,
    }))
}

// ============================================================================
// Crons API
// ============================================================================

/// Cron job summary.
#[derive(Debug, Clone, Serialize)]
pub struct CronSummary {
    pub name: String,
    pub schedule: String,
    pub status: String,
    pub last_run: Option<DateTime<Utc>>,
    pub last_result: Option<String>,
    pub next_run: Option<DateTime<Utc>>,
    pub avg_duration_ms: Option<f64>,
    pub success_count: i64,
    pub failure_count: i64,
}

/// Cron execution history entry.
#[derive(Debug, Clone, Serialize)]
pub struct CronExecution {
    pub id: String,
    pub cron_name: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub status: String,
    pub error: Option<String>,
}

/// Cron statistics.
#[derive(Debug, Clone, Serialize)]
pub struct CronStats {
    pub active_count: i64,
    pub paused_count: i64,
    pub total_executions_24h: i64,
    pub success_rate_24h: f64,
    pub next_scheduled_run: Option<DateTime<Utc>>,
}

/// List all cron jobs.
pub async fn list_crons(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<Vec<CronSummary>>> {
    // Use forge_cron_runs table to derive cron list and stats
    let result = sqlx::query(
        r#"
        SELECT
            cron_name as name,
            MAX(scheduled_time) as last_run_at,
            MAX(CASE WHEN status = 'completed' THEN 'success' WHEN status = 'failed' THEN 'failed' ELSE status END) as last_result,
            COALESCE(AVG(EXTRACT(EPOCH FROM (completed_at - started_at)) * 1000), 0) as avg_duration_ms,
            COUNT(CASE WHEN status = 'completed' THEN 1 END) as success_count,
            COUNT(CASE WHEN status = 'failed' THEN 1 END) as failure_count
        FROM forge_cron_runs
        GROUP BY cron_name
        ORDER BY cron_name
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let crons: Vec<CronSummary> = rows
                .into_iter()
                .map(|r| CronSummary {
                    name: r.get("name"),
                    schedule: "* * * * *".to_string(), // Default schedule (would need registry for actual)
                    status: "active".to_string(),
                    last_run: r.get("last_run_at"),
                    last_result: r.get("last_result"),
                    next_run: None, // Would need registry to calculate
                    avg_duration_ms: r.try_get::<f64, _>("avg_duration_ms").ok(),
                    success_count: r.try_get::<i64, _>("success_count").unwrap_or(0),
                    failure_count: r.try_get::<i64, _>("failure_count").unwrap_or(0),
                })
                .collect();
            Json(ApiResponse::success(crons))
        }
        Err(_) => {
            // Table may not exist yet
            Json(ApiResponse::success(vec![]))
        }
    }
}

/// Get cron statistics.
pub async fn get_cron_stats(State(state): State<DashboardState>) -> Json<ApiResponse<CronStats>> {
    // Get stats from forge_cron_runs
    let stats = sqlx::query(
        r#"
        SELECT
            COUNT(DISTINCT cron_name) as active_count,
            0 as paused_count
        FROM forge_cron_runs
        "#,
    )
    .fetch_optional(&state.pool)
    .await;

    let execution_stats = sqlx::query(
        r#"
        SELECT
            COUNT(*) as total,
            COUNT(CASE WHEN status = 'completed' THEN 1 END) as success
        FROM forge_cron_runs
        WHERE started_at > NOW() - INTERVAL '24 hours'
        "#,
    )
    .fetch_optional(&state.pool)
    .await;

    match (stats, execution_stats) {
        (Ok(Some(s)), Ok(Some(e))) => {
            let total = e.try_get::<i64, _>("total").unwrap_or(0) as f64;
            let success = e.try_get::<i64, _>("success").unwrap_or(0) as f64;
            let success_rate = if total > 0.0 {
                success / total * 100.0
            } else {
                100.0
            };

            Json(ApiResponse::success(CronStats {
                active_count: s.try_get::<i64, _>("active_count").unwrap_or(0),
                paused_count: s.try_get::<i64, _>("paused_count").unwrap_or(0),
                total_executions_24h: e.try_get::<i64, _>("total").unwrap_or(0),
                success_rate_24h: success_rate,
                next_scheduled_run: None, // Would need registry to calculate
            }))
        }
        _ => Json(ApiResponse::success(CronStats {
            active_count: 0,
            paused_count: 0,
            total_executions_24h: 0,
            success_rate_24h: 100.0,
            next_scheduled_run: None,
        })),
    }
}

/// Get cron execution history.
pub async fn get_cron_history(
    State(state): State<DashboardState>,
    Query(pagination): Query<PaginationQuery>,
) -> Json<ApiResponse<Vec<CronExecution>>> {
    let limit = pagination.get_limit();
    let offset = pagination.get_offset();

    // Use forge_cron_runs table instead of forge_cron_history
    let result = sqlx::query(
        r#"
        SELECT
            id::text as id,
            cron_name,
            started_at,
            completed_at as finished_at,
            EXTRACT(EPOCH FROM (completed_at - started_at)) * 1000 as duration_ms,
            CASE WHEN status = 'completed' THEN 'success' ELSE status END as status,
            error
        FROM forge_cron_runs
        ORDER BY started_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await;

    match result {
        Ok(rows) => {
            let executions: Vec<CronExecution> = rows
                .into_iter()
                .map(|r| CronExecution {
                    id: r.try_get::<String, _>("id").unwrap_or_default(),
                    cron_name: r.get("cron_name"),
                    started_at: r.get("started_at"),
                    finished_at: r.try_get("finished_at").ok(),
                    duration_ms: r.try_get::<f64, _>("duration_ms").ok().map(|d| d as i64),
                    status: r.get("status"),
                    error: r.try_get("error").ok(),
                })
                .collect();
            Json(ApiResponse::success(executions))
        }
        Err(_) => Json(ApiResponse::success(vec![])),
    }
}

/// Manually trigger a cron job.
pub async fn trigger_cron(
    State(_state): State<DashboardState>,
    Path(name): Path<String>,
) -> Json<ApiResponse<()>> {
    // In a real implementation, this would dispatch the cron job immediately
    tracing::info!(cron = %name, "Manual cron trigger requested");
    Json(ApiResponse::success(()))
}

/// Pause a cron job.
pub async fn pause_cron(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> Json<ApiResponse<()>> {
    let result = sqlx::query("UPDATE forge_crons SET status = 'paused' WHERE name = $1")
        .bind(&name)
        .execute(&state.pool)
        .await;

    match result {
        Ok(_) => {
            tracing::info!(cron = %name, "Cron paused");
            Json(ApiResponse::success(()))
        }
        Err(e) => Json(ApiResponse::error(format!("Failed to pause cron: {}", e))),
    }
}

/// Resume a paused cron job.
pub async fn resume_cron(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> Json<ApiResponse<()>> {
    let result = sqlx::query("UPDATE forge_crons SET status = 'active' WHERE name = $1")
        .bind(&name)
        .execute(&state.pool)
        .await;

    match result {
        Ok(_) => {
            tracing::info!(cron = %name, "Cron resumed");
            Json(ApiResponse::success(()))
        }
        Err(e) => Json(ApiResponse::error(format!("Failed to resume cron: {}", e))),
    }
}

// ============================================================================
// Registered Types API
// ============================================================================

/// Registered job type info.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredJob {
    pub name: String,
    pub max_attempts: u32,
    pub priority: String,
    pub timeout_secs: u64,
    pub worker_capability: Option<String>,
}

/// Registered cron type info.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredCron {
    pub name: String,
    pub schedule: String,
    pub timezone: String,
    pub catch_up: bool,
    pub timeout_secs: u64,
}

/// Registered workflow type info.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredWorkflow {
    pub name: String,
    pub version: u32,
    pub timeout_secs: u64,
    pub deprecated: bool,
}

/// List registered job types from the registry.
pub async fn list_registered_jobs(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<Vec<RegisteredJob>>> {
    let jobs: Vec<RegisteredJob> = state
        .job_registry
        .jobs()
        .map(|(_, entry)| RegisteredJob {
            name: entry.info.name.to_string(),
            max_attempts: entry.info.retry.max_attempts,
            priority: format!("{:?}", entry.info.priority),
            timeout_secs: entry.info.timeout.as_secs(),
            worker_capability: entry.info.worker_capability.map(|s| s.to_string()),
        })
        .collect();
    Json(ApiResponse::success(jobs))
}

/// List registered cron types from the registry.
pub async fn list_registered_crons(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<Vec<RegisteredCron>>> {
    let crons: Vec<RegisteredCron> = state
        .cron_registry
        .list()
        .into_iter()
        .map(|entry| RegisteredCron {
            name: entry.info.name.to_string(),
            schedule: entry.info.schedule.expression().to_string(),
            timezone: entry.info.timezone.to_string(),
            catch_up: entry.info.catch_up,
            timeout_secs: entry.info.timeout.as_secs(),
        })
        .collect();
    Json(ApiResponse::success(crons))
}

/// List registered workflow types from the registry.
pub async fn list_registered_workflows(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<Vec<RegisteredWorkflow>>> {
    let workflows: Vec<RegisteredWorkflow> = state
        .workflow_registry
        .list()
        .into_iter()
        .map(|entry| RegisteredWorkflow {
            name: entry.info.name.to_string(),
            version: entry.info.version,
            timeout_secs: entry.info.timeout.as_secs(),
            deprecated: entry.info.deprecated,
        })
        .collect();
    Json(ApiResponse::success(workflows))
}

// ============== Job/Workflow Dispatch ==============

/// Request body for dispatching a job.
#[derive(Debug, Deserialize)]
pub struct DispatchJobRequest {
    /// Arguments for the job (JSON).
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Response body for dispatching a job.
#[derive(Debug, Serialize)]
pub struct DispatchJobResponse {
    /// The ID of the dispatched job.
    pub job_id: uuid::Uuid,
}

/// Request body for starting a workflow.
#[derive(Debug, Deserialize)]
pub struct StartWorkflowRequest {
    /// Input for the workflow (JSON).
    #[serde(default)]
    pub input: serde_json::Value,
}

/// Response body for starting a workflow.
#[derive(Debug, Serialize)]
pub struct StartWorkflowResponse {
    /// The ID of the started workflow run.
    pub workflow_id: uuid::Uuid,
}

/// Dispatch a job by type.
pub async fn dispatch_job(
    State(state): State<DashboardState>,
    Path(job_type): Path<String>,
    Json(request): Json<DispatchJobRequest>,
) -> (StatusCode, Json<ApiResponse<DispatchJobResponse>>) {
    let dispatcher = match &state.job_dispatcher {
        Some(d) => d,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse::error("Job dispatcher not available")),
            );
        }
    };

    match dispatcher.dispatch_by_name(&job_type, request.args).await {
        Ok(job_id) => (
            StatusCode::OK,
            Json(ApiResponse::success(DispatchJobResponse { job_id })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error(e.to_string())),
        ),
    }
}

/// Start a workflow by name.
pub async fn start_workflow(
    State(state): State<DashboardState>,
    Path(workflow_name): Path<String>,
    Json(request): Json<StartWorkflowRequest>,
) -> (StatusCode, Json<ApiResponse<StartWorkflowResponse>>) {
    let executor = match &state.workflow_executor {
        Some(e) => e,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse::error("Workflow executor not available")),
            );
        }
    };

    match executor.start_by_name(&workflow_name, request.input).await {
        Ok(workflow_id) => (
            StatusCode::OK,
            Json(ApiResponse::success(StartWorkflowResponse { workflow_id })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error(e.to_string())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_response_success() {
        let response: ApiResponse<String> = ApiResponse::success("test".to_string());
        assert!(response.success);
        assert_eq!(response.data, Some("test".to_string()));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_api_response_error() {
        let response: ApiResponse<String> = ApiResponse::error("failed");
        assert!(!response.success);
        assert!(response.data.is_none());
        assert_eq!(response.error, Some("failed".to_string()));
    }

    #[test]
    fn test_time_range_query_defaults() {
        let query = TimeRangeQuery {
            start: None,
            end: None,
            period: None,
        };
        let (start, end) = query.get_range();
        assert!(end > start);
        assert!((end - start).num_hours() == 1);
    }

    #[test]
    fn test_time_range_query_period() {
        let query = TimeRangeQuery {
            start: None,
            end: None,
            period: Some("24h".to_string()),
        };
        let (start, end) = query.get_range();
        assert!((end - start).num_hours() == 24);
    }

    #[test]
    fn test_pagination_query() {
        let query = PaginationQuery {
            page: Some(2),
            limit: Some(20),
        };
        assert_eq!(query.get_limit(), 20);
        assert_eq!(query.get_offset(), 20);
    }
}

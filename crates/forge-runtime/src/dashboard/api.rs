use axum::{
    extract::{Path, Query, State},
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
    pub name: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub fired_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
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
pub async fn get_metric_series(
    State(state): State<DashboardState>,
    Query(query): Query<TimeRangeQuery>,
) -> Json<ApiResponse<Vec<MetricSeries>>> {
    let (start, end) = query.get_range();

    let result = sqlx::query(
        r#"
        SELECT name, labels, value, timestamp
        FROM forge_metrics
        WHERE timestamp >= $1 AND timestamp <= $2
        ORDER BY name, timestamp ASC
        "#,
    )
    .bind(start)
    .bind(end)
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
    State(_state): State<DashboardState>,
    Query(_query): Query<PaginationQuery>,
) -> Json<ApiResponse<Vec<AlertSummary>>> {
    // Alerts are not yet persisted - return empty for now
    Json(ApiResponse::success(vec![]))
}

/// Get active alerts.
pub async fn get_active_alerts(
    State(_state): State<DashboardState>,
) -> Json<ApiResponse<Vec<AlertSummary>>> {
    Json(ApiResponse::success(vec![]))
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

    // Get HTTP request metrics from forge_metrics table
    let http_requests_total = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(value, 0) FROM forge_metrics WHERE name = 'forge_http_requests_total' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0) as u64;

    // Calculate requests per second from recent metrics (last minute)
    let http_requests_per_second = sqlx::query_scalar::<_, f64>(
        r#"
        SELECT COALESCE(
            (MAX(value) - MIN(value)) / NULLIF(EXTRACT(EPOCH FROM (MAX(timestamp) - MIN(timestamp))), 0),
            0
        )
        FROM forge_metrics
        WHERE name = 'forge_http_requests_total'
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

    Json(ApiResponse::success(SystemStats {
        http_requests_total,
        http_requests_per_second,
        function_calls_total,
        active_connections: active_sessions,
        active_subscriptions,
        jobs_pending,
        memory_used_mb: 0,      // Not tracked in DB - would need OS metrics
        cpu_usage_percent: 0.0, // Not tracked in DB - would need OS metrics
    }))
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

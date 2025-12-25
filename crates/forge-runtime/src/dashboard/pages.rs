use axum::{
    extract::{Path, State},
    response::Html,
};

use super::DashboardState;

/// Dashboard page handlers.
pub struct DashboardPages;

/// Base HTML template.
fn base_template(title: &str, content: &str, active_page: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{title} - FORGE Dashboard</title>
    <link rel="stylesheet" href="/_dashboard/assets/styles.css">
    <script src="/_dashboard/assets/chart.js" defer></script>
    <script src="/_dashboard/assets/main.js" defer></script>
</head>
<body>
    <div class="dashboard">
        <nav class="sidebar">
            <div class="sidebar-header">
                <h1>⚒️ FORGE</h1>
                <span class="version">v{version}</span>
            </div>
            <ul class="nav-links">
                <li><a href="/_dashboard" class="{overview_active}">📊 Overview</a></li>
                <li><a href="/_dashboard/metrics" class="{metrics_active}">📈 Metrics</a></li>
                <li><a href="/_dashboard/logs" class="{logs_active}">📝 Logs</a></li>
                <li><a href="/_dashboard/traces" class="{traces_active}">🔍 Traces</a></li>
                <li><a href="/_dashboard/alerts" class="{alerts_active}">🚨 Alerts</a></li>
                <li><a href="/_dashboard/jobs" class="{jobs_active}">⚙️ Jobs</a></li>
                <li><a href="/_dashboard/workflows" class="{workflows_active}">🔄 Workflows</a></li>
                <li><a href="/_dashboard/cluster" class="{cluster_active}">🖥️ Cluster</a></li>
            </ul>
            <div class="sidebar-footer">
                <a href="https://github.com/example/forge" target="_blank">Documentation</a>
            </div>
        </nav>
        <main class="content">
            <header class="content-header">
                <h2>{title}</h2>
                <div class="header-actions">
                    <select id="time-range" class="time-range-select">
                        <option value="5m">Last 5 minutes</option>
                        <option value="15m">Last 15 minutes</option>
                        <option value="1h" selected>Last hour</option>
                        <option value="6h">Last 6 hours</option>
                        <option value="24h">Last 24 hours</option>
                        <option value="7d">Last 7 days</option>
                    </select>
                    <button id="refresh-btn" class="btn btn-secondary">↻ Refresh</button>
                </div>
            </header>
            <div class="content-body">
                {content}
            </div>
        </main>
    </div>
</body>
</html>"#,
        title = title,
        content = content,
        version = env!("CARGO_PKG_VERSION"),
        overview_active = if active_page == "overview" {
            "active"
        } else {
            ""
        },
        metrics_active = if active_page == "metrics" {
            "active"
        } else {
            ""
        },
        logs_active = if active_page == "logs" { "active" } else { "" },
        traces_active = if active_page == "traces" {
            "active"
        } else {
            ""
        },
        alerts_active = if active_page == "alerts" {
            "active"
        } else {
            ""
        },
        jobs_active = if active_page == "jobs" { "active" } else { "" },
        workflows_active = if active_page == "workflows" {
            "active"
        } else {
            ""
        },
        cluster_active = if active_page == "cluster" {
            "active"
        } else {
            ""
        },
    )
}

/// Overview/index page.
pub async fn index(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="stats-grid">
            <div class="stat-card">
                <div class="stat-icon">📊</div>
                <div class="stat-content">
                    <h3>Requests</h3>
                    <p class="stat-value" id="stat-requests">-</p>
                    <p class="stat-label">per second</p>
                </div>
            </div>
            <div class="stat-card">
                <div class="stat-icon">⚡</div>
                <div class="stat-content">
                    <h3>Latency (p99)</h3>
                    <p class="stat-value" id="stat-latency">-</p>
                    <p class="stat-label">milliseconds</p>
                </div>
            </div>
            <div class="stat-card">
                <div class="stat-icon">❌</div>
                <div class="stat-content">
                    <h3>Error Rate</h3>
                    <p class="stat-value" id="stat-errors">-</p>
                    <p class="stat-label">percent</p>
                </div>
            </div>
            <div class="stat-card">
                <div class="stat-icon">🔌</div>
                <div class="stat-content">
                    <h3>Connections</h3>
                    <p class="stat-value" id="stat-connections">-</p>
                    <p class="stat-label">active</p>
                </div>
            </div>
        </div>

        <div class="charts-row">
            <div class="chart-container">
                <h3>Request Rate</h3>
                <canvas id="requests-chart"></canvas>
            </div>
            <div class="chart-container">
                <h3>Response Times</h3>
                <canvas id="latency-chart"></canvas>
            </div>
        </div>

        <div class="panels-row">
            <div class="panel">
                <h3>🚨 Active Alerts</h3>
                <div id="active-alerts" class="alert-list">
                    <p class="empty-state">No active alerts</p>
                </div>
            </div>
            <div class="panel">
                <h3>📝 Recent Logs</h3>
                <div id="recent-logs" class="log-list">
                    <p class="empty-state">Loading...</p>
                </div>
            </div>
        </div>

        <div class="panel">
            <h3>🖥️ Cluster Nodes</h3>
            <div id="cluster-nodes" class="nodes-grid">
                <p class="empty-state">Loading...</p>
            </div>
        </div>
    "#;

    Html(base_template("Overview", content, "overview"))
}

/// Metrics page.
pub async fn metrics(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="metrics-controls">
            <input type="text" id="metric-search" placeholder="Search metrics..." class="search-input">
            <select id="metric-type" class="select-input">
                <option value="">All Types</option>
                <option value="counter">Counters</option>
                <option value="gauge">Gauges</option>
                <option value="histogram">Histograms</option>
            </select>
        </div>

        <div class="metrics-grid" id="metrics-list">
            <div class="metric-card">
                <h4>forge_http_requests_total</h4>
                <p class="metric-value">12,345</p>
                <p class="metric-type">counter</p>
                <canvas class="metric-sparkline"></canvas>
            </div>
            <div class="metric-card">
                <h4>forge_http_request_duration_seconds</h4>
                <p class="metric-value">0.045s</p>
                <p class="metric-type">histogram</p>
                <canvas class="metric-sparkline"></canvas>
            </div>
            <div class="metric-card">
                <h4>forge_function_calls_total</h4>
                <p class="metric-value">5,678</p>
                <p class="metric-type">counter</p>
                <canvas class="metric-sparkline"></canvas>
            </div>
            <div class="metric-card">
                <h4>forge_websocket_connections</h4>
                <p class="metric-value">42</p>
                <p class="metric-type">gauge</p>
                <canvas class="metric-sparkline"></canvas>
            </div>
        </div>

        <div class="chart-container full-width">
            <h3>Selected Metric</h3>
            <canvas id="metric-detail-chart"></canvas>
        </div>
    "#;

    Html(base_template("Metrics", content, "metrics"))
}

/// Logs page.
pub async fn logs(State(_state): State<DashboardState>) -> Html<String> {
    let content = r##"
        <div class="logs-controls">
            <input type="text" id="log-search" placeholder="Search logs..." class="search-input">
            <select id="log-level" class="select-input">
                <option value="">All Levels</option>
                <option value="error">Error</option>
                <option value="warn">Warning</option>
                <option value="info">Info</option>
                <option value="debug">Debug</option>
            </select>
            <button id="log-stream-toggle" class="btn btn-primary">▶ Live Stream</button>
        </div>

        <div class="logs-table-container">
            <table class="logs-table" id="logs-table">
                <thead>
                    <tr>
                        <th>Time</th>
                        <th>Level</th>
                        <th>Message</th>
                        <th>Trace</th>
                    </tr>
                </thead>
                <tbody>
                    <tr class="log-row log-info">
                        <td class="log-time">12:34:56.789</td>
                        <td class="log-level"><span class="level-badge level-info">INFO</span></td>
                        <td class="log-message">Request completed successfully</td>
                        <td class="log-trace"><a href="#">abc123</a></td>
                    </tr>
                </tbody>
            </table>
        </div>

        <div class="logs-pagination">
            <button class="btn btn-secondary" id="logs-prev">← Previous</button>
            <span id="logs-page-info">Page 1 of 10</span>
            <button class="btn btn-secondary" id="logs-next">Next →</button>
        </div>
    "##;

    Html(base_template("Logs", content, "logs"))
}

/// Traces page.
pub async fn traces(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="traces-controls">
            <input type="text" id="trace-search" placeholder="Search by trace ID, service, or operation..." class="search-input">
            <input type="number" id="min-duration" placeholder="Min duration (ms)" class="number-input">
            <label class="checkbox-label">
                <input type="checkbox" id="errors-only"> Errors only
            </label>
        </div>

        <div class="traces-table-container">
            <table class="traces-table" id="traces-table">
                <thead>
                    <tr>
                        <th>Trace ID</th>
                        <th>Root Span</th>
                        <th>Service</th>
                        <th>Duration</th>
                        <th>Spans</th>
                        <th>Status</th>
                        <th>Started</th>
                    </tr>
                </thead>
                <tbody>
                    <tr class="trace-row">
                        <td class="trace-id"><a href="/_dashboard/traces/abc123">abc123</a></td>
                        <td class="trace-name">HTTP GET /api/projects</td>
                        <td class="trace-service">forge-app</td>
                        <td class="trace-duration">45ms</td>
                        <td class="trace-spans">5</td>
                        <td class="trace-status"><span class="status-badge status-ok">OK</span></td>
                        <td class="trace-time">12:34:56</td>
                    </tr>
                </tbody>
            </table>
        </div>
    "#;

    Html(base_template("Traces", content, "traces"))
}

/// Trace detail page.
pub async fn trace_detail(
    State(_state): State<DashboardState>,
    Path(trace_id): Path<String>,
) -> Html<String> {
    let content = format!(
        r#"
        <div class="trace-header">
            <div class="trace-info">
                <h3>Trace: {}</h3>
                <p>Started: 12:34:56.789 | Duration: 45ms | 5 spans</p>
            </div>
            <button class="btn btn-secondary" onclick="history.back()">← Back to Traces</button>
        </div>

        <div class="trace-timeline" id="trace-timeline">
            <div class="timeline-header">
                <span>Service</span>
                <span>Operation</span>
                <span class="timeline-bar-header">Duration</span>
            </div>
            <div class="timeline-row root">
                <span class="service">forge-app</span>
                <span class="operation">HTTP GET /api/projects</span>
                <div class="timeline-bar" style="width: 100%; left: 0%;">45ms</div>
            </div>
            <div class="timeline-row child" style="margin-left: 20px;">
                <span class="service">forge-app</span>
                <span class="operation">authenticate</span>
                <div class="timeline-bar" style="width: 10%; left: 0%;">5ms</div>
            </div>
            <div class="timeline-row child" style="margin-left: 20px;">
                <span class="service">forge-app</span>
                <span class="operation">get_projects</span>
                <div class="timeline-bar" style="width: 80%; left: 12%;">35ms</div>
            </div>
            <div class="timeline-row child" style="margin-left: 40px;">
                <span class="service">postgres</span>
                <span class="operation">SELECT * FROM projects</span>
                <div class="timeline-bar" style="width: 70%; left: 15%;">30ms</div>
            </div>
        </div>

        <div class="span-details" id="span-details">
            <h4>Span Details</h4>
            <p class="empty-state">Click on a span to view details</p>
        </div>
    "#,
        trace_id
    );

    Html(base_template("Trace Detail", &content, "traces"))
}

/// Alerts page.
pub async fn alerts(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="alerts-summary">
            <div class="alert-stat critical">
                <span class="count">0</span>
                <span class="label">Critical</span>
            </div>
            <div class="alert-stat warning">
                <span class="count">0</span>
                <span class="label">Warning</span>
            </div>
            <div class="alert-stat info">
                <span class="count">0</span>
                <span class="label">Info</span>
            </div>
        </div>

        <div class="tabs">
            <button class="tab active" data-tab="active">Active Alerts</button>
            <button class="tab" data-tab="history">Alert History</button>
            <button class="tab" data-tab="rules">Alert Rules</button>
        </div>

        <div class="tab-content" id="alerts-content">
            <div class="empty-state">
                <p>🎉 No active alerts</p>
                <p class="subtitle">Your system is running smoothly</p>
            </div>
        </div>
    "#;

    Html(base_template("Alerts", content, "alerts"))
}

/// Jobs page.
pub async fn jobs(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="jobs-stats">
            <div class="job-stat">
                <span class="count" id="jobs-pending">0</span>
                <span class="label">Pending</span>
            </div>
            <div class="job-stat">
                <span class="count" id="jobs-running">0</span>
                <span class="label">Running</span>
            </div>
            <div class="job-stat">
                <span class="count" id="jobs-completed">0</span>
                <span class="label">Completed</span>
            </div>
            <div class="job-stat error">
                <span class="count" id="jobs-failed">0</span>
                <span class="label">Failed</span>
            </div>
        </div>

        <div class="tabs">
            <button class="tab active" data-tab="queue">Queue</button>
            <button class="tab" data-tab="running">Running</button>
            <button class="tab" data-tab="history">History</button>
            <button class="tab" data-tab="dead-letter">Dead Letter</button>
        </div>

        <div class="jobs-table-container" id="jobs-content">
            <table class="jobs-table">
                <thead>
                    <tr>
                        <th>Job ID</th>
                        <th>Type</th>
                        <th>Priority</th>
                        <th>Status</th>
                        <th>Attempts</th>
                        <th>Created</th>
                        <th>Actions</th>
                    </tr>
                </thead>
                <tbody>
                    <tr class="empty-row">
                        <td colspan="7">No jobs in queue</td>
                    </tr>
                </tbody>
            </table>
        </div>
    "#;

    Html(base_template("Jobs", content, "jobs"))
}

/// Workflows page.
pub async fn workflows(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="workflows-stats">
            <div class="workflow-stat">
                <span class="count">0</span>
                <span class="label">Running</span>
            </div>
            <div class="workflow-stat">
                <span class="count">0</span>
                <span class="label">Completed</span>
            </div>
            <div class="workflow-stat">
                <span class="count">0</span>
                <span class="label">Waiting</span>
            </div>
            <div class="workflow-stat error">
                <span class="count">0</span>
                <span class="label">Failed</span>
            </div>
        </div>

        <div class="workflows-table-container">
            <table class="workflows-table">
                <thead>
                    <tr>
                        <th>Run ID</th>
                        <th>Workflow</th>
                        <th>Version</th>
                        <th>Status</th>
                        <th>Steps</th>
                        <th>Started</th>
                        <th>Duration</th>
                    </tr>
                </thead>
                <tbody>
                    <tr class="empty-row">
                        <td colspan="7">No workflow runs</td>
                    </tr>
                </tbody>
            </table>
        </div>
    "#;

    Html(base_template("Workflows", content, "workflows"))
}

/// Cluster page.
pub async fn cluster(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="cluster-health">
            <div class="health-indicator healthy">
                <span class="health-icon">✓</span>
                <span class="health-text">Cluster Healthy</span>
            </div>
            <div class="cluster-info">
                <span>1 Node</span>
                <span>|</span>
                <span>Leader: node-1</span>
            </div>
        </div>

        <div class="nodes-grid" id="nodes-grid">
            <div class="node-card leader">
                <div class="node-header">
                    <span class="node-status online"></span>
                    <h4>node-1</h4>
                    <span class="leader-badge">Leader</span>
                </div>
                <div class="node-details">
                    <p><strong>Roles:</strong> Gateway, Function, Worker, Scheduler</p>
                    <p><strong>Version:</strong> 0.1.0</p>
                    <p><strong>Started:</strong> 12:00:00</p>
                    <p><strong>Last Heartbeat:</strong> Just now</p>
                </div>
                <div class="node-metrics">
                    <div class="node-metric">
                        <span class="metric-label">CPU</span>
                        <div class="metric-bar"><div class="metric-fill" style="width: 25%"></div></div>
                        <span class="metric-value">25%</span>
                    </div>
                    <div class="node-metric">
                        <span class="metric-label">Memory</span>
                        <div class="metric-bar"><div class="metric-fill" style="width: 45%"></div></div>
                        <span class="metric-value">45%</span>
                    </div>
                </div>
            </div>
        </div>

        <div class="panel">
            <h3>Leadership</h3>
            <table class="leadership-table">
                <thead>
                    <tr>
                        <th>Role</th>
                        <th>Leader Node</th>
                        <th>Since</th>
                    </tr>
                </thead>
                <tbody>
                    <tr>
                        <td>Scheduler</td>
                        <td>node-1</td>
                        <td>12:00:00</td>
                    </tr>
                    <tr>
                        <td>Metrics Aggregator</td>
                        <td>node-1</td>
                        <td>12:00:00</td>
                    </tr>
                </tbody>
            </table>
        </div>
    "#;

    Html(base_template("Cluster", content, "cluster"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_template() {
        let html = base_template("Test", "<p>Content</p>", "overview");
        assert!(html.contains("Test - FORGE Dashboard"));
        assert!(html.contains("<p>Content</p>"));
        assert!(html.contains("class=\"active\""));
    }
}

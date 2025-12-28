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
                <li><a href="/_dashboard/crons" class="{crons_active}">⏰ Crons</a></li>
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
        crons_active = if active_page == "crons" { "active" } else { "" },
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
            <p class="empty-state">Loading metrics...</p>
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
                <tbody id="logs-tbody">
                    <tr class="empty-row">
                        <td colspan="4">Loading logs...</td>
                    </tr>
                </tbody>
            </table>
        </div>

        <div class="logs-pagination">
            <button class="btn btn-secondary" id="logs-prev">← Previous</button>
            <span id="logs-page-info">Page 1</span>
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
                <tbody id="traces-tbody">
                    <tr class="empty-row">
                        <td colspan="7">Loading traces...</td>
                    </tr>
                </tbody>
            </table>
        </div>
    "#;

    Html(base_template("Traces", content, "traces"))
}

/// Trace detail page with waterfall visualization.
pub async fn trace_detail(
    State(_state): State<DashboardState>,
    Path(trace_id): Path<String>,
) -> Html<String> {
    let content = format!(
        r##"
        <div class="trace-header">
            <div class="trace-info">
                <h3>Trace: <code id="trace-id-display">{trace_id}</code></h3>
                <div id="trace-summary" class="trace-summary">
                    <span class="summary-item">Loading...</span>
                </div>
            </div>
            <div class="trace-actions">
                <button class="btn btn-secondary" onclick="copyTraceId()">📋 Copy ID</button>
                <button class="btn btn-secondary" onclick="history.back()">← Back</button>
            </div>
        </div>

        <div class="trace-waterfall-container">
            <div class="waterfall-header">
                <div class="waterfall-labels">
                    <span class="label-service">Service / Operation</span>
                </div>
                <div class="waterfall-timeline">
                    <div class="timeline-ruler" id="timeline-ruler"></div>
                </div>
            </div>
            <div class="waterfall-body" id="waterfall-body">
                <p class="empty-state">Loading spans...</p>
            </div>
        </div>

        <div class="trace-details-panel">
            <div class="panel span-list-panel">
                <h4>Span Tree</h4>
                <div id="span-tree" class="span-tree">
                    <p class="empty-state">Loading...</p>
                </div>
            </div>
            <div class="panel span-details-panel" id="span-details">
                <h4>Span Details</h4>
                <p class="empty-state">Select a span to view details</p>
            </div>
        </div>

        <div class="panel">
            <h4>Span Attributes</h4>
            <div class="tabs">
                <button class="tab active" data-tab="attributes">Attributes</button>
                <button class="tab" data-tab="events">Events</button>
                <button class="tab" data-tab="logs">Logs</button>
            </div>
            <div class="tab-content" id="span-attributes-content">
                <p class="empty-state">Select a span to view attributes</p>
            </div>
        </div>

        <script>
            const traceId = '{trace_id}';

            function copyTraceId() {{
                navigator.clipboard.writeText(traceId);
                showToast('Trace ID copied!');
            }}

            function showToast(message) {{
                const toast = document.createElement('div');
                toast.className = 'toast';
                toast.textContent = message;
                document.body.appendChild(toast);
                setTimeout(() => toast.remove(), 2000);
            }}

            document.addEventListener('DOMContentLoaded', function() {{
                loadTraceDetail(traceId);
            }});
        </script>
    "##,
        trace_id = trace_id
    );

    Html(base_template("Trace Detail", &content, "traces"))
}

/// Alerts page.
pub async fn alerts(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="alerts-summary">
            <div class="alert-stat critical">
                <span class="count" id="alerts-critical">-</span>
                <span class="label">Critical</span>
            </div>
            <div class="alert-stat warning">
                <span class="count" id="alerts-warning">-</span>
                <span class="label">Warning</span>
            </div>
            <div class="alert-stat info">
                <span class="count" id="alerts-info">-</span>
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
                <p>Alerts not yet configured</p>
                <p class="subtitle">Configure alert rules in forge.toml to enable alerting</p>
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
                <span class="count" id="jobs-pending">-</span>
                <span class="label">Pending</span>
            </div>
            <div class="job-stat">
                <span class="count" id="jobs-running">-</span>
                <span class="label">Running</span>
            </div>
            <div class="job-stat">
                <span class="count" id="jobs-completed">-</span>
                <span class="label">Completed</span>
            </div>
            <div class="job-stat error">
                <span class="count" id="jobs-failed">-</span>
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
                        <th>Progress</th>
                        <th>Attempts</th>
                        <th>Created</th>
                        <th>Error</th>
                    </tr>
                </thead>
                <tbody id="jobs-tbody">
                    <tr class="empty-row">
                        <td colspan="8">Loading jobs...</td>
                    </tr>
                </tbody>
            </table>
        </div>

        <!-- Job Detail Modal -->
        <div id="job-modal" class="modal" style="display:none;">
            <div class="modal-content">
                <div class="modal-header">
                    <h3>Job Details</h3>
                    <button class="modal-close" onclick="closeJobModal()">&times;</button>
                </div>
                <div class="modal-body" id="job-modal-body">
                    Loading...
                </div>
            </div>
        </div>
    "#;

    Html(base_template("Jobs", content, "jobs"))
}

/// Workflows page.
pub async fn workflows(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="workflows-stats">
            <div class="workflow-stat">
                <span class="count" id="workflows-running">-</span>
                <span class="label">Running</span>
            </div>
            <div class="workflow-stat">
                <span class="count" id="workflows-completed">-</span>
                <span class="label">Completed</span>
            </div>
            <div class="workflow-stat">
                <span class="count" id="workflows-waiting">-</span>
                <span class="label">Waiting</span>
            </div>
            <div class="workflow-stat error">
                <span class="count" id="workflows-failed">-</span>
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
                        <th>Current Step</th>
                        <th>Started</th>
                        <th>Error</th>
                    </tr>
                </thead>
                <tbody id="workflows-tbody">
                    <tr class="empty-row">
                        <td colspan="7">Loading workflows...</td>
                    </tr>
                </tbody>
            </table>
        </div>

        <!-- Workflow Detail Modal -->
        <div id="workflow-modal" class="modal" style="display:none;">
            <div class="modal-content">
                <div class="modal-header">
                    <h3>Workflow Details</h3>
                    <button class="modal-close" onclick="closeWorkflowModal()">&times;</button>
                </div>
                <div class="modal-body" id="workflow-modal-body">
                    Loading...
                </div>
            </div>
        </div>
    "#;

    Html(base_template("Workflows", content, "workflows"))
}

/// Crons page.
pub async fn crons(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="crons-stats">
            <div class="cron-stat">
                <span class="count" id="crons-active">-</span>
                <span class="label">Active</span>
            </div>
            <div class="cron-stat">
                <span class="count" id="crons-paused">-</span>
                <span class="label">Paused</span>
            </div>
            <div class="cron-stat success">
                <span class="count" id="crons-success-rate">-</span>
                <span class="label">Success Rate</span>
            </div>
            <div class="cron-stat">
                <span class="count" id="crons-next-run">-</span>
                <span class="label">Next Run</span>
            </div>
        </div>

        <div class="crons-table-container">
            <table class="crons-table">
                <thead>
                    <tr>
                        <th>Name</th>
                        <th>Schedule</th>
                        <th>Status</th>
                        <th>Last Run</th>
                        <th>Last Result</th>
                        <th>Next Run</th>
                        <th>Avg Duration</th>
                        <th>Actions</th>
                    </tr>
                </thead>
                <tbody id="crons-tbody">
                    <tr class="empty-row">
                        <td colspan="8">Loading cron jobs...</td>
                    </tr>
                </tbody>
            </table>
        </div>

        <div class="panel">
            <h3>📊 Recent Executions</h3>
            <div class="chart-container">
                <canvas id="cron-executions-chart"></canvas>
            </div>
        </div>

        <div class="panel">
            <h3>📜 Execution History</h3>
            <div class="cron-history-table-container">
                <table class="cron-history-table">
                    <thead>
                        <tr>
                            <th>Cron</th>
                            <th>Started</th>
                            <th>Duration</th>
                            <th>Status</th>
                            <th>Error</th>
                        </tr>
                    </thead>
                    <tbody id="cron-history-tbody">
                        <tr class="empty-row">
                            <td colspan="5">Loading history...</td>
                        </tr>
                    </tbody>
                </table>
            </div>
        </div>
    "#;

    Html(base_template("Crons", content, "crons"))
}

/// Cluster page.
pub async fn cluster(State(_state): State<DashboardState>) -> Html<String> {
    let content = r#"
        <div class="cluster-health" id="cluster-health-panel">
            <div class="health-indicator" id="health-indicator">
                <span class="health-icon" id="health-icon">...</span>
                <span class="health-text" id="health-text">Loading...</span>
            </div>
            <div class="cluster-info">
                <span id="node-count">- Nodes</span>
                <span>|</span>
                <span id="leader-info">Leader: -</span>
            </div>
        </div>

        <div class="nodes-grid" id="nodes-grid">
            <p class="empty-state">Loading nodes...</p>
        </div>

        <div class="panel">
            <h3>Leadership</h3>
            <table class="leadership-table">
                <thead>
                    <tr>
                        <th>Role</th>
                        <th>Leader Node</th>
                    </tr>
                </thead>
                <tbody id="leadership-tbody">
                    <tr class="empty-row">
                        <td colspan="2">Loading leaders...</td>
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

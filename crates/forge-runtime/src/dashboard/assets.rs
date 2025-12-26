use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Dashboard asset handlers.
pub struct DashboardAssets;

/// CSS styles.
pub async fn styles_css() -> Response {
    let css = r#"
:root {
    --bg-primary: #0f0f0f;
    --bg-secondary: #1a1a1a;
    --bg-tertiary: #252525;
    --text-primary: #ffffff;
    --text-secondary: #a0a0a0;
    --accent: #3b82f6;
    --accent-hover: #2563eb;
    --success: #22c55e;
    --warning: #eab308;
    --error: #ef4444;
    --border: #333;
    --shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.3);
}

* {
    margin: 0;
    padding: 0;
    box-sizing: border-box;
}

body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: var(--bg-primary);
    color: var(--text-primary);
    line-height: 1.6;
}

.dashboard {
    display: flex;
    min-height: 100vh;
}

/* Sidebar */
.sidebar {
    width: 240px;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    position: fixed;
    height: 100vh;
}

.sidebar-header {
    padding: 20px;
    border-bottom: 1px solid var(--border);
}

.sidebar-header h1 {
    font-size: 1.5rem;
    font-weight: 700;
}

.sidebar-header .version {
    font-size: 0.75rem;
    color: var(--text-secondary);
}

.nav-links {
    list-style: none;
    padding: 10px 0;
    flex: 1;
}

.nav-links li a {
    display: block;
    padding: 12px 20px;
    color: var(--text-secondary);
    text-decoration: none;
    transition: all 0.2s;
}

.nav-links li a:hover,
.nav-links li a.active {
    background: var(--bg-tertiary);
    color: var(--text-primary);
    border-left: 3px solid var(--accent);
}

.sidebar-footer {
    padding: 20px;
    border-top: 1px solid var(--border);
}

.sidebar-footer a {
    color: var(--text-secondary);
    text-decoration: none;
    font-size: 0.875rem;
}

/* Main Content */
.content {
    margin-left: 240px;
    flex: 1;
    min-height: 100vh;
}

.content-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 20px 30px;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    z-index: 100;
}

.content-header h2 {
    font-size: 1.5rem;
    font-weight: 600;
}

.header-actions {
    display: flex;
    gap: 10px;
}

.content-body {
    padding: 30px;
}

/* Stats Grid */
.stats-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 20px;
    margin-bottom: 30px;
}

.stat-card {
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 20px;
    display: flex;
    gap: 15px;
    border: 1px solid var(--border);
}

.stat-icon {
    font-size: 2rem;
}

.stat-content h3 {
    font-size: 0.875rem;
    color: var(--text-secondary);
    margin-bottom: 5px;
}

.stat-value {
    font-size: 1.75rem;
    font-weight: 700;
}

.stat-label {
    font-size: 0.75rem;
    color: var(--text-secondary);
}

/* Charts */
.charts-row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(400px, 1fr));
    gap: 20px;
    margin-bottom: 30px;
}

.chart-container {
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 20px;
    border: 1px solid var(--border);
}

.chart-container.full-width {
    grid-column: 1 / -1;
}

.chart-container h3 {
    font-size: 1rem;
    margin-bottom: 15px;
    color: var(--text-secondary);
}

.chart-container canvas {
    width: 100% !important;
    height: 200px !important;
}

/* Panels */
.panels-row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(400px, 1fr));
    gap: 20px;
    margin-bottom: 30px;
}

.panel {
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 20px;
    border: 1px solid var(--border);
    margin-bottom: 20px;
}

.panel h3 {
    font-size: 1rem;
    margin-bottom: 15px;
}

/* Forms */
.btn {
    padding: 8px 16px;
    border-radius: 6px;
    border: none;
    cursor: pointer;
    font-size: 0.875rem;
    transition: all 0.2s;
}

.btn-primary {
    background: var(--accent);
    color: white;
}

.btn-primary:hover {
    background: var(--accent-hover);
}

.btn-secondary {
    background: var(--bg-tertiary);
    color: var(--text-primary);
    border: 1px solid var(--border);
}

.btn-secondary:hover {
    background: var(--border);
}

.search-input,
.select-input,
.number-input,
.time-range-select {
    padding: 8px 12px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg-tertiary);
    color: var(--text-primary);
    font-size: 0.875rem;
}

.search-input {
    width: 300px;
}

.number-input {
    width: 150px;
}

/* Tables */
table {
    width: 100%;
    border-collapse: collapse;
}

th, td {
    padding: 12px;
    text-align: left;
    border-bottom: 1px solid var(--border);
}

th {
    font-weight: 600;
    color: var(--text-secondary);
    font-size: 0.75rem;
    text-transform: uppercase;
}

tbody tr:hover {
    background: var(--bg-tertiary);
}

/* Badges */
.level-badge,
.status-badge {
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
}

.level-info { background: rgba(59, 130, 246, 0.2); color: #60a5fa; }
.level-warn { background: rgba(234, 179, 8, 0.2); color: #fbbf24; }
.level-error { background: rgba(239, 68, 68, 0.2); color: #f87171; }
.level-debug { background: rgba(107, 114, 128, 0.2); color: #9ca3af; }

.status-ok { background: rgba(34, 197, 94, 0.2); color: #4ade80; }
.status-error { background: rgba(239, 68, 68, 0.2); color: #f87171; }

/* Metrics */
.metrics-controls {
    display: flex;
    gap: 15px;
    margin-bottom: 20px;
}

.metrics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr));
    gap: 15px;
    margin-bottom: 30px;
}

.metric-card {
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 15px;
    border: 1px solid var(--border);
    cursor: pointer;
    transition: all 0.2s;
}

.metric-card:hover {
    border-color: var(--accent);
}

.metric-card h4 {
    font-size: 0.875rem;
    margin-bottom: 8px;
    word-break: break-all;
}

.metric-value {
    font-size: 1.5rem;
    font-weight: 700;
    margin-bottom: 5px;
}

.metric-type {
    font-size: 0.75rem;
    color: var(--text-secondary);
}

.metric-sparkline {
    height: 40px;
    margin-top: 10px;
}

/* Logs */
.logs-controls {
    display: flex;
    gap: 15px;
    margin-bottom: 20px;
}

.logs-table-container {
    background: var(--bg-secondary);
    border-radius: 8px;
    border: 1px solid var(--border);
    overflow-x: auto;
    margin-bottom: 20px;
}

.log-time {
    font-family: monospace;
    white-space: nowrap;
}

.log-message {
    max-width: 500px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.logs-pagination {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 20px;
}

/* Traces */
.traces-controls {
    display: flex;
    gap: 15px;
    margin-bottom: 20px;
    flex-wrap: wrap;
}

.checkbox-label {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-secondary);
}

.trace-timeline {
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 20px;
    border: 1px solid var(--border);
    margin-bottom: 20px;
}

.timeline-header {
    display: grid;
    grid-template-columns: 120px 200px 1fr;
    gap: 20px;
    padding-bottom: 10px;
    border-bottom: 1px solid var(--border);
    font-size: 0.75rem;
    color: var(--text-secondary);
    text-transform: uppercase;
}

.timeline-row {
    display: grid;
    grid-template-columns: 120px 200px 1fr;
    gap: 20px;
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
    align-items: center;
}

.timeline-bar {
    background: var(--accent);
    height: 20px;
    border-radius: 4px;
    position: relative;
    font-size: 0.75rem;
    display: flex;
    align-items: center;
    padding-left: 8px;
    color: white;
}

/* Alerts */
.alerts-summary {
    display: flex;
    gap: 20px;
    margin-bottom: 20px;
}

.alert-stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 15px 30px;
    background: var(--bg-secondary);
    border-radius: 8px;
    border: 1px solid var(--border);
}

.alert-stat.critical { border-color: var(--error); }
.alert-stat.warning { border-color: var(--warning); }

.alert-stat .count {
    font-size: 2rem;
    font-weight: 700;
}

.alert-stat .label {
    font-size: 0.875rem;
    color: var(--text-secondary);
}

/* Tabs */
.tabs {
    display: flex;
    gap: 5px;
    margin-bottom: 20px;
    border-bottom: 1px solid var(--border);
    padding-bottom: 5px;
}

.tab {
    padding: 10px 20px;
    background: transparent;
    border: none;
    color: var(--text-secondary);
    cursor: pointer;
    border-radius: 6px 6px 0 0;
}

.tab:hover {
    color: var(--text-primary);
}

.tab.active {
    background: var(--bg-secondary);
    color: var(--text-primary);
}

/* Jobs */
.jobs-stats,
.workflows-stats {
    display: flex;
    gap: 20px;
    margin-bottom: 20px;
}

.job-stat,
.workflow-stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 15px 30px;
    background: var(--bg-secondary);
    border-radius: 8px;
    border: 1px solid var(--border);
}

.job-stat.error,
.workflow-stat.error {
    border-color: var(--error);
}

.job-stat .count,
.workflow-stat .count {
    font-size: 2rem;
    font-weight: 700;
}

.job-stat .label,
.workflow-stat .label {
    font-size: 0.875rem;
    color: var(--text-secondary);
}

/* Cluster */
.cluster-health {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 20px;
    background: var(--bg-secondary);
    border-radius: 8px;
    border: 1px solid var(--border);
    margin-bottom: 20px;
}

.health-indicator {
    display: flex;
    align-items: center;
    gap: 10px;
    font-weight: 600;
}

.health-indicator.healthy .health-icon {
    color: var(--success);
}

.cluster-info {
    display: flex;
    gap: 15px;
    color: var(--text-secondary);
}

.nodes-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 20px;
    margin-bottom: 20px;
}

.node-card {
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 20px;
    border: 1px solid var(--border);
}

.node-card.leader {
    border-color: var(--accent);
}

.node-header {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 15px;
}

.node-status {
    width: 10px;
    height: 10px;
    border-radius: 50%;
}

.node-status.online {
    background: var(--success);
}

.node-status.offline {
    background: var(--error);
}

.leader-badge {
    background: rgba(59, 130, 246, 0.2);
    color: #60a5fa;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.75rem;
    margin-left: auto;
}

.node-details p {
    font-size: 0.875rem;
    margin-bottom: 5px;
    color: var(--text-secondary);
}

.node-metrics {
    margin-top: 15px;
}

.node-metric {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
}

.metric-bar {
    flex: 1;
    height: 8px;
    background: var(--bg-tertiary);
    border-radius: 4px;
    overflow: hidden;
}

.metric-fill {
    height: 100%;
    background: var(--accent);
}

/* Empty State */
.empty-state {
    text-align: center;
    padding: 40px;
    color: var(--text-secondary);
}

.empty-state .subtitle {
    font-size: 0.875rem;
    margin-top: 10px;
}

.empty-row td {
    text-align: center;
    color: var(--text-secondary);
    padding: 40px;
}

/* Responsive */
@media (max-width: 768px) {
    .sidebar {
        display: none;
    }

    .content {
        margin-left: 0;
    }

    .stats-grid,
    .charts-row,
    .panels-row {
        grid-template-columns: 1fr;
    }
}
"#;

    (StatusCode::OK, [(header::CONTENT_TYPE, "text/css")], css).into_response()
}

/// Main JavaScript.
pub async fn main_js() -> Response {
    let js = r#"
// FORGE Dashboard JavaScript

let currentPage = 1;
const pageSize = 50;

document.addEventListener('DOMContentLoaded', function() {
    initDashboard();
    setInterval(refreshData, 5000); // Refresh every 5 seconds
});

function initDashboard() {
    refreshData();
    setupEventHandlers();
    loadPageSpecificData();
    if (document.getElementById('requests-chart')) {
        initCharts();
    }
}

function getTimeRange() {
    const select = document.getElementById('time-range');
    return select ? select.value : '1h';
}

function setupEventHandlers() {
    const timeRange = document.getElementById('time-range');
    if (timeRange) {
        timeRange.addEventListener('change', function() {
            refreshData();
            loadPageSpecificData();
        });
    }

    const refreshBtn = document.getElementById('refresh-btn');
    if (refreshBtn) {
        refreshBtn.addEventListener('click', function() {
            refreshData();
            loadPageSpecificData();
        });
    }

    const tabs = document.querySelectorAll('.tab');
    tabs.forEach(tab => {
        tab.addEventListener('click', function() {
            tabs.forEach(t => t.classList.remove('active'));
            this.classList.add('active');
        });
    });
}

function loadPageSpecificData() {
    const path = window.location.pathname;
    if (path.includes('/metrics')) loadMetrics();
    else if (path.includes('/logs')) loadLogs();
    else if (path.includes('/traces') && !path.includes('/traces/')) loadTraces();
    else if (path.includes('/jobs')) loadJobs();
    else if (path.includes('/workflows')) loadWorkflows();
    else if (path.includes('/cluster')) loadCluster();
}

async function refreshData() {
    try {
        const [stats, alerts, logs, health, nodes] = await Promise.all([
            fetch('/_api/system/stats').then(r => r.json()).catch(() => null),
            fetch('/_api/alerts/active').then(r => r.json()).catch(() => null),
            fetch('/_api/logs?limit=10').then(r => r.json()).catch(() => null),
            fetch('/_api/cluster/health').then(r => r.json()).catch(() => null),
            fetch('/_api/cluster/nodes').then(r => r.json()).catch(() => null),
        ]);

        if (stats?.success) updateStats(stats.data);
        if (alerts?.success) updateAlerts(alerts.data);
        if (logs?.success) updateRecentLogs(logs.data);
        if (health?.success) updateClusterHealth(health.data);
        if (nodes?.success) updateClusterNodes(nodes.data);
    } catch (error) {
        console.error('Failed to refresh data:', error);
    }
}

function updateStats(data) {
    setText('stat-requests', data.http_requests_per_second?.toFixed(1) || '0');
    setText('stat-connections', data.active_connections || '0');
    setText('stat-latency', '-');
    setText('stat-errors', '0%');
}

function updateAlerts(alerts) {
    const container = document.getElementById('active-alerts');
    if (!container) return;
    if (!alerts || alerts.length === 0) {
        container.innerHTML = '<p class="empty-state">No active alerts</p>';
        return;
    }
    container.innerHTML = alerts.map(alert => `
        <div class="alert-item ${alert.severity}">
            <span class="alert-severity">${alert.severity.toUpperCase()}</span>
            <span class="alert-message">${alert.message || alert.name}</span>
        </div>
    `).join('');
}

function updateRecentLogs(logs) {
    const container = document.getElementById('recent-logs');
    if (!container) return;
    if (!logs || logs.length === 0) {
        container.innerHTML = '<p class="empty-state">No recent logs</p>';
        return;
    }
    container.innerHTML = logs.map(log => `
        <div class="log-item ${log.level}">
            <span class="log-time">${formatTime(log.timestamp)}</span>
            <span class="level-badge level-${log.level}">${log.level.toUpperCase()}</span>
            <span class="log-message">${escapeHtml(log.message)}</span>
        </div>
    `).join('');
}

function updateClusterHealth(health) {
    const indicator = document.getElementById('health-indicator');
    const icon = document.getElementById('health-icon');
    const text = document.getElementById('health-text');
    const nodeCount = document.getElementById('node-count');
    const leaderInfo = document.getElementById('leader-info');

    if (indicator) indicator.className = `health-indicator ${health.status}`;
    if (icon) icon.textContent = health.status === 'healthy' ? '✓' : health.status === 'degraded' ? '!' : '✗';
    if (text) text.textContent = health.status === 'healthy' ? 'Cluster Healthy' : health.status === 'degraded' ? 'Cluster Degraded' : 'Cluster Unhealthy';
    if (nodeCount) nodeCount.textContent = `${health.node_count} Node${health.node_count !== 1 ? 's' : ''}`;
    if (leaderInfo) leaderInfo.textContent = `Leader: ${health.leader_node || 'None'}`;

    const leaderTbody = document.getElementById('leadership-tbody');
    if (leaderTbody && health.leaders) {
        const leaders = Object.entries(health.leaders);
        if (leaders.length === 0) {
            leaderTbody.innerHTML = '<tr class="empty-row"><td colspan="2">No leaders elected</td></tr>';
        } else {
            leaderTbody.innerHTML = leaders.map(([role, nodeId]) => `
                <tr><td>${role}</td><td>${nodeId}</td></tr>
            `).join('');
        }
    }
}

function updateClusterNodes(nodes) {
    const container = document.getElementById('nodes-grid');
    const overviewContainer = document.getElementById('cluster-nodes');

    const html = (!nodes || nodes.length === 0)
        ? '<p class="empty-state">No nodes registered</p>'
        : nodes.map(node => `
            <div class="node-card ${node.status === 'active' ? '' : 'offline'}">
                <div class="node-header">
                    <span class="node-status ${node.status === 'active' ? 'online' : 'offline'}"></span>
                    <h4>${escapeHtml(node.name)}</h4>
                </div>
                <div class="node-details">
                    <p><strong>Roles:</strong> ${node.roles?.join(', ') || 'None'}</p>
                    <p><strong>Version:</strong> ${node.version || 'Unknown'}</p>
                    <p><strong>Started:</strong> ${formatTime(node.started_at)}</p>
                    <p><strong>Last Heartbeat:</strong> ${formatRelativeTime(node.last_heartbeat)}</p>
                </div>
            </div>
        `).join('');

    if (container) container.innerHTML = html;
    if (overviewContainer) overviewContainer.innerHTML = html;
}

// Metrics page
async function loadMetrics() {
    const container = document.getElementById('metrics-list');
    if (!container) return;

    try {
        const res = await fetch('/_api/metrics').then(r => r.json());
        if (!res.success || !res.data || res.data.length === 0) {
            container.innerHTML = '<p class="empty-state">No metrics recorded yet</p>';
            return;
        }
        container.innerHTML = res.data.map(m => `
            <div class="metric-card" onclick="selectMetric('${escapeHtml(m.name)}')">
                <h4>${escapeHtml(m.name)}</h4>
                <p class="metric-value">${formatMetricValue(m.current_value, m.kind)}</p>
                <p class="metric-type">${m.kind}</p>
            </div>
        `).join('');
    } catch (e) {
        container.innerHTML = '<p class="empty-state">Failed to load metrics</p>';
    }
}

function formatMetricValue(value, kind) {
    if (kind === 'histogram' || kind === 'summary') return value.toFixed(4) + 's';
    if (value >= 1000000) return (value / 1000000).toFixed(2) + 'M';
    if (value >= 1000) return (value / 1000).toFixed(1) + 'K';
    return value.toFixed(value % 1 === 0 ? 0 : 2);
}

function selectMetric(name) {
    console.log('Selected metric:', name);
}

// Logs page
async function loadLogs() {
    const tbody = document.getElementById('logs-tbody');
    if (!tbody) return;

    const level = document.getElementById('log-level')?.value || '';
    const period = getTimeRange();

    try {
        const url = `/_api/logs?limit=${pageSize}&period=${period}` + (level ? `&level=${level}` : '');
        const res = await fetch(url).then(r => r.json());

        if (!res.success || !res.data || res.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="4">No logs found</td></tr>';
            return;
        }
        tbody.innerHTML = res.data.map(log => `
            <tr class="log-row log-${log.level}">
                <td class="log-time">${formatTime(log.timestamp)}</td>
                <td class="log-level"><span class="level-badge level-${log.level}">${log.level.toUpperCase()}</span></td>
                <td class="log-message">${escapeHtml(log.message)}</td>
                <td class="log-trace">${log.trace_id ? `<a href="/_dashboard/traces/${log.trace_id}">${log.trace_id.substring(0, 8)}</a>` : '-'}</td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="4">Failed to load logs</td></tr>';
    }
}

// Traces page
async function loadTraces() {
    const tbody = document.getElementById('traces-tbody');
    if (!tbody) return;

    const errorsOnly = document.getElementById('errors-only')?.checked || false;
    const period = getTimeRange();

    try {
        const url = `/_api/traces?limit=${pageSize}&period=${period}` + (errorsOnly ? '&errors_only=true' : '');
        const res = await fetch(url).then(r => r.json());

        if (!res.success || !res.data || res.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="7">No traces found</td></tr>';
            return;
        }
        tbody.innerHTML = res.data.map(t => `
            <tr class="trace-row">
                <td class="trace-id"><a href="/_dashboard/traces/${t.trace_id}">${t.trace_id.substring(0, 12)}</a></td>
                <td class="trace-name">${escapeHtml(t.root_span_name || 'Unknown')}</td>
                <td class="trace-service">${escapeHtml(t.service)}</td>
                <td class="trace-duration">${t.duration_ms}ms</td>
                <td class="trace-spans">${t.span_count}</td>
                <td class="trace-status"><span class="status-badge ${t.error ? 'status-error' : 'status-ok'}">${t.error ? 'ERROR' : 'OK'}</span></td>
                <td class="trace-time">${formatTime(t.started_at)}</td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="7">Failed to load traces</td></tr>';
    }
}

// Trace detail page
async function loadTraceDetail(traceId) {
    const spansContainer = document.getElementById('trace-spans');
    const summary = document.getElementById('trace-summary');

    if (!spansContainer) return;

    try {
        const res = await fetch(`/_api/traces/${traceId}`).then(r => r.json());

        if (!res.success || !res.data || !res.data.spans || res.data.spans.length === 0) {
            spansContainer.innerHTML = '<p class="empty-state">Trace not found</p>';
            return;
        }

        const trace = res.data;
        const spans = trace.spans;
        const rootSpan = spans[0];
        const totalDuration = Math.max(...spans.map(s => s.duration_ms || 0));
        const startTime = new Date(rootSpan.start_time);

        if (summary) {
            summary.textContent = `Started: ${formatTime(rootSpan.start_time)} | Duration: ${totalDuration}ms | ${spans.length} spans`;
        }

        spansContainer.innerHTML = spans.map((span, i) => {
            const offset = span.start_time ? ((new Date(span.start_time) - startTime) / totalDuration * 100) : 0;
            const width = span.duration_ms ? (span.duration_ms / totalDuration * 100) : 1;
            const indent = span.parent_span_id ? 20 : 0;
            const statusClass = span.status === 'error' ? 'status-error' : '';

            return `
                <div class="timeline-row ${i === 0 ? 'root' : 'child'} ${statusClass}" style="margin-left: ${indent}px;" onclick="showSpanDetails(${i})">
                    <span class="service">${escapeHtml(span.service)}</span>
                    <span class="operation">${escapeHtml(span.name)}</span>
                    <div class="timeline-bar" style="width: ${Math.max(width, 1)}%; left: ${offset}%;">${span.duration_ms || 0}ms</div>
                </div>
            `;
        }).join('');

        window.traceSpans = spans;
    } catch (e) {
        spansContainer.innerHTML = '<p class="empty-state">Failed to load trace</p>';
    }
}

function showSpanDetails(index) {
    const span = window.traceSpans?.[index];
    const container = document.getElementById('span-details');
    if (!span || !container) return;

    container.innerHTML = `
        <h4>Span: ${escapeHtml(span.name)}</h4>
        <table>
            <tr><td><strong>Span ID:</strong></td><td>${span.span_id}</td></tr>
            <tr><td><strong>Parent:</strong></td><td>${span.parent_span_id || 'Root'}</td></tr>
            <tr><td><strong>Service:</strong></td><td>${span.service}</td></tr>
            <tr><td><strong>Kind:</strong></td><td>${span.kind}</td></tr>
            <tr><td><strong>Status:</strong></td><td>${span.status}</td></tr>
            <tr><td><strong>Duration:</strong></td><td>${span.duration_ms || 0}ms</td></tr>
            <tr><td><strong>Start:</strong></td><td>${formatTime(span.start_time)}</td></tr>
        </table>
        ${Object.keys(span.attributes || {}).length > 0 ? `<h5>Attributes</h5><pre>${JSON.stringify(span.attributes, null, 2)}</pre>` : ''}
        ${(span.events || []).length > 0 ? `<h5>Events</h5><pre>${JSON.stringify(span.events, null, 2)}</pre>` : ''}
    `;
}

// Jobs page
async function loadJobs() {
    const tbody = document.getElementById('jobs-tbody');
    if (!tbody) return;

    try {
        const [jobsRes, statsRes] = await Promise.all([
            fetch('/_api/jobs?limit=50').then(r => r.json()),
            fetch('/_api/jobs/stats').then(r => r.json()),
        ]);

        if (statsRes.success) {
            const s = statsRes.data;
            setText('jobs-pending', s.pending);
            setText('jobs-running', s.running);
            setText('jobs-completed', s.completed);
            setText('jobs-failed', s.failed + s.dead_letter);
        }

        if (!jobsRes.success || !jobsRes.data || jobsRes.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="7">No jobs found</td></tr>';
            return;
        }

        tbody.innerHTML = jobsRes.data.map(job => `
            <tr>
                <td>${job.id.substring(0, 8)}</td>
                <td>${escapeHtml(job.job_type)}</td>
                <td>${job.priority}</td>
                <td><span class="status-badge status-${job.status}">${job.status}</span></td>
                <td>${job.attempts}/${job.max_attempts}</td>
                <td>${formatTime(job.created_at)}</td>
                <td>${job.last_error ? escapeHtml(job.last_error.substring(0, 30)) : '-'}</td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="7">Failed to load jobs</td></tr>';
    }
}

// Workflows page
async function loadWorkflows() {
    const tbody = document.getElementById('workflows-tbody');
    if (!tbody) return;

    try {
        const [workflowsRes, statsRes] = await Promise.all([
            fetch('/_api/workflows?limit=50').then(r => r.json()),
            fetch('/_api/workflows/stats').then(r => r.json()),
        ]);

        if (statsRes.success) {
            const s = statsRes.data;
            setText('workflows-running', s.running);
            setText('workflows-completed', s.completed);
            setText('workflows-waiting', s.waiting);
            setText('workflows-failed', s.failed);
        }

        if (!workflowsRes.success || !workflowsRes.data || workflowsRes.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="7">No workflow runs found</td></tr>';
            return;
        }

        tbody.innerHTML = workflowsRes.data.map(w => `
            <tr>
                <td>${w.id.substring(0, 8)}</td>
                <td>${escapeHtml(w.workflow_name)}</td>
                <td>${w.version || '-'}</td>
                <td><span class="status-badge status-${w.status}">${w.status}</span></td>
                <td>${w.current_step || '-'}</td>
                <td>${formatTime(w.started_at)}</td>
                <td>${w.error ? escapeHtml(w.error.substring(0, 30)) : '-'}</td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="7">Failed to load workflows</td></tr>';
    }
}

// Cluster page
async function loadCluster() {
    try {
        const [nodesRes, healthRes] = await Promise.all([
            fetch('/_api/cluster/nodes').then(r => r.json()),
            fetch('/_api/cluster/health').then(r => r.json()),
        ]);

        if (healthRes.success) updateClusterHealth(healthRes.data);
        if (nodesRes.success) updateClusterNodes(nodesRes.data);
    } catch (e) {
        console.error('Failed to load cluster data:', e);
    }
}

// Charts
async function initCharts() {
    const period = getTimeRange();
    try {
        const res = await fetch(`/_api/metrics/series?period=${period}`).then(r => r.json());
        const series = res.success ? res.data : [];

        const requestsData = series.find(s => s.name.includes('http_requests'));
        const latencyData = series.find(s => s.name.includes('duration') || s.name.includes('latency'));

        renderChart('requests-chart', requestsData?.points || [], '#3b82f6', 'Requests');
        renderChart('latency-chart', latencyData?.points || [], '#22c55e', 'Latency (ms)');
    } catch (e) {
        renderChart('requests-chart', [], '#3b82f6', 'Requests');
        renderChart('latency-chart', [], '#22c55e', 'Latency (ms)');
    }
}

function renderChart(canvasId, points, color, label) {
    const canvas = document.getElementById(canvasId);
    if (!canvas || !window.Chart) return;

    const labels = points.length > 0
        ? points.map(p => formatTime(p.timestamp))
        : Array.from({length: 20}, (_, i) => '');
    const data = points.length > 0
        ? points.map(p => p.value)
        : Array.from({length: 20}, () => 0);

    const ctx = canvas.getContext('2d');
    new Chart(ctx, {
        type: 'line',
        data: {
            labels: labels.slice(-60),
            datasets: [{
                label: label,
                data: data.slice(-60),
                borderColor: color,
                backgroundColor: color + '20',
                fill: true,
                tension: 0.4,
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false } },
            scales: {
                x: { grid: { color: '#333' }, display: false },
                y: { grid: { color: '#333' }, beginAtZero: true }
            }
        }
    });
}

// Utility functions
function setText(id, value) {
    const el = document.getElementById(id);
    if (el) el.textContent = value;
}

function escapeHtml(str) {
    if (!str) return '';
    return String(str).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function formatTime(timestamp) {
    if (!timestamp) return '-';
    return new Date(timestamp).toLocaleTimeString();
}

function formatRelativeTime(timestamp) {
    if (!timestamp) return 'Unknown';
    const diff = Date.now() - new Date(timestamp).getTime();
    if (diff < 5000) return 'Just now';
    if (diff < 60000) return Math.floor(diff / 1000) + 's ago';
    if (diff < 3600000) return Math.floor(diff / 60000) + 'm ago';
    return Math.floor(diff / 3600000) + 'h ago';
}
"#;

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript")],
        js,
    )
        .into_response()
}

/// Chart.js library (minified placeholder).
pub async fn chart_js() -> Response {
    // In production, this would be the actual Chart.js library
    // For now, we provide a minimal stub that allows the dashboard to render
    let js = r#"
// Chart.js stub - in production, replace with actual Chart.js library
(function(global) {
    function Chart(ctx, config) {
        this.ctx = ctx;
        this.config = config;
        this.render();
    }

    Chart.prototype.render = function() {
        // Minimal placeholder rendering
        const ctx = this.ctx;
        const canvas = ctx.canvas;
        const data = this.config.data.datasets[0].data;
        const color = this.config.data.datasets[0].borderColor || '#3b82f6';

        // Clear canvas
        ctx.clearRect(0, 0, canvas.width, canvas.height);

        // Draw simple line chart
        const width = canvas.width;
        const height = canvas.height;
        const padding = 20;
        const chartWidth = width - 2 * padding;
        const chartHeight = height - 2 * padding;

        const max = Math.max(...data) * 1.1;
        const min = Math.min(...data) * 0.9;
        const range = max - min;

        ctx.strokeStyle = color;
        ctx.lineWidth = 2;
        ctx.beginPath();

        data.forEach((value, i) => {
            const x = padding + (i / (data.length - 1)) * chartWidth;
            const y = height - padding - ((value - min) / range) * chartHeight;

            if (i === 0) {
                ctx.moveTo(x, y);
            } else {
                ctx.lineTo(x, y);
            }
        });

        ctx.stroke();

        // Fill area
        ctx.lineTo(padding + chartWidth, height - padding);
        ctx.lineTo(padding, height - padding);
        ctx.closePath();
        ctx.fillStyle = this.config.data.datasets[0].backgroundColor || 'rgba(59, 130, 246, 0.1)';
        ctx.fill();
    };

    Chart.prototype.destroy = function() {};
    Chart.prototype.update = function() { this.render(); };

    global.Chart = Chart;
})(window);
"#;

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript")],
        js,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_assets_compile() {
        // Just verify the module compiles
    }
}

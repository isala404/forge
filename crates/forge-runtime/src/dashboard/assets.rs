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

/* Modal */
.modal {
    position: fixed;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    background: rgba(0, 0, 0, 0.7);
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
}

.modal-content {
    background: var(--bg-secondary);
    border-radius: 8px;
    width: 90%;
    max-width: 700px;
    max-height: 80vh;
    overflow: hidden;
    box-shadow: var(--shadow);
}

.modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 16px 20px;
    border-bottom: 1px solid var(--border);
}

.modal-header h3 {
    margin: 0;
    font-size: 1.1rem;
}

.modal-close {
    background: none;
    border: none;
    color: var(--text-secondary);
    font-size: 1.5rem;
    cursor: pointer;
    padding: 0;
    line-height: 1;
}

.modal-close:hover {
    color: var(--text-primary);
}

.modal-body {
    padding: 20px;
    overflow-y: auto;
    max-height: calc(80vh - 60px);
}

.detail-grid {
    display: grid;
    grid-template-columns: 140px 1fr;
    gap: 12px;
}

.detail-label {
    color: var(--text-secondary);
    font-weight: 500;
}

.detail-value {
    color: var(--text-primary);
}

.detail-value.error {
    color: var(--error);
}

/* Progress bar */
.progress-bar {
    width: 100%;
    height: 20px;
    background: var(--bg-tertiary);
    border-radius: 4px;
    overflow: hidden;
    position: relative;
}

.progress-bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s ease;
}

.progress-bar-text {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    font-size: 0.75rem;
    color: var(--text-primary);
}

.progress-inline {
    display: flex;
    align-items: center;
    gap: 8px;
}

.progress-inline .progress-bar {
    width: 80px;
    height: 8px;
}

.progress-inline .progress-percent {
    font-size: 0.8rem;
    color: var(--text-secondary);
    min-width: 35px;
}

/* Clickable rows */
.clickable-row {
    cursor: pointer;
    transition: background 0.2s;
}

.clickable-row:hover {
    background: var(--bg-tertiary);
}

/* Workflow steps */
.workflow-steps {
    margin-top: 20px;
}

.workflow-steps h4 {
    margin-bottom: 12px;
    color: var(--text-secondary);
}

.step-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 12px;
    background: var(--bg-tertiary);
    border-radius: 4px;
    margin-bottom: 8px;
}

.step-icon {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.75rem;
}

.step-icon.completed { background: var(--success); color: white; }
.step-icon.running { background: var(--accent); color: white; }
.step-icon.pending { background: var(--bg-secondary); color: var(--text-secondary); border: 1px solid var(--border); }
.step-icon.failed { background: var(--error); color: white; }

.step-name {
    flex: 1;
    font-weight: 500;
}

.step-status {
    font-size: 0.8rem;
    color: var(--text-secondary);
}

.step-time {
    font-size: 0.75rem;
    color: var(--text-secondary);
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

    // Metrics page handlers
    const metricSearch = document.getElementById('metric-search');
    const metricType = document.getElementById('metric-type');
    if (metricSearch) {
        metricSearch.addEventListener('input', debounce(() => loadMetrics(), 300));
    }
    if (metricType) {
        metricType.addEventListener('change', () => loadMetrics());
    }

    // Logs page handlers
    const logSearch = document.getElementById('log-search');
    const logLevel = document.getElementById('log-level');
    const liveStreamBtn = document.getElementById('live-stream-btn');
    if (logSearch) {
        logSearch.addEventListener('input', debounce(() => loadLogs(), 300));
    }
    if (logLevel) {
        logLevel.addEventListener('change', () => loadLogs());
    }
    if (liveStreamBtn) {
        liveStreamBtn.addEventListener('click', toggleLiveStream);
    }

    // Traces page handlers
    const traceSearch = document.getElementById('trace-search');
    const minDuration = document.getElementById('min-duration');
    const errorsOnly = document.getElementById('errors-only');
    if (traceSearch) {
        traceSearch.addEventListener('input', debounce(() => loadTraces(), 300));
    }
    if (minDuration) {
        minDuration.addEventListener('input', debounce(() => loadTraces(), 300));
    }
    if (errorsOnly) {
        errorsOnly.addEventListener('change', () => loadTraces());
    }

    // Modal handlers - close on background click or escape key
    document.addEventListener('click', function(e) {
        if (e.target.classList.contains('modal')) {
            e.target.style.display = 'none';
        }
    });
    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') {
            const jobModal = document.getElementById('job-modal');
            const workflowModal = document.getElementById('workflow-modal');
            const metricModal = document.getElementById('metric-modal');
            if (jobModal) jobModal.style.display = 'none';
            if (workflowModal) workflowModal.style.display = 'none';
            if (metricModal) metricModal.style.display = 'none';
        }
    });
}

// Utility: debounce for search inputs
function debounce(fn, delay) {
    let timeout;
    return function(...args) {
        clearTimeout(timeout);
        timeout = setTimeout(() => fn.apply(this, args), delay);
    };
}

function loadPageSpecificData() {
    const path = window.location.pathname;
    if (path.includes('/metrics')) loadMetrics();
    else if (path.includes('/logs')) loadLogs();
    else if (path.includes('/traces') && !path.includes('/traces/')) loadTraces();
    else if (path.includes('/jobs')) loadJobs();
    else if (path.includes('/workflows')) loadWorkflows();
    else if (path.includes('/crons')) loadCrons();
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
    setText('stat-latency', data.p99_latency_ms != null ? data.p99_latency_ms.toFixed(1) : '-');
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

    // Get filter values
    const searchQuery = document.getElementById('metric-search')?.value?.toLowerCase() || '';
    const typeFilter = document.getElementById('metric-type')?.value || '';

    try {
        const res = await fetch('/_api/metrics').then(r => r.json());
        if (!res.success || !res.data || res.data.length === 0) {
            container.innerHTML = '<p class="empty-state">No metrics recorded yet</p>';
            return;
        }

        // Apply filters
        let metrics = res.data;
        if (searchQuery) {
            metrics = metrics.filter(m => m.name.toLowerCase().includes(searchQuery));
        }
        if (typeFilter) {
            metrics = metrics.filter(m => m.kind === typeFilter);
        }

        if (metrics.length === 0) {
            container.innerHTML = '<p class="empty-state">No metrics match your filters</p>';
            return;
        }

        container.innerHTML = metrics.map(m => `
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

async function selectMetric(name) {
    const modal = document.getElementById('metric-modal');
    const title = document.getElementById('metric-modal-title');
    const body = document.getElementById('metric-modal-body');

    if (!modal || !body) return;

    modal.style.display = 'flex';
    title.textContent = name;
    body.innerHTML = 'Loading...';

    try {
        // Fetch metric details
        const res = await fetch(`/_api/metrics/${encodeURIComponent(name)}`).then(r => r.json());
        if (!res.success) {
            body.innerHTML = `<p class="error">Failed to load metric: ${escapeHtml(res.error || 'Unknown error')}</p>`;
            return;
        }

        const metric = res.data;
        body.innerHTML = `
            <div id="metric-detail-chart-container" style="height: 200px; margin-bottom: 16px;">
                <canvas id="metric-detail-chart"></canvas>
            </div>
            <div class="detail-grid">
                <span class="detail-label">Name</span>
                <span class="detail-value">${escapeHtml(metric.name)}</span>

                <span class="detail-label">Type</span>
                <span class="detail-value">${metric.kind || 'counter'}</span>

                <span class="detail-label">Current Value</span>
                <span class="detail-value">${formatMetricValue(metric.current_value || 0, metric.kind)}</span>

                <span class="detail-label">Last Updated</span>
                <span class="detail-value">${metric.timestamp ? formatTime(metric.timestamp) : '-'}</span>
            </div>
            ${Object.keys(metric.labels || {}).length > 0 ? `
                <h4 style="margin-top: 16px;">Labels</h4>
                <pre style="background: var(--bg-tertiary); padding: 12px; border-radius: 4px; overflow-x: auto;">${JSON.stringify(metric.labels, null, 2)}</pre>
            ` : ''}
        `;

        // Load time series for this metric
        const seriesRes = await fetch(`/_api/metrics/series?name=${encodeURIComponent(name)}&period=${getTimeRange()}`).then(r => r.json());
        if (seriesRes.success && seriesRes.data && seriesRes.data.length > 0) {
            const metricSeries = seriesRes.data.find(s => s.name === name) || seriesRes.data[0];
            if (metricSeries && metricSeries.points && metricSeries.points.length > 0) {
                renderChart('metric-detail-chart', metricSeries.points, '#3b82f6', name);
            }
        }
    } catch (e) {
        body.innerHTML = `<p class="error">Failed to load metric details: ${escapeHtml(e.message)}</p>`;
    }
}

function closeMetricModal() {
    const modal = document.getElementById('metric-modal');
    if (modal) modal.style.display = 'none';
}

// Logs page
let logStreamEventSource = null;

async function loadLogs() {
    const tbody = document.getElementById('logs-tbody');
    if (!tbody) return;

    // Get filter values
    const searchQuery = document.getElementById('log-search')?.value?.toLowerCase() || '';
    const level = document.getElementById('log-level')?.value || '';
    const period = getTimeRange();

    try {
        const url = `/_api/logs?limit=${pageSize}&period=${period}` + (level ? `&level=${level}` : '');
        const res = await fetch(url).then(r => r.json());

        if (!res.success || !res.data || res.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="4">No logs found</td></tr>';
            return;
        }

        // Apply search filter client-side
        let logs = res.data;
        if (searchQuery) {
            logs = logs.filter(log =>
                log.message?.toLowerCase().includes(searchQuery) ||
                log.trace_id?.toLowerCase().includes(searchQuery)
            );
        }

        if (logs.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="4">No logs match your search</td></tr>';
            return;
        }

        tbody.innerHTML = logs.map(log => `
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

function toggleLiveStream() {
    const btn = document.getElementById('live-stream-btn');
    const tbody = document.getElementById('logs-tbody');

    if (logStreamEventSource) {
        // Stop streaming
        logStreamEventSource.close();
        logStreamEventSource = null;
        if (btn) {
            btn.textContent = 'Start Live Stream';
            btn.classList.remove('streaming');
        }
        return;
    }

    // Start streaming via SSE
    if (btn) {
        btn.textContent = 'Stop Live Stream';
        btn.classList.add('streaming');
    }

    const level = document.getElementById('log-level')?.value || '';
    const url = '/_api/logs/stream' + (level ? `?level=${level}` : '');

    logStreamEventSource = new EventSource(url);

    logStreamEventSource.onmessage = function(event) {
        try {
            const log = JSON.parse(event.data);
            if (!tbody) return;

            const row = document.createElement('tr');
            row.className = `log-row log-${log.level}`;
            row.innerHTML = `
                <td class="log-time">${formatTime(log.timestamp)}</td>
                <td class="log-level"><span class="level-badge level-${log.level}">${log.level.toUpperCase()}</span></td>
                <td class="log-message">${escapeHtml(log.message)}</td>
                <td class="log-trace">${log.trace_id ? `<a href="/_dashboard/traces/${log.trace_id}">${log.trace_id.substring(0, 8)}</a>` : '-'}</td>
            `;

            // Insert at top (newest first)
            tbody.insertBefore(row, tbody.firstChild);

            // Limit displayed rows
            while (tbody.children.length > 100) {
                tbody.removeChild(tbody.lastChild);
            }
        } catch (e) {
            console.error('Failed to parse log event:', e);
        }
    };

    logStreamEventSource.onerror = function(e) {
        console.error('Log stream error:', e);
        // Auto-reconnect after 3 seconds
        setTimeout(() => {
            if (logStreamEventSource && logStreamEventSource.readyState === EventSource.CLOSED) {
                toggleLiveStream(); // Stop
                toggleLiveStream(); // Restart
            }
        }, 3000);
    };
}

// Traces page
async function loadTraces() {
    const tbody = document.getElementById('traces-tbody');
    if (!tbody) return;

    // Get filter values
    const searchQuery = document.getElementById('trace-search')?.value?.toLowerCase() || '';
    const minDuration = parseInt(document.getElementById('min-duration')?.value) || 0;
    const errorsOnly = document.getElementById('errors-only')?.checked || false;
    const period = getTimeRange();

    try {
        const url = `/_api/traces?limit=${pageSize}&period=${period}` + (errorsOnly ? '&errors_only=true' : '');
        const res = await fetch(url).then(r => r.json());

        if (!res.success || !res.data || res.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="7">No traces found</td></tr>';
            return;
        }

        // Apply filters client-side
        let traces = res.data;

        if (searchQuery) {
            traces = traces.filter(t =>
                t.trace_id?.toLowerCase().includes(searchQuery) ||
                t.service?.toLowerCase().includes(searchQuery) ||
                t.root_span_name?.toLowerCase().includes(searchQuery)
            );
        }

        if (minDuration > 0) {
            traces = traces.filter(t => t.duration_ms >= minDuration);
        }

        if (traces.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="7">No traces match your filters</td></tr>';
            return;
        }

        tbody.innerHTML = traces.map(t => `
            <tr class="trace-row ${t.error ? 'trace-error' : ''}">
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
    const spansContainer = document.getElementById('waterfall-body');
    const summary = document.getElementById('trace-summary');
    const spanTree = document.getElementById('span-tree');

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
                    <span class="service">${escapeHtml(span.service || 'unknown')}</span>
                    <span class="operation">${escapeHtml(span.name)}</span>
                    <div class="timeline-bar" style="width: ${Math.max(width, 1)}%; left: ${offset}%;">${span.duration_ms || 0}ms</div>
                </div>
            `;
        }).join('');

        // Render span tree
        if (spanTree) {
            spanTree.innerHTML = spans.map((span, i) => `
                <div class="span-tree-item ${i === 0 ? 'root' : ''}" onclick="showSpanDetails(${i})" style="padding-left: ${span.parent_span_id ? 20 : 0}px;">
                    <span class="span-icon">${i === 0 ? '🌳' : '├─'}</span>
                    <span class="span-name">${escapeHtml(span.name)}</span>
                    <span class="span-duration">${span.duration_ms || 0}ms</span>
                </div>
            `).join('');
        }

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
            tbody.innerHTML = '<tr class="empty-row"><td colspan="8">No jobs found</td></tr>';
            return;
        }

        tbody.innerHTML = jobsRes.data.map(job => `
            <tr class="clickable-row" onclick="openJobModal('${job.id}')">
                <td>${job.id.substring(0, 8)}</td>
                <td>${escapeHtml(job.job_type)}</td>
                <td>${job.priority}</td>
                <td><span class="status-badge status-${job.status}">${job.status}</span></td>
                <td>${renderJobProgress(job)}</td>
                <td>${job.attempts}/${job.max_attempts}</td>
                <td>${formatTime(job.created_at)}</td>
                <td>${job.last_error ? escapeHtml(job.last_error.substring(0, 30)) : '-'}</td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="8">Failed to load jobs</td></tr>';
    }
}

function renderJobProgress(job) {
    const percent = job.progress_percent;
    if (percent === null || percent === undefined) {
        if (job.status === 'completed') return '<span class="status-badge status-completed">Done</span>';
        if (job.status === 'pending') return '-';
        return '-';
    }
    return `
        <div class="progress-inline">
            <div class="progress-bar">
                <div class="progress-bar-fill" style="width: ${percent}%"></div>
            </div>
            <span class="progress-percent">${percent}%</span>
        </div>
    `;
}

async function openJobModal(jobId) {
    const modal = document.getElementById('job-modal');
    const body = document.getElementById('job-modal-body');
    if (!modal || !body) return;

    modal.style.display = 'flex';
    body.innerHTML = 'Loading...';

    try {
        const res = await fetch(`/_api/jobs/${jobId}`).then(r => r.json());
        if (!res.success) {
            body.innerHTML = `<p class="error">Failed to load job: ${escapeHtml(res.error || 'Unknown error')}</p>`;
            return;
        }

        const job = res.data;
        const progressBar = job.progress_percent !== null ? `
            <div class="progress-bar" style="margin-top: 8px;">
                <div class="progress-bar-fill" style="width: ${job.progress_percent}%"></div>
                <span class="progress-bar-text">${job.progress_percent}%</span>
            </div>
        ` : '';

        body.innerHTML = `
            <div class="detail-grid">
                <span class="detail-label">Job ID</span>
                <span class="detail-value">${escapeHtml(job.id)}</span>

                <span class="detail-label">Type</span>
                <span class="detail-value">${escapeHtml(job.job_type)}</span>

                <span class="detail-label">Status</span>
                <span class="detail-value"><span class="status-badge status-${job.status}">${job.status}</span></span>

                <span class="detail-label">Priority</span>
                <span class="detail-value">${job.priority}</span>

                <span class="detail-label">Attempts</span>
                <span class="detail-value">${job.attempts} / ${job.max_attempts}</span>

                <span class="detail-label">Progress</span>
                <span class="detail-value">${job.progress_percent !== null ? job.progress_percent + '%' : '-'}${job.progress_message ? ' - ' + escapeHtml(job.progress_message) : ''}</span>

                <span class="detail-label">Created</span>
                <span class="detail-value">${formatTime(job.created_at)}</span>

                <span class="detail-label">Started</span>
                <span class="detail-value">${job.started_at ? formatTime(job.started_at) : '-'}</span>

                <span class="detail-label">Completed</span>
                <span class="detail-value">${job.completed_at ? formatTime(job.completed_at) : '-'}</span>

                ${job.last_error ? `
                <span class="detail-label">Error</span>
                <span class="detail-value error">${escapeHtml(job.last_error)}</span>
                ` : ''}
            </div>
            ${progressBar}
            ${job.input ? `<h4 style="margin-top: 16px;">Input</h4><pre style="background: var(--bg-tertiary); padding: 12px; border-radius: 4px; overflow-x: auto;">${JSON.stringify(job.input, null, 2)}</pre>` : ''}
            ${job.output ? `<h4 style="margin-top: 16px;">Output</h4><pre style="background: var(--bg-tertiary); padding: 12px; border-radius: 4px; overflow-x: auto;">${JSON.stringify(job.output, null, 2)}</pre>` : ''}
        `;
    } catch (e) {
        body.innerHTML = `<p class="error">Failed to load job details</p>`;
    }
}

function closeJobModal() {
    const modal = document.getElementById('job-modal');
    if (modal) modal.style.display = 'none';
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
            <tr class="clickable-row" onclick="openWorkflowModal('${w.id}')">
                <td>${w.id.substring(0, 8)}</td>
                <td>${escapeHtml(w.workflow_name)}</td>
                <td>${w.version || '-'}</td>
                <td><span class="status-badge status-${w.status}">${w.status}</span></td>
                <td>${w.current_step ? escapeHtml(w.current_step) : '-'}</td>
                <td>${formatTime(w.started_at)}</td>
                <td>${w.error ? escapeHtml(w.error.substring(0, 30)) : '-'}</td>
            </tr>
        `).join('');
    } catch (e) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="7">Failed to load workflows</td></tr>';
    }
}

async function openWorkflowModal(workflowId) {
    const modal = document.getElementById('workflow-modal');
    const body = document.getElementById('workflow-modal-body');
    if (!modal || !body) return;

    modal.style.display = 'flex';
    body.innerHTML = 'Loading...';

    try {
        const res = await fetch(`/_api/workflows/${workflowId}`).then(r => r.json());
        if (!res.success) {
            body.innerHTML = `<p class="error">Failed to load workflow: ${escapeHtml(res.error || 'Unknown error')}</p>`;
            return;
        }

        const wf = res.data;
        const stepIcons = { completed: '✓', running: '▶', pending: '○', failed: '✗', compensated: '↩' };

        const stepsHtml = wf.steps && wf.steps.length > 0 ? `
            <div class="workflow-steps">
                <h4>Steps</h4>
                ${wf.steps.map(step => `
                    <div class="step-item">
                        <div class="step-icon ${step.status}">${stepIcons[step.status] || '○'}</div>
                        <span class="step-name">${escapeHtml(step.name)}</span>
                        <span class="step-status">${step.status}</span>
                        ${step.started_at ? `<span class="step-time">${formatRelativeTime(step.started_at)}</span>` : ''}
                    </div>
                `).join('')}
            </div>
        ` : '';

        body.innerHTML = `
            <div class="detail-grid">
                <span class="detail-label">Run ID</span>
                <span class="detail-value">${escapeHtml(wf.id)}</span>

                <span class="detail-label">Workflow</span>
                <span class="detail-value">${escapeHtml(wf.workflow_name)}</span>

                <span class="detail-label">Version</span>
                <span class="detail-value">${wf.version || '-'}</span>

                <span class="detail-label">Status</span>
                <span class="detail-value"><span class="status-badge status-${wf.status}">${wf.status}</span></span>

                <span class="detail-label">Current Step</span>
                <span class="detail-value">${wf.current_step ? escapeHtml(wf.current_step) : '-'}</span>

                <span class="detail-label">Started</span>
                <span class="detail-value">${wf.started_at ? formatTime(wf.started_at) : '-'}</span>

                <span class="detail-label">Completed</span>
                <span class="detail-value">${wf.completed_at ? formatTime(wf.completed_at) : '-'}</span>

                ${wf.error ? `
                <span class="detail-label">Error</span>
                <span class="detail-value error">${escapeHtml(wf.error)}</span>
                ` : ''}
            </div>
            ${stepsHtml}
            ${wf.input ? `<h4 style="margin-top: 16px;">Input</h4><pre style="background: var(--bg-tertiary); padding: 12px; border-radius: 4px; overflow-x: auto;">${JSON.stringify(wf.input, null, 2)}</pre>` : ''}
            ${wf.output ? `<h4 style="margin-top: 16px;">Output</h4><pre style="background: var(--bg-tertiary); padding: 12px; border-radius: 4px; overflow-x: auto;">${JSON.stringify(wf.output, null, 2)}</pre>` : ''}
        `;
    } catch (e) {
        body.innerHTML = `<p class="error">Failed to load workflow details</p>`;
    }
}

function closeWorkflowModal() {
    const modal = document.getElementById('workflow-modal');
    if (modal) modal.style.display = 'none';
}

// Crons page
async function loadCrons() {
    const tbody = document.getElementById('crons-tbody');
    const historyTbody = document.getElementById('cron-history-tbody');
    if (!tbody) return;

    try {
        const [cronsRes, statsRes, historyRes] = await Promise.all([
            fetch('/_api/crons').then(r => r.json()),
            fetch('/_api/crons/stats').then(r => r.json()),
            fetch('/_api/crons/history?limit=50').then(r => r.json()),
        ]);

        if (statsRes.success) {
            const s = statsRes.data;
            setText('crons-active', s.active_count || 0);
            setText('crons-paused', s.paused_count || 0);
            setText('crons-success-rate', s.success_rate_24h !== null ? s.success_rate_24h.toFixed(1) + '%' : '-');
            setText('crons-next-run', s.next_scheduled_run ? formatTime(s.next_scheduled_run) : '-');
        }

        if (!cronsRes.success || !cronsRes.data || cronsRes.data.length === 0) {
            tbody.innerHTML = '<tr class="empty-row"><td colspan="8">No cron jobs found</td></tr>';
        } else {
            tbody.innerHTML = cronsRes.data.map(cron => `
                <tr>
                    <td>${escapeHtml(cron.name)}</td>
                    <td><code>${escapeHtml(cron.schedule || '* * * * *')}</code></td>
                    <td><span class="status-badge status-${cron.status || 'active'}">${cron.status || 'active'}</span></td>
                    <td>${cron.last_run ? formatTime(cron.last_run) : '-'}</td>
                    <td>${cron.last_result ? `<span class="status-badge status-${cron.last_result}">${cron.last_result}</span>` : '-'}</td>
                    <td>${cron.next_run ? formatTime(cron.next_run) : '-'}</td>
                    <td>${cron.avg_duration_ms ? cron.avg_duration_ms.toFixed(0) + 'ms' : '-'}</td>
                    <td>-</td>
                </tr>
            `).join('');
        }

        // Load history
        if (historyTbody) {
            if (!historyRes.success || !historyRes.data || historyRes.data.length === 0) {
                historyTbody.innerHTML = '<tr class="empty-row"><td colspan="5">No execution history found</td></tr>';
            } else {
                historyTbody.innerHTML = historyRes.data.map(h => `
                    <tr>
                        <td>${escapeHtml(h.cron_name)}</td>
                        <td>${h.started_at ? formatTime(h.started_at) : '-'}</td>
                        <td>${h.duration_ms ? h.duration_ms.toFixed(0) + 'ms' : '-'}</td>
                        <td><span class="status-badge status-${h.status}">${h.status}</span></td>
                        <td>${h.error ? escapeHtml(h.error.substring(0, 50)) : '-'}</td>
                    </tr>
                `).join('');
            }
        }
    } catch (e) {
        console.error('Failed to load crons:', e);
        tbody.innerHTML = '<tr class="empty-row"><td colspan="8">Failed to load cron jobs</td></tr>';
        if (historyTbody) {
            historyTbody.innerHTML = '<tr class="empty-row"><td colspan="5">Failed to load history</td></tr>';
        }
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
    if (!canvas) return;

    // Wait for Chart.js to load if not ready
    if (!window.Chart) {
        window.addEventListener('chartjs-ready', () => renderChart(canvasId, points, color, label), { once: true });
        return;
    }

    // Destroy existing chart to prevent memory leaks
    if (canvas._chart) {
        canvas._chart.destroy();
    }

    const labels = points.length > 0
        ? points.map(p => formatTime(p.timestamp))
        : Array.from({length: 20}, (_, i) => '');
    const data = points.length > 0
        ? points.map(p => p.value)
        : Array.from({length: 20}, () => 0);

    const ctx = canvas.getContext('2d');
    const chart = new Chart(ctx, {
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
                pointRadius: 2,
                pointHoverRadius: 6,
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            interaction: {
                intersect: false,
                mode: 'index',
            },
            plugins: {
                legend: { display: true, position: 'top', labels: { color: '#9ca3af' } },
                tooltip: {
                    enabled: true,
                    backgroundColor: '#1f2937',
                    titleColor: '#f9fafb',
                    bodyColor: '#d1d5db',
                    borderColor: '#374151',
                    borderWidth: 1,
                    callbacks: {
                        label: (ctx) => ctx.dataset.label + ': ' + ctx.formattedValue
                    }
                },
                zoom: {
                    zoom: {
                        wheel: { enabled: true },
                        pinch: { enabled: true },
                        mode: 'x',
                    },
                    pan: {
                        enabled: true,
                        mode: 'x',
                    }
                }
            },
            scales: {
                x: {
                    grid: { color: '#333' },
                    ticks: { color: '#9ca3af', maxRotation: 0 }
                },
                y: {
                    grid: { color: '#333' },
                    ticks: { color: '#9ca3af' },
                    beginAtZero: true
                }
            },
            onClick: (event, elements) => {
                if (elements.length > 0) {
                    const idx = elements[0].index;
                    const point = points[idx];
                    console.log('Chart clicked:', label, point);
                }
            }
        }
    });

    // Store reference for cleanup
    canvas._chart = chart;
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

/// Chart.js library loader (loads from CDN).
pub async fn chart_js() -> Response {
    // Load Chart.js from CDN with zoom plugin for interactive charts
    let js = r#"
// Chart.js CDN Loader
// Loads Chart.js and zoom plugin dynamically for interactive charts
(function(global) {
    // Check if already loaded
    if (global.Chart && global.Chart.version) {
        global.dispatchEvent(new Event('chartjs-ready'));
        return;
    }

    // Chart.js CDN URL
    const CHARTJS_URL = 'https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js';
    const ZOOM_PLUGIN_URL = 'https://cdn.jsdelivr.net/npm/chartjs-plugin-zoom@2.1.0/dist/chartjs-plugin-zoom.min.js';

    // Load script from URL
    function loadScript(url) {
        return new Promise((resolve, reject) => {
            const script = document.createElement('script');
            script.src = url;
            script.onload = resolve;
            script.onerror = reject;
            document.head.appendChild(script);
        });
    }

    // Load Chart.js first, then zoom plugin
    loadScript(CHARTJS_URL)
        .then(() => loadScript(ZOOM_PLUGIN_URL))
        .then(() => {
            if (global.Chart) {
                // Register zoom plugin
                try {
                    global.Chart.register(global['chartjs-plugin-zoom']);
                } catch (e) {
                    console.warn('Could not register zoom plugin:', e);
                }
                global.dispatchEvent(new Event('chartjs-ready'));
                console.log('Chart.js loaded with zoom plugin');
            }
        })
        .catch(err => {
            console.error('Failed to load Chart.js from CDN:', err);
            // Provide minimal fallback
            global.Chart = createFallbackChart();
            global.dispatchEvent(new Event('chartjs-ready'));
        });

    // Fallback chart implementation (used when CDN fails)
    function createFallbackChart() {
        function FallbackChart(ctx, config) {
            this.ctx = ctx;
            this.config = config;
            this.render();
        }

        FallbackChart.prototype.render = function() {
            const ctx = this.ctx;
            const canvas = ctx.canvas;
            const data = this.config.data?.datasets?.[0]?.data || [];
            const color = this.config.data?.datasets?.[0]?.borderColor || '#3b82f6';

            ctx.clearRect(0, 0, canvas.width, canvas.height);

            if (data.length < 2) return;

            const width = canvas.width;
            const height = canvas.height;
            const padding = 30;
            const chartWidth = width - 2 * padding;
            const chartHeight = height - 2 * padding;

            const max = Math.max(...data) * 1.1 || 1;
            const min = Math.min(...data) * 0.9 || 0;
            const range = max - min || 1;

            // Draw grid
            ctx.strokeStyle = '#333';
            ctx.lineWidth = 0.5;
            for (let i = 0; i <= 4; i++) {
                const y = padding + (i / 4) * chartHeight;
                ctx.beginPath();
                ctx.moveTo(padding, y);
                ctx.lineTo(width - padding, y);
                ctx.stroke();
            }

            // Draw line
            ctx.strokeStyle = color;
            ctx.lineWidth = 2;
            ctx.beginPath();

            data.forEach((value, i) => {
                const x = padding + (i / (data.length - 1)) * chartWidth;
                const y = height - padding - ((value - min) / range) * chartHeight;
                i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
            });

            ctx.stroke();

            // Fill area
            const lastX = padding + chartWidth;
            const lastY = height - padding - ((data[data.length - 1] - min) / range) * chartHeight;
            ctx.lineTo(lastX, height - padding);
            ctx.lineTo(padding, height - padding);
            ctx.closePath();
            ctx.fillStyle = this.config.data?.datasets?.[0]?.backgroundColor || 'rgba(59, 130, 246, 0.1)';
            ctx.fill();
        };

        FallbackChart.prototype.destroy = function() { this.ctx.clearRect(0, 0, this.ctx.canvas.width, this.ctx.canvas.height); };
        FallbackChart.prototype.update = function() { this.render(); };
        FallbackChart.version = 'fallback';

        return FallbackChart;
    }
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

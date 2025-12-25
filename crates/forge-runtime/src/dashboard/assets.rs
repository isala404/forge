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

document.addEventListener('DOMContentLoaded', function() {
    // Initialize dashboard
    initDashboard();

    // Set up auto-refresh
    setInterval(refreshData, 30000);
});

function initDashboard() {
    // Load initial data
    refreshData();

    // Set up event handlers
    setupEventHandlers();

    // Initialize charts if on overview page
    if (document.getElementById('requests-chart')) {
        initCharts();
    }
}

function setupEventHandlers() {
    // Time range selector
    const timeRange = document.getElementById('time-range');
    if (timeRange) {
        timeRange.addEventListener('change', function() {
            refreshData();
        });
    }

    // Refresh button
    const refreshBtn = document.getElementById('refresh-btn');
    if (refreshBtn) {
        refreshBtn.addEventListener('click', function() {
            refreshData();
        });
    }

    // Tab switching
    const tabs = document.querySelectorAll('.tab');
    tabs.forEach(tab => {
        tab.addEventListener('click', function() {
            tabs.forEach(t => t.classList.remove('active'));
            this.classList.add('active');
            // Handle tab content switching
        });
    });
}

async function refreshData() {
    try {
        // Fetch system stats
        const stats = await fetch('/_api/system/stats').then(r => r.json());
        if (stats.success) {
            updateStats(stats.data);
        }

        // Fetch active alerts
        const alerts = await fetch('/_api/alerts/active').then(r => r.json());
        if (alerts.success) {
            updateAlerts(alerts.data);
        }

        // Fetch recent logs
        const logs = await fetch('/_api/logs?limit=10').then(r => r.json());
        if (logs.success) {
            updateLogs(logs.data);
        }

        // Fetch cluster health
        const health = await fetch('/_api/cluster/health').then(r => r.json());
        if (health.success) {
            updateClusterHealth(health.data);
        }
    } catch (error) {
        console.error('Failed to refresh data:', error);
    }
}

function updateStats(data) {
    const elements = {
        'stat-requests': data.http_requests_per_second?.toFixed(1) || '0',
        'stat-latency': '-',
        'stat-errors': '0%',
        'stat-connections': data.active_connections || '0',
    };

    Object.entries(elements).forEach(([id, value]) => {
        const el = document.getElementById(id);
        if (el) el.textContent = value;
    });
}

function updateAlerts(alerts) {
    const container = document.getElementById('active-alerts');
    if (!container) return;

    if (alerts.length === 0) {
        container.innerHTML = '<p class="empty-state">No active alerts</p>';
        return;
    }

    container.innerHTML = alerts.map(alert => `
        <div class="alert-item ${alert.severity}">
            <span class="alert-severity">${alert.severity.toUpperCase()}</span>
            <span class="alert-message">${alert.message}</span>
        </div>
    `).join('');
}

function updateLogs(logs) {
    const container = document.getElementById('recent-logs');
    if (!container) return;

    if (logs.length === 0) {
        container.innerHTML = '<p class="empty-state">No recent logs</p>';
        return;
    }

    container.innerHTML = logs.map(log => `
        <div class="log-item ${log.level}">
            <span class="log-time">${new Date(log.timestamp).toLocaleTimeString()}</span>
            <span class="level-badge level-${log.level}">${log.level.toUpperCase()}</span>
            <span class="log-message">${log.message}</span>
        </div>
    `).join('');
}

function updateClusterHealth(health) {
    // Update cluster health display if on cluster page
    const healthIndicator = document.querySelector('.health-indicator');
    if (healthIndicator) {
        healthIndicator.className = `health-indicator ${health.status}`;
    }
}

function initCharts() {
    // Initialize request rate chart
    const requestsCanvas = document.getElementById('requests-chart');
    if (requestsCanvas && window.Chart) {
        const ctx = requestsCanvas.getContext('2d');
        new Chart(ctx, {
            type: 'line',
            data: {
                labels: Array.from({length: 60}, (_, i) => `${60-i}m`),
                datasets: [{
                    label: 'Requests/s',
                    data: Array.from({length: 60}, () => Math.random() * 100),
                    borderColor: '#3b82f6',
                    backgroundColor: 'rgba(59, 130, 246, 0.1)',
                    fill: true,
                    tension: 0.4,
                }]
            },
            options: {
                responsive: true,
                maintainAspectRatio: false,
                plugins: { legend: { display: false } },
                scales: {
                    x: { grid: { color: '#333' } },
                    y: { grid: { color: '#333' }, beginAtZero: true }
                }
            }
        });
    }

    // Initialize latency chart
    const latencyCanvas = document.getElementById('latency-chart');
    if (latencyCanvas && window.Chart) {
        const ctx = latencyCanvas.getContext('2d');
        new Chart(ctx, {
            type: 'line',
            data: {
                labels: Array.from({length: 60}, (_, i) => `${60-i}m`),
                datasets: [{
                    label: 'p99 Latency (ms)',
                    data: Array.from({length: 60}, () => 20 + Math.random() * 30),
                    borderColor: '#22c55e',
                    backgroundColor: 'rgba(34, 197, 94, 0.1)',
                    fill: true,
                    tension: 0.4,
                }]
            },
            options: {
                responsive: true,
                maintainAspectRatio: false,
                plugins: { legend: { display: false } },
                scales: {
                    x: { grid: { color: '#333' } },
                    y: { grid: { color: '#333' }, beginAtZero: true }
                }
            }
        });
    }
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

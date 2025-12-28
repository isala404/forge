# PLAN: Runtime Versioning, Dashboard Fixes, and Code Generation Improvements

Overall Goal: Enable safe runtime updates without breaking user code, fix all dashboard observability issues, and improve generated code quality.

---

## Part A: Runtime Versioning & Managed Runtime Foundation

Goal: Separate user code from framework runtime to enable "Ruby-style" updates and future managed hosting.

### Architecture Overview

```
Current (Monolithic):
┌─────────────────────────────────────────┐
│           User Binary                    │
│  ┌─────────────────────────────────────┐│
│  │ User Code + FORGE Runtime (linked)  ││
│  └─────────────────────────────────────┘│
└─────────────────────────────────────────┘

Target (Decoupled):
┌──────────────────┐     ┌──────────────────────────────┐
│   User Binary    │────▶│    FORGE Runtime (managed)   │
│  - Functions     │gRPC │  - Gateway, Jobs, Crons      │
│  - Schema        │     │  - Observability, Dashboard  │
│  - Migrations    │     │  - Cluster coordination      │
└──────────────────┘     └──────────────────────────────┘
```

---

### Step A.1: Define Runtime Contract Interface

Goal: Create a stable interface between user code and runtime
Files: `crates/forge-core/src/contract/mod.rs` (NEW), `crates/forge-core/src/contract/version.rs` (NEW)
Verify: `cargo check` passes, interface compiles

Create new module `forge-core/src/contract/`:

```rust
// contract/version.rs
pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CONTRACT_VERSION: u32 = 1;  // Bump on breaking changes

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub contract_version: u32,
    pub supports_hot_reload: bool,
    pub supports_wasm_functions: bool,
    pub supports_grpc_mesh: bool,
    pub max_concurrent_jobs: u32,
    pub observability_version: u32,
}

// contract/mod.rs
pub mod version;
pub mod function;
pub mod registry;

/// Stable function registration contract
pub trait FunctionContract: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn kind(&self) -> FunctionKind;
    fn input_schema(&self) -> serde_json::Value;
    fn output_schema(&self) -> serde_json::Value;
    fn execute(&self, ctx: &dyn ExecutionContext, input: serde_json::Value)
        -> Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send + '_>>;
}

/// Execution context abstraction (hides runtime details)
pub trait ExecutionContext: Send + Sync {
    fn db(&self) -> &dyn DatabaseAccess;
    fn http(&self) -> &dyn HttpClient;
    fn auth(&self) -> &AuthContext;
    fn observability(&self) -> &dyn ObservabilityAccess;
}
```

---

### Step A.2: Create Runtime Manifest System

Goal: Track runtime version requirements in user projects
Files: `crates/forge-core/src/manifest.rs` (NEW), update `crates/forge/src/cli/new.rs`
Verify: Generated projects include `forge.lock` file

Create manifest system:

```rust
// manifest.rs
#[derive(Debug, Serialize, Deserialize)]
pub struct ForgeManifest {
    pub forge_version: String,        // "0.2.0"
    pub contract_version: u32,        // 1
    pub runtime_channel: RuntimeChannel,
    pub features: Vec<String>,        // ["jobs", "crons", "workflows"]
    pub locked_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RuntimeChannel {
    Stable,     // Production-ready
    Beta,       // Preview features
    Canary,     // Bleeding edge
    Pinned(String),  // Specific version "0.2.1"
}

impl ForgeManifest {
    pub fn is_compatible_with(&self, runtime: &RuntimeCapabilities) -> CompatibilityResult {
        if self.contract_version > runtime.contract_version {
            return CompatibilityResult::Incompatible {
                reason: "Runtime too old for this project",
                suggestion: "Update runtime or pin to older version",
            };
        }
        // Forward compatible within same major contract version
        CompatibilityResult::Compatible
    }
}
```

Update CLI to generate `forge.lock`:
```toml
# forge.lock (auto-generated)
forge_version = "0.2.0"
contract_version = 1
runtime_channel = "stable"
features = ["jobs", "crons", "workflows", "realtime"]
locked_at = "2025-12-28T12:00:00Z"

[checksums]
runtime = "sha256:abc123..."
schema = "sha256:def456..."
```

---

### Step A.3: Implement Feature Flags for Runtime Components

Goal: Allow runtime features to be toggled without recompilation
Files: `crates/forge-runtime/src/features.rs` (NEW), update `crates/forge/src/runtime.rs`
Verify: Can disable features via config, `cargo test` passes

```rust
// features.rs
#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeFeatures {
    pub gateway: GatewayFeatures,
    pub jobs: JobFeatures,
    pub crons: CronFeatures,
    pub workflows: WorkflowFeatures,
    pub realtime: RealtimeFeatures,
    pub observability: ObservabilityFeatures,
    pub dashboard: DashboardFeatures,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayFeatures {
    pub enabled: bool,
    pub cors_enabled: bool,
    pub rate_limiting: Option<RateLimitConfig>,
    pub request_logging: bool,
    pub tracing_enabled: bool,
}

// Runtime respects these flags
impl Forge {
    pub async fn run_with_features(&self, features: RuntimeFeatures) -> Result<()> {
        if features.gateway.enabled {
            self.start_gateway(&features.gateway).await?;
        }
        if features.jobs.enabled {
            self.start_job_worker(&features.jobs).await?;
        }
        // ... etc
    }
}
```

---

### Step A.4: Add Runtime Version Negotiation

Goal: Runtime and user code negotiate compatible versions at startup
Files: `crates/forge/src/runtime.rs`, `crates/forge-runtime/src/bootstrap.rs` (NEW)
Verify: Incompatible versions fail fast with clear error

```rust
// bootstrap.rs
pub struct RuntimeBootstrap {
    manifest: ForgeManifest,
    capabilities: RuntimeCapabilities,
}

impl RuntimeBootstrap {
    pub fn negotiate(&self) -> Result<NegotiatedRuntime> {
        let compat = self.manifest.is_compatible_with(&self.capabilities);

        match compat {
            CompatibilityResult::Compatible => {
                tracing::info!(
                    manifest_version = %self.manifest.forge_version,
                    runtime_version = %RUNTIME_VERSION,
                    "Runtime version negotiation successful"
                );
                Ok(NegotiatedRuntime {
                    effective_contract: self.manifest.contract_version,
                    features: self.resolve_features(),
                })
            }
            CompatibilityResult::Incompatible { reason, suggestion } => {
                Err(ForgeError::RuntimeIncompatible {
                    manifest_version: self.manifest.forge_version.clone(),
                    runtime_version: RUNTIME_VERSION.to_string(),
                    reason: reason.to_string(),
                    suggestion: suggestion.to_string(),
                })
            }
        }
    }
}
```

---

### Step A.5: Create Stable RPC Protocol for Managed Runtime

Goal: Define gRPC protocol for future managed runtime communication
Files: `proto/forge_runtime.proto` (NEW), `crates/forge-runtime/src/grpc/mod.rs` (NEW)
Verify: Protocol compiles with `tonic-build`

```protobuf
// proto/forge_runtime.proto
syntax = "proto3";
package forge.runtime.v1;

service ForgeRuntime {
    // Function execution
    rpc ExecuteFunction(ExecuteFunctionRequest) returns (ExecuteFunctionResponse);
    rpc StreamFunction(ExecuteFunctionRequest) returns (stream ExecuteFunctionResponse);

    // Job management
    rpc DispatchJob(DispatchJobRequest) returns (DispatchJobResponse);
    rpc GetJobStatus(GetJobStatusRequest) returns (JobStatus);

    // Subscription management
    rpc Subscribe(SubscribeRequest) returns (stream SubscriptionEvent);
    rpc Unsubscribe(UnsubscribeRequest) returns (UnsubscribeResponse);

    // Health and capabilities
    rpc GetCapabilities(Empty) returns (RuntimeCapabilities);
    rpc HealthCheck(Empty) returns (HealthStatus);
}

message ExecuteFunctionRequest {
    string function_name = 1;
    bytes input_json = 2;
    map<string, string> metadata = 3;
    string trace_id = 4;
}

message RuntimeCapabilities {
    uint32 contract_version = 1;
    string runtime_version = 2;
    repeated string supported_features = 3;
    bool hot_reload_enabled = 4;
}
```

---

### Step A.6: Add Backward Compatibility Layer

Goal: Ensure old user code works with new runtime
Files: `crates/forge-runtime/src/compat/mod.rs` (NEW), `crates/forge-runtime/src/compat/v1.rs` (NEW)
Verify: Tests with old-style registration still pass

```rust
// compat/v1.rs
/// Compatibility shim for contract v1 functions
pub fn wrap_v1_query<Q: ForgeQuery>(query: Q) -> Box<dyn FunctionContract> {
    Box::new(V1QueryWrapper { inner: query })
}

struct V1QueryWrapper<Q: ForgeQuery> {
    inner: Q,
}

impl<Q: ForgeQuery> FunctionContract for V1QueryWrapper<Q> {
    fn name(&self) -> &'static str {
        Q::info().name
    }

    fn execute(&self, ctx: &dyn ExecutionContext, input: serde_json::Value)
        -> Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        Box::pin(async move {
            let args: Q::Args = serde_json::from_value(input)?;
            let legacy_ctx = LegacyQueryContext::from_contract(ctx);
            let result = Q::execute(&legacy_ctx, args).await?;
            Ok(serde_json::to_value(result)?)
        })
    }
}
```

---

## Part B: Dashboard Observability Fixes

Goal: Make the dashboard fully functional with real interactivity

---

### Step B.1: Replace Chart.js Stub with Real Library

Goal: Enable interactive charts with tooltips, zoom, click handlers
Files: `crates/forge-runtime/src/dashboard/assets.rs`
Verify: Charts show tooltips on hover, zoom with scroll wheel

Replace the stub (lines 1218-1290) with CDN-loaded Chart.js:

```rust
// In dashboard/mod.rs - add CDN route or inline real library
pub fn chart_js() -> &'static str {
    // Option 1: Load from CDN (recommended for now)
    r#"
    // Load Chart.js from CDN
    (function() {
        if (window.Chart) return;
        const script = document.createElement('script');
        script.src = 'https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js';
        script.onload = function() {
            // Load zoom plugin
            const zoomScript = document.createElement('script');
            zoomScript.src = 'https://cdn.jsdelivr.net/npm/chartjs-plugin-zoom@2.1.0/dist/chartjs-plugin-zoom.min.js';
            zoomScript.onload = function() {
                Chart.register(window['chartjs-plugin-zoom']);
                window.dispatchEvent(new Event('chartjs-ready'));
            };
            document.head.appendChild(zoomScript);
        };
        document.head.appendChild(script);
    })();
    "#
}
```

Update `renderChart()` function (lines 1147-1182):

```javascript
function renderChart(canvasId, points, color, label) {
    const canvas = document.getElementById(canvasId);
    if (!canvas) return;

    // Wait for Chart.js to load
    if (!window.Chart) {
        window.addEventListener('chartjs-ready', () => renderChart(canvasId, points, color, label));
        return;
    }

    // Destroy existing chart
    if (canvas._chart) {
        canvas._chart.destroy();
    }

    const ctx = canvas.getContext('2d');
    const chart = new Chart(ctx, {
        type: 'line',
        data: {
            labels: points.map(p => formatTime(p.timestamp)),
            datasets: [{
                label: label,
                data: points.map(p => p.value),
                borderColor: color,
                backgroundColor: color + '20',
                fill: true,
                tension: 0.4,
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
                legend: { display: true, position: 'top' },
                tooltip: {
                    enabled: true,
                    callbacks: {
                        label: (ctx) => `${ctx.dataset.label}: ${ctx.formattedValue}`
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
                    ticks: { color: '#888' }
                },
                y: {
                    grid: { color: '#333' },
                    beginAtZero: true,
                    ticks: { color: '#888' }
                }
            },
            onClick: (event, elements) => {
                if (elements.length > 0) {
                    const idx = elements[0].index;
                    const point = points[idx];
                    console.log('Clicked:', point);
                    // Could open detail modal
                }
            }
        }
    });

    canvas._chart = chart;
}
```

---

### Step B.2: Wire Metrics Page Search and Filter

Goal: Make metric search and type filter functional
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Typing in search filters metrics, dropdown filters by type

Add to `setupEventHandlers()` (after line 738):

```javascript
// Metrics page handlers
const metricSearch = document.getElementById('metric-search');
const metricType = document.getElementById('metric-type');

if (metricSearch) {
    let debounceTimer;
    metricSearch.addEventListener('input', (e) => {
        clearTimeout(debounceTimer);
        debounceTimer = setTimeout(() => filterMetrics(), 300);
    });
}

if (metricType) {
    metricType.addEventListener('change', () => filterMetrics());
}

function filterMetrics() {
    const search = document.getElementById('metric-search')?.value?.toLowerCase() || '';
    const type = document.getElementById('metric-type')?.value || '';

    document.querySelectorAll('.metric-card').forEach(card => {
        const name = card.querySelector('h4')?.textContent?.toLowerCase() || '';
        const kind = card.querySelector('.metric-type')?.textContent?.toLowerCase() || '';

        const matchesSearch = !search || name.includes(search);
        const matchesType = !type || kind === type;

        card.style.display = (matchesSearch && matchesType) ? 'block' : 'none';
    });
}
```

Implement `selectMetric()` properly (replace line 890-892):

```javascript
async function selectMetric(name) {
    // Highlight selected card
    document.querySelectorAll('.metric-card').forEach(card => {
        card.classList.remove('selected');
    });
    event.currentTarget.classList.add('selected');

    // Fetch metric time series
    const period = getTimeRange();
    try {
        const res = await fetch(`/_api/metrics/${encodeURIComponent(name)}?period=${period}`).then(r => r.json());
        if (res.success && res.data) {
            const container = document.getElementById('metric-detail');
            if (container) {
                container.innerHTML = `
                    <h3>${escapeHtml(name)}</h3>
                    <canvas id="metric-detail-chart"></canvas>
                    <div class="metric-labels">
                        ${Object.entries(res.data.labels || {}).map(([k, v]) =>
                            `<span class="label">${escapeHtml(k)}: ${escapeHtml(v)}</span>`
                        ).join('')}
                    </div>
                `;
                renderChart('metric-detail-chart', res.data.points || [], '#3b82f6', name);
            }
        }
    } catch (e) {
        console.error('Failed to load metric:', e);
    }
}
```

Add API endpoint for single metric (in `api.rs`):

```rust
pub async fn get_metric(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
    Query(query): Query<TimeRangeQuery>,
) -> Json<ApiResponse<MetricDetail>> {
    let (start, end) = query.get_range();

    let result = sqlx::query(
        r#"
        SELECT value, labels, timestamp
        FROM forge_metrics
        WHERE name = $1 AND timestamp >= $2 AND timestamp <= $3
        ORDER BY timestamp ASC
        "#,
    )
    .bind(&name)
    .bind(start)
    .bind(end)
    .fetch_all(&state.pool)
    .await;

    // ... format response
}
```

Add route in `mod.rs`:
```rust
.route("/metrics/{name}", get(api::get_metric))
```

---

### Step B.3: Wire Logs Page Search and Live Stream

Goal: Make log search functional and add live streaming
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Search filters logs, live stream button shows real-time logs

Add log search handlers (in `setupEventHandlers()`):

```javascript
// Log page handlers
const logSearch = document.getElementById('log-search');
const logLevel = document.getElementById('log-level');
const logStreamToggle = document.getElementById('log-stream-toggle');

if (logSearch) {
    let debounceTimer;
    logSearch.addEventListener('keyup', (e) => {
        if (e.key === 'Enter') {
            searchLogs();
        } else {
            clearTimeout(debounceTimer);
            debounceTimer = setTimeout(() => searchLogs(), 500);
        }
    });
}

if (logLevel) {
    logLevel.addEventListener('change', () => loadLogs());
}

if (logStreamToggle) {
    logStreamToggle.addEventListener('click', toggleLogStream);
}

let logStreamSource = null;
let isStreaming = false;

function toggleLogStream() {
    const btn = document.getElementById('log-stream-toggle');
    if (isStreaming) {
        stopLogStream();
        btn.textContent = '▶ Live Stream';
        btn.classList.remove('streaming');
    } else {
        startLogStream();
        btn.textContent = '⏹ Stop Stream';
        btn.classList.add('streaming');
    }
}

function startLogStream() {
    isStreaming = true;
    logStreamSource = new EventSource('/_api/logs/stream');

    logStreamSource.onmessage = (event) => {
        const log = JSON.parse(event.data);
        prependLogEntry(log);
    };

    logStreamSource.onerror = () => {
        console.error('Log stream error');
        stopLogStream();
    };
}

function stopLogStream() {
    isStreaming = false;
    if (logStreamSource) {
        logStreamSource.close();
        logStreamSource = null;
    }
}

function prependLogEntry(log) {
    const tbody = document.getElementById('logs-tbody');
    if (!tbody) return;

    const row = document.createElement('tr');
    row.innerHTML = `
        <td>${formatTime(log.timestamp)}</td>
        <td><span class="log-level ${log.level}">${log.level}</span></td>
        <td>${escapeHtml(log.message)}</td>
        <td>${escapeHtml(log.target || '-')}</td>
    `;
    tbody.insertBefore(row, tbody.firstChild);

    // Keep max 500 rows
    while (tbody.children.length > 500) {
        tbody.removeChild(tbody.lastChild);
    }
}

async function searchLogs() {
    const query = document.getElementById('log-search')?.value || '';
    const level = document.getElementById('log-level')?.value || '';
    const period = getTimeRange();

    if (!query) {
        loadLogs();
        return;
    }

    const tbody = document.getElementById('logs-tbody');
    if (!tbody) return;

    try {
        const params = new URLSearchParams({ q: query, period });
        if (level) params.append('level', level);

        const res = await fetch(`/_api/logs/search?${params}`).then(r => r.json());
        if (res.success) {
            renderLogTable(res.data);
        }
    } catch (e) {
        console.error('Log search failed:', e);
    }
}
```

Add SSE endpoint in `api.rs`:

```rust
use axum::response::sse::{Event, Sse};
use tokio_stream::StreamExt;

pub async fn stream_logs(
    State(state): State<DashboardState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let pool = state.pool.clone();

    let stream = async_stream::stream! {
        let mut last_id: Option<i64> = None;

        loop {
            let query = match last_id {
                Some(id) => sqlx::query_as::<_, LogEntry>(
                    "SELECT * FROM forge_logs WHERE id > $1 ORDER BY id ASC LIMIT 50"
                ).bind(id),
                None => sqlx::query_as::<_, LogEntry>(
                    "SELECT * FROM forge_logs ORDER BY id DESC LIMIT 1"
                ),
            };

            if let Ok(logs) = query.fetch_all(&pool).await {
                for log in logs {
                    last_id = Some(log.id.max(last_id.unwrap_or(0)));
                    let json = serde_json::to_string(&log).unwrap_or_default();
                    yield Ok(Event::default().data(json));
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
    )
}
```

Add route:
```rust
.route("/logs/stream", get(api::stream_logs))
```

---

### Step B.4: Wire Traces Page Filters

Goal: Make service, operation, duration filters work
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Filters narrow down trace results

Remove `#[allow(dead_code)]` from `TraceSearchQuery` fields and implement filtering in `list_traces()`:

```rust
// api.rs - Update list_traces()
pub async fn list_traces(
    State(state): State<DashboardState>,
    Query(query): Query<TraceSearchQuery>,
) -> Json<ApiResponse<Vec<TraceSummary>>> {
    let (start, end) = query.get_range();

    let mut sql = r#"
        WITH trace_stats AS (
            SELECT
                trace_id,
                MIN(started_at) as started_at,
                MAX(duration_ms) as duration_ms,
                COUNT(*) as span_count,
                BOOL_OR(status = 'error') as has_error,
                (array_agg(attributes->>'service.name' ORDER BY started_at ASC))[1] as service_name,
                (array_agg(name ORDER BY started_at ASC))[1] as root_span_name
            FROM forge_traces
            WHERE started_at >= $1 AND started_at <= $2
    "#.to_string();

    let mut param_idx = 3;

    // Add service filter
    if let Some(ref service) = query.service {
        sql.push_str(&format!(" AND attributes->>'service.name' ILIKE ${}", param_idx));
        param_idx += 1;
    }

    // Add operation filter
    if let Some(ref operation) = query.operation {
        sql.push_str(&format!(" AND name ILIKE ${}", param_idx));
        param_idx += 1;
    }

    sql.push_str(" GROUP BY trace_id ) SELECT * FROM trace_stats WHERE 1=1");

    // Add min_duration filter
    if let Some(min_duration) = query.min_duration {
        sql.push_str(&format!(" AND duration_ms >= ${}", param_idx));
        param_idx += 1;
    }

    // Add errors_only filter
    if query.errors_only.unwrap_or(false) {
        sql.push_str(" AND has_error = TRUE");
    }

    sql.push_str(" ORDER BY started_at DESC LIMIT $limit");

    // Build and execute query with bindings...
}
```

Add JavaScript handlers:

```javascript
// In setupEventHandlers()
const traceSearch = document.getElementById('trace-search');
const minDuration = document.getElementById('min-duration');
const errorsOnly = document.getElementById('errors-only');

[traceSearch, minDuration, errorsOnly].forEach(el => {
    if (el) {
        el.addEventListener('change', () => loadTraces());
    }
});

// Update loadTraces() to use filters
async function loadTraces() {
    const search = document.getElementById('trace-search')?.value || '';
    const minDur = document.getElementById('min-duration')?.value || '';
    const errorsOnly = document.getElementById('errors-only')?.checked || false;
    const period = getTimeRange();

    const params = new URLSearchParams({ period, limit: '50' });
    if (search) params.append('q', search);
    if (minDur) params.append('min_duration', minDur);
    if (errorsOnly) params.append('errors_only', 'true');

    try {
        const res = await fetch(`/_api/traces?${params}`).then(r => r.json());
        if (res.success) {
            renderTraceTable(res.data);
        }
    } catch (e) {
        console.error('Failed to load traces:', e);
    }
}
```

---

### Step B.5: Implement Alerts Page Data Loading

Goal: Load and display alerts, enable acknowledge/resolve
Files: `crates/forge-runtime/src/dashboard/assets.rs`
Verify: Alerts page shows real data, buttons work

Add `loadAlerts()` function and wire it in `loadPageSpecificData()`:

```javascript
// Add to loadPageSpecificData() around line 741
else if (path.includes('/alerts')) {
    loadAlerts();
}

async function loadAlerts() {
    try {
        const [alertsRes, rulesRes] = await Promise.all([
            fetch('/_api/alerts?limit=100').then(r => r.json()),
            fetch('/_api/alerts/rules').then(r => r.json()),
        ]);

        // Render active alerts tab
        const activeAlerts = (alertsRes.data || []).filter(a => a.status === 'firing');
        const resolvedAlerts = (alertsRes.data || []).filter(a => a.status === 'resolved');

        renderAlertTable('active-alerts-tbody', activeAlerts, true);
        renderAlertTable('alert-history-tbody', resolvedAlerts, false);
        renderRulesTable('alert-rules-tbody', rulesRes.data || []);

        // Update stats
        updateElement('alerts-active', activeAlerts.length);
        updateElement('alerts-critical', activeAlerts.filter(a => a.severity === 'critical').length);
        updateElement('alerts-warning', activeAlerts.filter(a => a.severity === 'warning').length);

    } catch (e) {
        console.error('Failed to load alerts:', e);
    }
}

function renderAlertTable(tbodyId, alerts, showActions) {
    const tbody = document.getElementById(tbodyId);
    if (!tbody) return;

    if (alerts.length === 0) {
        tbody.innerHTML = '<tr class="empty-row"><td colspan="6">No alerts</td></tr>';
        return;
    }

    tbody.innerHTML = alerts.map(alert => `
        <tr class="alert-row ${alert.severity}">
            <td><span class="severity-badge ${alert.severity}">${alert.severity}</span></td>
            <td>${escapeHtml(alert.rule_name)}</td>
            <td>${alert.metric_value?.toFixed(2) || '-'} ${alert.threshold ? `(threshold: ${alert.threshold})` : ''}</td>
            <td>${formatRelativeTime(alert.triggered_at)}</td>
            <td>${alert.acknowledged_by ? `✓ ${escapeHtml(alert.acknowledged_by)}` : '-'}</td>
            ${showActions ? `
                <td>
                    ${!alert.acknowledged_at ? `<button onclick="acknowledgeAlert('${alert.id}')" class="btn btn-sm">Ack</button>` : ''}
                    <button onclick="resolveAlert('${alert.id}')" class="btn btn-sm btn-danger">Resolve</button>
                </td>
            ` : '<td>-</td>'}
        </tr>
    `).join('');
}

async function acknowledgeAlert(id) {
    const name = prompt('Your name for acknowledgment:');
    if (!name) return;

    try {
        await fetch(`/_api/alerts/${id}/acknowledge`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ acknowledged_by: name })
        });
        loadAlerts();
    } catch (e) {
        alert('Failed to acknowledge alert');
    }
}

async function resolveAlert(id) {
    if (!confirm('Resolve this alert?')) return;

    try {
        await fetch(`/_api/alerts/${id}/resolve`, { method: 'POST' });
        loadAlerts();
    } catch (e) {
        alert('Failed to resolve alert');
    }
}
```

---

### Step B.6: Implement Jobs Page Tab Switching and Retry

Goal: Tab switching filters jobs, retry button works
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Clicking tabs filters jobs, retry re-queues failed jobs

Add job detail and retry endpoints:

```rust
// api.rs
pub async fn get_job(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<JobDetail>> {
    let job = sqlx::query_as::<_, JobRecord>(
        "SELECT * FROM forge_jobs WHERE id = $1"
    )
    .bind(Uuid::parse_str(&id)?)
    .fetch_optional(&state.pool)
    .await?;

    // Return job with full input/output JSON
}

pub async fn retry_job(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    let job_id = Uuid::parse_str(&id)?;

    // Reset job to pending status
    sqlx::query(
        r#"
        UPDATE forge_jobs
        SET status = 'pending',
            attempts = 0,
            last_error = NULL,
            scheduled_at = NOW()
        WHERE id = $1 AND status IN ('failed', 'dead_letter')
        "#
    )
    .bind(job_id)
    .execute(&state.pool)
    .await?;

    Json(ApiResponse::success(()))
}
```

Add routes:
```rust
.route("/jobs/{id}", get(api::get_job))
.route("/jobs/{id}/retry", post(api::retry_job))
```

Add JavaScript handlers:

```javascript
// Job tab switching
document.querySelectorAll('.jobs-tab').forEach(tab => {
    tab.addEventListener('click', (e) => {
        document.querySelectorAll('.jobs-tab').forEach(t => t.classList.remove('active'));
        e.target.classList.add('active');

        const status = e.target.dataset.status;
        loadJobs(status);
    });
});

async function loadJobs(statusFilter = null) {
    const params = new URLSearchParams({ limit: '50' });
    if (statusFilter) params.append('status', statusFilter);

    try {
        const [jobsRes, statsRes] = await Promise.all([
            fetch(`/_api/jobs?${params}`).then(r => r.json()),
            fetch('/_api/jobs/stats').then(r => r.json()),
        ]);

        renderJobTable(jobsRes.data || []);
        updateJobStats(statsRes.data);
    } catch (e) {
        console.error('Failed to load jobs:', e);
    }
}

function renderJobTable(jobs) {
    const tbody = document.getElementById('jobs-tbody');
    if (!tbody) return;

    tbody.innerHTML = jobs.map(job => `
        <tr>
            <td>${escapeHtml(job.job_type)}</td>
            <td><span class="status-badge status-${job.status}">${job.status}</span></td>
            <td>${job.priority}</td>
            <td>${job.attempts}/${job.max_attempts}</td>
            <td>${formatRelativeTime(job.created_at)}</td>
            <td>${job.last_error ? `<span class="error-text">${escapeHtml(job.last_error.substring(0, 50))}...</span>` : '-'}</td>
            <td>
                ${['failed', 'dead_letter'].includes(job.status) ?
                    `<button onclick="retryJob('${job.id}')" class="btn btn-sm">Retry</button>` : ''}
                <button onclick="viewJobDetail('${job.id}')" class="btn btn-sm">View</button>
            </td>
        </tr>
    `).join('');
}

async function retryJob(id) {
    if (!confirm('Retry this job?')) return;

    try {
        await fetch(`/_api/jobs/${id}/retry`, { method: 'POST' });
        loadJobs();
    } catch (e) {
        alert('Failed to retry job');
    }
}
```

---

### Step B.7: Implement Workflows Page Detail View

Goal: View workflow details including steps and compensation
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Clicking workflow shows all steps with status

Add workflow detail endpoint:

```rust
// api.rs
#[derive(Serialize)]
pub struct WorkflowDetail {
    pub id: String,
    pub workflow_name: String,
    pub version: Option<u32>,
    pub status: String,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub current_step: Option<String>,
    pub steps: Vec<WorkflowStepDetail>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct WorkflowStepDetail {
    pub step_name: String,
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
}

pub async fn get_workflow(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ApiResponse<WorkflowDetail>> {
    let workflow_id = Uuid::parse_str(&id)?;

    let run = sqlx::query_as::<_, WorkflowRecord>(
        "SELECT * FROM forge_workflow_runs WHERE id = $1"
    )
    .bind(workflow_id)
    .fetch_one(&state.pool)
    .await?;

    let steps = sqlx::query_as::<_, WorkflowStepRecord>(
        "SELECT * FROM forge_workflow_steps WHERE workflow_run_id = $1 ORDER BY started_at ASC"
    )
    .bind(workflow_id)
    .fetch_all(&state.pool)
    .await?;

    // Combine and return
}
```

Add route:
```rust
.route("/workflows/{id}", get(api::get_workflow))
```

---

### Step B.8: Implement Crons Page with Full Functionality

Goal: Load crons, show history, enable pause/resume/trigger
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Crons list loads, buttons trigger actions

Add `loadCrons()` function:

```javascript
// Add to loadPageSpecificData()
else if (path.includes('/crons')) {
    loadCrons();
}

async function loadCrons() {
    try {
        const [cronsRes, statsRes, historyRes] = await Promise.all([
            fetch('/_api/crons').then(r => r.json()),
            fetch('/_api/crons/stats').then(r => r.json()),
            fetch('/_api/crons/history?limit=50').then(r => r.json()),
        ]);

        renderCronTable(cronsRes.data || []);
        renderCronHistory(historyRes.data || []);
        updateCronStats(statsRes.data);

    } catch (e) {
        console.error('Failed to load crons:', e);
    }
}

function renderCronTable(crons) {
    const tbody = document.getElementById('crons-tbody');
    if (!tbody) return;

    tbody.innerHTML = crons.map(cron => `
        <tr>
            <td>${escapeHtml(cron.name)}</td>
            <td><code>${escapeHtml(cron.schedule)}</code></td>
            <td><span class="status-badge status-${cron.status}">${cron.status}</span></td>
            <td>${cron.last_run ? formatRelativeTime(cron.last_run) : 'Never'}</td>
            <td>${cron.next_run ? formatTime(cron.next_run) : '-'}</td>
            <td>${cron.avg_duration_ms ? cron.avg_duration_ms + 'ms' : '-'}</td>
            <td>
                <button onclick="triggerCron('${cron.name}')" class="btn btn-sm">Trigger</button>
                ${cron.status === 'active' ?
                    `<button onclick="pauseCron('${cron.name}')" class="btn btn-sm">Pause</button>` :
                    `<button onclick="resumeCron('${cron.name}')" class="btn btn-sm">Resume</button>`
                }
            </td>
        </tr>
    `).join('');
}

async function triggerCron(name) {
    try {
        await fetch(`/_api/crons/${encodeURIComponent(name)}/trigger`, { method: 'POST' });
        loadCrons();
    } catch (e) {
        alert('Failed to trigger cron');
    }
}

async function pauseCron(name) {
    try {
        await fetch(`/_api/crons/${encodeURIComponent(name)}/pause`, { method: 'POST' });
        loadCrons();
    } catch (e) {
        alert('Failed to pause cron');
    }
}

async function resumeCron(name) {
    try {
        await fetch(`/_api/crons/${encodeURIComponent(name)}/resume`, { method: 'POST' });
        loadCrons();
    } catch (e) {
        alert('Failed to resume cron');
    }
}
```

Fix trigger endpoint to actually dispatch cron (in `api.rs`):

```rust
pub async fn trigger_cron(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> Json<ApiResponse<()>> {
    // Insert a cron run record scheduled for NOW
    sqlx::query(
        r#"
        INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, timezone, status, started_at)
        VALUES ($1, $2, NOW(), 'UTC', 'pending', NULL)
        "#
    )
    .bind(Uuid::new_v4())
    .bind(&name)
    .execute(&state.pool)
    .await?;

    tracing::info!(cron_name = %name, "Cron manually triggered via dashboard");

    Json(ApiResponse::success(()))
}
```

---

### Step B.9: Enhance Cluster Page Leadership Display

Goal: Show leadership roles with lease info, node capabilities
Files: `crates/forge-runtime/src/dashboard/assets.rs`, `crates/forge-runtime/src/dashboard/api.rs`
Verify: Leadership table shows lease expiry, nodes show capabilities

Extend ClusterHealth response:

```rust
// api.rs
#[derive(Serialize)]
pub struct LeaderInfo {
    pub role: String,
    pub node_id: String,
    pub node_name: Option<String>,
    pub acquired_at: DateTime<Utc>,
    pub lease_until: DateTime<Utc>,
    pub is_healthy: bool,
}

pub async fn get_cluster_health(
    State(state): State<DashboardState>,
) -> Json<ApiResponse<ClusterHealth>> {
    // Fetch leaders with full info
    let leaders = sqlx::query(
        r#"
        SELECT l.role, l.node_id, l.acquired_at, l.lease_until,
               n.hostname as node_name,
               l.lease_until > NOW() as is_healthy
        FROM forge_leaders l
        LEFT JOIN forge_nodes n ON n.id = l.node_id
        "#
    )
    .fetch_all(&state.pool)
    .await?;

    // Fetch nodes with capabilities
    let nodes = sqlx::query(
        r#"
        SELECT id, hostname, ip_address, status, roles, worker_capabilities,
               current_connections, current_jobs, cpu_usage, memory_usage,
               last_heartbeat, version, started_at
        FROM forge_nodes
        WHERE status != 'dead'
        ORDER BY started_at ASC
        "#
    )
    .fetch_all(&state.pool)
    .await?;

    // Include capabilities and metrics in response
}
```

Update JavaScript to display enhanced info:

```javascript
function updateClusterLeaders(leaders) {
    const tbody = document.getElementById('leaders-tbody');
    if (!tbody) return;

    tbody.innerHTML = leaders.map(l => `
        <tr class="${l.is_healthy ? '' : 'unhealthy'}">
            <td><span class="role-badge ${l.role}">${l.role}</span></td>
            <td>${escapeHtml(l.node_name || l.node_id.substring(0, 8))}</td>
            <td>${formatRelativeTime(l.acquired_at)}</td>
            <td class="${l.is_healthy ? 'healthy' : 'expired'}">
                ${l.is_healthy ? formatRelativeTime(l.lease_until) : 'EXPIRED'}
            </td>
            <td>${l.is_healthy ? '✓ Healthy' : '⚠ Unhealthy'}</td>
        </tr>
    `).join('');
}

function updateClusterNodes(nodes, leaders) {
    const container = document.getElementById('nodes-grid');
    if (!container) return;

    const leaderNodeIds = new Set(leaders.map(l => l.node_id));

    container.innerHTML = nodes.map(node => `
        <div class="node-card ${leaderNodeIds.has(node.id) ? 'leader' : ''} ${node.status}">
            <div class="node-header">
                <h4>${escapeHtml(node.hostname || node.id.substring(0, 8))}</h4>
                ${leaderNodeIds.has(node.id) ? '<span class="leader-badge">Leader</span>' : ''}
            </div>
            <div class="node-status">
                <span class="status-badge status-${node.status}">${node.status}</span>
            </div>
            <div class="node-roles">
                ${(node.roles || []).map(r => `<span class="role-tag">${r}</span>`).join('')}
            </div>
            <div class="node-capabilities">
                <strong>Capabilities:</strong>
                ${(node.worker_capabilities || []).map(c => `<span class="cap-tag">${c}</span>`).join('') || 'None'}
            </div>
            <div class="node-metrics">
                <div class="metric">CPU: ${node.cpu_usage?.toFixed(1) || 0}%</div>
                <div class="metric">Memory: ${node.memory_usage?.toFixed(1) || 0}%</div>
                <div class="metric">Connections: ${node.current_connections || 0}</div>
                <div class="metric">Jobs: ${node.current_jobs || 0}</div>
            </div>
            <div class="node-footer">
                <small>Last heartbeat: ${formatRelativeTime(node.last_heartbeat)}</small>
            </div>
        </div>
    `).join('');
}
```

---

### Step B.10: Add Dashboard CSS for New Components

Goal: Style all new dashboard components
Files: `crates/forge-runtime/src/dashboard/assets.rs`
Verify: All new UI components render properly styled

Add CSS (append to `styles_css()`):

```css
/* Leader badges */
.leader-badge {
    background: #3b82f6;
    color: white;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 12px;
}

.role-badge {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 12px;
}
.role-badge.scheduler { background: #f59e0b; color: white; }
.role-badge.metrics_aggregator { background: #10b981; color: white; }
.role-badge.log_compactor { background: #8b5cf6; color: white; }

/* Node cards with leader highlight */
.node-card.leader {
    border: 2px solid #3b82f6;
    box-shadow: 0 0 10px rgba(59, 130, 246, 0.3);
}

/* Capabilities tags */
.cap-tag, .role-tag {
    display: inline-block;
    background: #374151;
    color: #9ca3af;
    padding: 2px 6px;
    border-radius: 3px;
    font-size: 11px;
    margin: 2px;
}

/* Severity badges */
.severity-badge {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 12px;
    font-weight: 500;
}
.severity-badge.critical { background: #ef4444; color: white; }
.severity-badge.warning { background: #f59e0b; color: white; }
.severity-badge.info { background: #3b82f6; color: white; }

/* Streaming button */
.btn.streaming {
    background: #ef4444;
    animation: pulse 1s infinite;
}

@keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.7; }
}

/* Metric card selected state */
.metric-card.selected {
    border: 2px solid #3b82f6;
    background: #1e293b;
}

/* Log level badges */
.log-level {
    padding: 2px 6px;
    border-radius: 3px;
    font-size: 11px;
}
.log-level.error { background: #ef4444; color: white; }
.log-level.warn { background: #f59e0b; color: white; }
.log-level.info { background: #3b82f6; color: white; }
.log-level.debug { background: #6b7280; color: white; }

/* Unhealthy row styling */
tr.unhealthy {
    background: rgba(239, 68, 68, 0.1);
}

.expired {
    color: #ef4444;
    font-weight: bold;
}

.healthy {
    color: #10b981;
}
```

---

## Part C: Generated Code Fixes

Goal: Fix all issues with CLI code generation

---

### Step C.1: Fix .gitignore Template

Goal: Include all necessary ignores including .svelte-kit
Files: `crates/forge/src/cli/new.rs`
Verify: Generated .gitignore includes .svelte-kit

Update gitignore template (around line 302-306):

```rust
let gitignore = r#"# Rust
/target
Cargo.lock

# Node/Frontend
node_modules/
.svelte-kit/
/frontend/dist
/frontend/build

# Environment
.env
.env.local
.env.*.local

# IDE
.vscode/
.idea/
*.swp
*.swo

# OS
.DS_Store
Thumbs.db

# Logs
*.log
npm-debug.log*
"#;
```

---

### Step C.2: Add Environment Variable Support for ForgeProvider URL

Goal: Use VITE_API_URL from .env instead of hardcoded URL
Files: `crates/forge/src/cli/new.rs`
Verify: Generated +layout.svelte reads from env

Update +layout.svelte template (around line 420-430):

```rust
let layout_svelte = r#"<script lang="ts">
    import { ForgeProvider } from '$lib/forge/runtime';
    import '../app.css';

    interface Props {
        children: import('svelte').Snippet;
    }

    let { children }: Props = $props();

    // Read API URL from environment or use default
    const apiUrl = import.meta.env.VITE_API_URL || 'http://localhost:8080';
</script>

<ForgeProvider url={apiUrl}>
    {@render children()}
</ForgeProvider>
"#;
```

Update +page.svelte to also use env (around line 507):

```rust
// Replace hardcoded localhost links
let page_svelte = r#"<script lang="ts">
    // ... existing code ...
    const apiUrl = import.meta.env.VITE_API_URL || 'http://localhost:8080';
</script>

<!-- In template -->
<a href="{apiUrl}/health" target="_blank">API Health</a>
<a href="{apiUrl}/_dashboard" target="_blank">Dashboard</a>
"#;
```

Generate .env.example file:

```rust
// Add to create_frontend()
let env_example = r#"# FORGE Environment Variables

# API URL for the FORGE backend
VITE_API_URL=http://localhost:8080

# Add your environment-specific variables below
"#;

fs::write(frontend_dir.join(".env.example"), env_example)?;
```

---

### Step C.3: Fix Duplicate mod.rs Entries

Goal: Check for existing entries before appending
Files: `crates/forge/src/cli/add.rs`
Verify: Running `forge add` twice doesn't create duplicates

Update `update_schema_mod()` and `update_functions_mod()` (lines 548-573):

```rust
fn update_schema_mod(snake_name: &str, pascal_name: &str) -> Result<()> {
    let mod_path = Path::new("src/schema/mod.rs");
    let content = fs::read_to_string(mod_path).unwrap_or_default();

    let mod_decl = format!("pub mod {};", snake_name);
    let use_decl = format!("pub use {}::{};", snake_name, pascal_name);

    // Check if already exists
    if content.contains(&mod_decl) {
        println!("  {} already declared in mod.rs", snake_name);
        return Ok(());
    }

    // Build new content without extra blank lines
    let mut new_content = content.trim_end().to_string();
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(&mod_decl);
    new_content.push('\n');
    new_content.push_str(&use_decl);
    new_content.push('\n');

    fs::write(mod_path, new_content)?;
    Ok(())
}

fn update_functions_mod(snake_name: &str) -> Result<()> {
    let mod_path = Path::new("src/functions/mod.rs");
    let content = fs::read_to_string(mod_path).unwrap_or_default();

    let mod_decl = format!("pub mod {};", snake_name);

    // Check if already exists
    if content.contains(&mod_decl) {
        println!("  {} already declared in mod.rs", snake_name);
        return Ok(());
    }

    // Append without extra blank lines
    let mut new_content = content.trim_end().to_string();
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(&mod_decl);
    new_content.push('\n');

    fs::write(mod_path, new_content)?;
    Ok(())
}
```

---

### Step C.4: Extend Parser to Recognize Jobs, Crons, Workflows

Goal: Make `forge generate` type-check jobs, crons, workflows
Files: `crates/forge-codegen/src/parser.rs`
Verify: Generated types.ts includes job/cron/workflow types

Extend `get_function_kind()` (around line 182-254):

```rust
fn get_function_kind(attrs: &[Attribute]) -> Option<FunctionKind> {
    for attr in attrs {
        let path = &attr.path();
        let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();

        // Check for #[forge::X] or #[X] patterns
        let kind_str = if segments.len() == 2 && segments[0] == "forge" {
            Some(segments[1].as_str())
        } else if segments.len() == 1 {
            Some(segments[0].as_str())
        } else {
            None
        };

        if let Some(kind) = kind_str {
            match kind {
                "query" => return Some(FunctionKind::Query),
                "mutation" => return Some(FunctionKind::Mutation),
                "action" => return Some(FunctionKind::Action),
                "job" => return Some(FunctionKind::Job),
                "cron" => return Some(FunctionKind::Cron),
                "workflow" => return Some(FunctionKind::Workflow),
                _ => {}
            }
        }
    }
    None
}

// Add new FunctionKind variants
#[derive(Debug, Clone)]
pub enum FunctionKind {
    Query,
    Mutation,
    Action,
    Job,
    Cron,
    Workflow,
}
```

Update TypeScript generation to include job/cron/workflow types:

```rust
// In typescript/api.rs
impl ApiGenerator {
    pub fn generate(&self, functions: &[FunctionDef]) -> String {
        let mut output = String::from("// Auto-generated by FORGE - DO NOT EDIT\n\n");

        // Generate function bindings by kind
        for func in functions {
            match func.kind {
                FunctionKind::Query => {
                    output.push_str(&self.generate_query(func));
                }
                FunctionKind::Mutation => {
                    output.push_str(&self.generate_mutation(func));
                }
                FunctionKind::Action => {
                    output.push_str(&self.generate_action(func));
                }
                FunctionKind::Job => {
                    output.push_str(&self.generate_job(func));
                }
                FunctionKind::Cron => {
                    // Crons don't need client bindings
                }
                FunctionKind::Workflow => {
                    output.push_str(&self.generate_workflow(func));
                }
            }
        }

        output
    }

    fn generate_job(&self, func: &FunctionDef) -> String {
        format!(
            "export const {} = createJob<{}, {}>('{}');\n",
            to_camel_case(&func.name),
            self.format_args_type(&func.args),
            self.format_return_type(&func.return_type),
            func.name
        )
    }

    fn generate_workflow(&self, func: &FunctionDef) -> String {
        format!(
            "export const {} = createWorkflow<{}, {}>('{}');\n",
            to_camel_case(&func.name),
            self.format_args_type(&func.args),
            self.format_return_type(&func.return_type),
            func.name
        )
    }
}
```

---

### Step C.5: Fix Delete Subscription UI Handling

Goal: Generated +page.svelte properly handles empty lists after delete
Files: `crates/forge/src/cli/new.rs`
Verify: Deleting all users shows "No users" instead of blank

Update the +page.svelte template user list section:

```rust
let page_svelte = r#"
<!-- User list section -->
{#if $users.loading}
    <div class="loading">Loading users...</div>
{:else if $users.error}
    <div class="error">Error: {$users.error.message}</div>
{:else if $users.data && $users.data.length > 0}
    <div class="user-list">
        {#each $users.data as user (user.id)}
            <div class="user-card">
                <h3>{user.name}</h3>
                <p>{user.email}</p>
                <button onclick={() => handleDelete(user.id)}>Delete</button>
            </div>
        {/each}
    </div>
{:else}
    <div class="empty-state">
        <p>No users found. Create one above!</p>
    </div>
{/if}
"#;
```

---

### Step C.6: Add Sample Cron, Job, Workflow, Alert to Generated Project

Goal: Generated project demonstrates all FORGE features
Files: `crates/forge/src/cli/new.rs`
Verify: New project includes working examples of each feature

Add sample files during project creation:

```rust
// In create_functions()

// Sample job
let send_welcome_email = r#"//! Background job: Send welcome email
//! Auto-generated by FORGE - DO NOT EDIT the structure, customize the logic

use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendWelcomeEmailInput {
    pub user_id: String,
    pub email: String,
}

#[forge::job]
#[max_attempts = 3]
#[timeout = "5m"]
pub async fn send_welcome_email(
    ctx: &JobContext,
    input: SendWelcomeEmailInput,
) -> Result<()> {
    tracing::info!(user_id = %input.user_id, "Sending welcome email");

    // TODO: Integrate with your email provider
    // Example: ctx.http().post("https://api.sendgrid.com/...").send().await?;

    tracing::info!(user_id = %input.user_id, "Welcome email sent");
    Ok(())
}
"#;

// Sample cron
let daily_cleanup = r#"//! Scheduled task: Daily cleanup
//! Auto-generated by FORGE - DO NOT EDIT the structure, customize the logic

use forge::prelude::*;

#[forge::cron("0 0 * * *")]  // Daily at midnight UTC
#[timeout = "30m"]
pub async fn daily_cleanup(ctx: &CronContext) -> Result<()> {
    tracing::info!(run_id = %ctx.run_id, "Starting daily cleanup");

    // Example: Delete old sessions
    let deleted = sqlx::query("DELETE FROM sessions WHERE created_at < NOW() - INTERVAL '7 days'")
        .execute(ctx.db())
        .await?
        .rows_affected();

    tracing::info!(run_id = %ctx.run_id, deleted = %deleted, "Cleanup complete");
    Ok(())
}
"#;

// Sample workflow
let user_onboarding = r#"//! Durable workflow: User onboarding
//! Auto-generated by FORGE - DO NOT EDIT the structure, customize the logic

use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingInput {
    pub user_id: String,
    pub email: String,
    pub plan: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingOutput {
    pub success: bool,
    pub welcome_email_sent: bool,
}

#[forge::workflow]
#[timeout = "1h"]
pub async fn user_onboarding(
    ctx: &WorkflowContext,
    input: OnboardingInput,
) -> Result<OnboardingOutput> {
    // Step 1: Initialize user profile
    ctx.step("init_profile")
        .run(|| async {
            tracing::info!(user_id = %input.user_id, "Initializing profile");
            Ok(())
        })
        .compensate(|_| async {
            tracing::info!(user_id = %input.user_id, "Rolling back profile");
            Ok(())
        })
        .await?;

    // Step 2: Send welcome email (optional - won't trigger compensation on failure)
    let email_sent = ctx.step("send_welcome")
        .run(|| async {
            // Dispatch job instead of blocking
            // ctx.dispatch_job::<SendWelcomeEmailJob>(input).await?;
            Ok(true)
        })
        .optional()
        .await
        .unwrap_or(false);

    Ok(OnboardingOutput {
        success: true,
        welcome_email_sent: email_sent,
    })
}
"#;

fs::write(functions_dir.join("send_welcome_email_job.rs"), send_welcome_email)?;
fs::write(functions_dir.join("daily_cleanup_cron.rs"), daily_cleanup)?;
fs::write(functions_dir.join("user_onboarding_workflow.rs"), user_onboarding)?;
```

Update main.rs to register all components:

```rust
let main_rs = r#"use forge::prelude::*;

mod schema;
mod functions;

use functions::{
    get_users::GetUsersQuery,
    create_user::CreateUserMutation,
    update_user::UpdateUserMutation,
    delete_user::DeleteUserMutation,
    send_welcome_email_job::SendWelcomeEmailJob,
    daily_cleanup_cron::DailyCleanupCron,
    user_onboarding_workflow::UserOnboardingWorkflow,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let config = ForgeConfig::from_env()?;
    let mut forge = Forge::builder();

    // Register queries and mutations
    forge.function_registry_mut().register_query::<GetUsersQuery>();
    forge.function_registry_mut().register_mutation::<CreateUserMutation>();
    forge.function_registry_mut().register_mutation::<UpdateUserMutation>();
    forge.function_registry_mut().register_mutation::<DeleteUserMutation>();

    // Register background jobs
    forge.job_registry_mut().register::<SendWelcomeEmailJob>();

    // Register cron jobs
    forge.cron_registry_mut().register::<DailyCleanupCron>();

    // Register workflows
    forge.workflow_registry_mut().register::<UserOnboardingWorkflow>();

    forge.config(config).build()?.run().await
}
"#;
```

Add sample alert creation to migrations:

```sql
-- In generated 0001_create_users.sql, add:

-- Sample alert rule: Notify when user count exceeds threshold
INSERT INTO forge_alert_rules (id, name, description, metric_name, condition, threshold, severity, enabled, cooldown_seconds)
VALUES (
    gen_random_uuid(),
    'high_user_count',
    'Alert when active user count exceeds 100',
    'active_user_count',
    'gt',
    100,
    'warning',
    true,
    300
) ON CONFLICT (name) DO NOTHING;
```

---

### Final Step: Cleanup & Validation

Goal: Run formatters and linters, ensure tests pass
Verify: `cargo fmt && cargo clippy && cargo test` passes

```bash
# Format all code
cargo fmt --all

# Run clippy
cargo clippy --all-targets --all-features -- -D warnings

# Run tests
LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo test

# Verify generated project works
cd /tmp && forge new test-project && cd test-project
cargo build
bun install && bun run build
```

---

---

## Part D: End-to-End Testing with Chrome MCP

Goal: Validate all fixes work correctly through browser-based testing

---

### Step D.1: Generate Fresh Test Application

Goal: Create a clean test app to validate all fixes
Files: N/A (generates new project)
Verify: Project compiles and runs without errors

```bash
# Create test app in temp directory
cd /tmp
rm -rf forge-e2e-test
forge new forge-e2e-test

# Verify project structure
cd forge-e2e-test
ls -la src/functions/  # Should include job, cron, workflow samples

# Build and start backend
cargo build
cargo run &
BACKEND_PID=$!

# Build and start frontend
cd frontend
bun install
echo "VITE_API_URL=http://localhost:8080" > .env
bun run dev &
FRONTEND_PID=$!

# Wait for services
sleep 5
```

---

### Step D.2: Test User CRUD with Chrome MCP

Goal: Verify create, read, update, delete work with real-time updates
Files: N/A (browser testing)
Verify: All CRUD operations reflect immediately in UI

**Test Sequence using Chrome MCP:**

```
1. Navigate to http://localhost:5173 (frontend)

2. CREATE Test:
   - Fill in user form (name: "Test User", email: "test@example.com")
   - Click Create button
   - VERIFY: New user appears in list immediately (subscription update)

3. READ Test:
   - Refresh page
   - VERIFY: User list loads correctly
   - VERIFY: Loading state shows briefly, then data appears

4. UPDATE Test (if implemented):
   - Click edit on user
   - Change name to "Updated User"
   - Save
   - VERIFY: Change reflects immediately in list

5. DELETE Test:
   - Click delete on user
   - Confirm deletion
   - VERIFY: User disappears from list immediately
   - VERIFY: If last user deleted, "No users" message shows

6. MULTIPLE USERS Test:
   - Create 5 users rapidly
   - VERIFY: All appear in correct order
   - Delete all users one by one
   - VERIFY: List updates after each deletion
   - VERIFY: Empty state shows after last deletion
```

---

### Step D.3: Test Dashboard Observability

Goal: Verify all dashboard pages work correctly
Files: N/A (browser testing)
Verify: Charts interactive, filters work, data loads

**Test Sequence using Chrome MCP:**

```
1. Navigate to http://localhost:8080/_dashboard

2. OVERVIEW Page:
   - VERIFY: Request Rate chart renders with data
   - VERIFY: Hover shows tooltip with values
   - VERIFY: Scroll wheel zooms chart
   - VERIFY: Stats cards show real numbers

3. METRICS Page:
   - VERIFY: Metric cards display
   - Type in search box "http"
   - VERIFY: Only HTTP-related metrics shown
   - Select "Counter" from type dropdown
   - VERIFY: Only counters shown
   - Click on a metric card
   - VERIFY: Detail chart appears with time series

4. LOGS Page:
   - VERIFY: Logs table loads with entries
   - Type "POST" in search box
   - VERIFY: Only logs containing POST shown
   - Select "error" from level dropdown
   - VERIFY: Only error logs shown
   - Click "Live Stream" button
   - VERIFY: Button changes to "Stop Stream"
   - Make API request in another tab
   - VERIFY: New log appears without refresh

5. TRACES Page:
   - VERIFY: Traces table loads
   - Check "Errors only" checkbox
   - VERIFY: Only error traces shown
   - Enter minimum duration (e.g., 10)
   - VERIFY: Only slow traces shown
   - Click on a trace ID
   - VERIFY: Trace detail view shows waterfall

6. ALERTS Page:
   - VERIFY: Alert tabs render (Active, History, Rules)
   - Click "Alert Rules" tab
   - VERIFY: Sample alert rule shows
   - If any alerts firing, click "Ack" button
   - VERIFY: Acknowledged by field updates

7. JOBS Page:
   - VERIFY: Job stats display (may be 0 initially)
   - Click different status tabs
   - VERIFY: Table filters by status
   - If failed jobs exist, click "Retry"
   - VERIFY: Job moves back to pending

8. WORKFLOWS Page:
   - VERIFY: Workflow list loads
   - Click on a workflow run (if any)
   - VERIFY: Detail view shows steps

9. CRONS Page:
   - VERIFY: Cron list shows daily_cleanup
   - Click "Trigger" button
   - VERIFY: Cron execution starts
   - Click "Pause" button
   - VERIFY: Status changes to paused
   - Click "Resume" button
   - VERIFY: Status changes to active

10. CLUSTER Page:
    - VERIFY: Current node shows in nodes grid
    - VERIFY: Leadership table shows roles
    - VERIFY: Node capabilities display
    - VERIFY: Health metrics show (CPU, memory)
```

---

### Step D.4: Test Real-Time Subscriptions

Goal: Verify WebSocket subscriptions update correctly
Files: N/A (browser testing)
Verify: Multiple browser tabs stay in sync

**Test Sequence using Chrome MCP:**

```
1. Open two browser tabs to http://localhost:5173

2. TAB SYNC Test:
   - In Tab 1: Create a new user
   - VERIFY: Tab 2 shows new user immediately (no refresh)

   - In Tab 2: Delete a user
   - VERIFY: Tab 1 removes user immediately

3. RECONNECTION Test:
   - Stop the backend (kill cargo run)
   - VERIFY: Frontend shows connection error/reconnecting state
   - Restart backend
   - VERIFY: Connection restores automatically
   - VERIFY: Data refreshes after reconnect

4. RAPID UPDATES Test:
   - Open browser console
   - Create 10 users rapidly via API:
     for i in {1..10}; do
       curl -X POST http://localhost:8080/rpc/create_user \
         -H "Content-Type: application/json" \
         -d "{\"args\":{\"name\":\"User $i\",\"email\":\"user$i@test.com\"}}"
     done
   - VERIFY: All users appear in UI (may batch updates)
```

---

### Step D.5: Test Generated Code Quality

Goal: Verify generated TypeScript types and client work correctly
Files: N/A (browser testing)
Verify: Type safety, no runtime errors

**Test Sequence:**

```
1. Regenerate types:
   cd /tmp/forge-e2e-test
   forge generate --force

2. Check for TypeScript errors:
   cd frontend
   bun run check
   # Should pass with 0 errors

3. Verify generated files have headers:
   head -1 src/lib/forge/types.ts
   # Should show: // Auto-generated by FORGE - DO NOT EDIT

4. Verify .gitignore:
   cat .gitignore | grep svelte-kit
   # Should show: .svelte-kit/

5. Verify .env support:
   cat .env
   # Should show: VITE_API_URL=...
```

---

### Step D.6: Test CLI Add Commands

Goal: Verify all `forge add` commands work correctly
Files: N/A (CLI testing)
Verify: Components generated, mod.rs updated without duplicates

**Test Sequence:**

```
1. Test add commands:
   cd /tmp/forge-e2e-test

   # Add each component type
   forge add model Customer
   forge add query get_customers
   forge add mutation create_customer
   forge add action sync_inventory
   forge add job process_order_job
   forge add cron weekly_report_cron
   forge add workflow checkout_workflow

2. Verify no duplicates:
   forge add model Customer  # Run again
   # Should show: "Customer already declared in mod.rs"
   grep -c "pub mod customer" src/schema/mod.rs
   # Should show: 1 (not 2)

3. Verify project still compiles:
   cargo check
   # Should pass

4. Verify types regenerate:
   forge generate
   grep "Customer" frontend/src/lib/forge/types.ts
   # Should find Customer interface
```

---

### Step D.7: Cleanup Test Applications

Goal: Remove test applications after validation
Files: N/A
Verify: Clean state restored

```bash
# Stop running processes
kill $BACKEND_PID $FRONTEND_PID 2>/dev/null

# Remove test app
rm -rf /tmp/forge-e2e-test

# Verify cleanup
ls /tmp | grep forge
# Should be empty or show no test apps
```

---

### Test Automation Script

Create a script to run all tests:

```bash
#!/bin/bash
# scripts/e2e-test.sh

set -e

echo "=== FORGE E2E Test Suite ==="

# Create test app
echo "Creating test application..."
cd /tmp
rm -rf forge-e2e-test
forge new forge-e2e-test
cd forge-e2e-test

# Build backend
echo "Building backend..."
cargo build --release

# Start backend
echo "Starting backend..."
./target/release/forge-e2e-test &
BACKEND_PID=$!
sleep 3

# Test health endpoint
echo "Testing health endpoint..."
curl -s http://localhost:8080/health | grep -q "ok" || exit 1

# Test RPC endpoints
echo "Testing RPC endpoints..."
curl -s -X POST http://localhost:8080/rpc/get_users \
  -H "Content-Type: application/json" \
  -d '{}' | grep -q "success" || exit 1

# Create user
echo "Testing create user..."
RESPONSE=$(curl -s -X POST http://localhost:8080/rpc/create_user \
  -H "Content-Type: application/json" \
  -d '{"args":{"name":"E2E Test","email":"e2e@test.com"}}')
echo $RESPONSE | grep -q "success" || exit 1
USER_ID=$(echo $RESPONSE | jq -r '.data.id')

# Delete user
echo "Testing delete user..."
curl -s -X POST http://localhost:8080/rpc/delete_user \
  -H "Content-Type: application/json" \
  -d "{\"args\":{\"id\":\"$USER_ID\"}}" | grep -q "success" || exit 1

# Test dashboard
echo "Testing dashboard..."
curl -s http://localhost:8080/_dashboard | grep -q "FORGE Dashboard" || exit 1

# Test metrics API
echo "Testing metrics API..."
curl -s http://localhost:8080/_api/metrics | grep -q "success" || exit 1

# Cleanup
echo "Cleaning up..."
kill $BACKEND_PID 2>/dev/null
rm -rf /tmp/forge-e2e-test

echo "=== All E2E Tests Passed ==="
```

---

## Summary

| Part | Steps | Estimated Files Changed |
|------|-------|------------------------|
| **A: Runtime Versioning** | 6 steps | ~15 new files, ~5 modified |
| **B: Dashboard Fixes** | 10 steps | 4 files (assets.rs, api.rs, pages.rs, mod.rs) |
| **C: Generated Code** | 6 steps | 3 files (new.rs, add.rs, parser.rs) |
| **D: E2E Testing** | 7 steps | 1 new script, browser testing |

Total: 29 implementation steps

Priority Order:
1. Part C (Generated Code) - Quick wins, immediate user impact
2. Part B (Dashboard) - User-facing, high visibility
3. Part D (E2E Testing) - Validate all fixes
4. Part A (Runtime) - Foundation for future managed hosting

---

## Execution Checklist

- [ ] **Part C.1**: Fix .gitignore template
- [ ] **Part C.2**: Add ENV support for ForgeProvider URL
- [ ] **Part C.3**: Fix duplicate mod.rs entries
- [ ] **Part C.4**: Extend parser for jobs/crons/workflows
- [ ] **Part C.5**: Fix delete subscription UI handling
- [ ] **Part C.6**: Add sample job/cron/workflow/alert
- [ ] **Part B.1**: Replace Chart.js stub
- [ ] **Part B.2**: Wire metrics page search/filter
- [ ] **Part B.3**: Wire logs page search/live stream
- [ ] **Part B.4**: Wire traces page filters
- [ ] **Part B.5**: Implement alerts page data loading
- [ ] **Part B.6**: Implement jobs page tabs/retry
- [ ] **Part B.7**: Implement workflows page detail
- [ ] **Part B.8**: Implement crons page functionality
- [ ] **Part B.9**: Enhance cluster page leadership
- [ ] **Part B.10**: Add dashboard CSS
- [ ] **Part D.1**: Generate test application
- [ ] **Part D.2**: Test CRUD with Chrome MCP
- [ ] **Part D.3**: Test dashboard observability
- [ ] **Part D.4**: Test real-time subscriptions
- [ ] **Part D.5**: Test generated code quality
- [ ] **Part D.6**: Test CLI add commands
- [ ] **Part D.7**: Cleanup test applications
- [ ] **Part A.1-A.6**: Runtime versioning (future phase)

package forge

import (
	"context"
	"fmt"
	"math"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

var metricHistogramBounds = []float64{0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10}

type metricSeries struct {
	name    string
	kind    string
	labels  map[string]string
	value   float64
	count   uint64
	sum     float64
	buckets []uint64
}

type instanceMetrics struct {
	mu          sync.Mutex
	series      map[string]*metricSeries
	lastSuccess map[Primitive]time.Time
}

func newInstanceMetrics() *instanceMetrics {
	return &instanceMetrics{series: make(map[string]*metricSeries), lastSuccess: make(map[Primitive]time.Time)}
}

func metricKey(name string, labels map[string]string) string {
	keys := make([]string, 0, len(labels))
	for key := range labels {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	var builder strings.Builder
	builder.WriteString(name)
	for _, key := range keys {
		builder.WriteByte('\x00')
		builder.WriteString(key)
		builder.WriteByte('=')
		builder.WriteString(labels[key])
	}
	return builder.String()
}

func cloneLabels(labels map[string]string) map[string]string {
	result := make(map[string]string, len(labels))
	for key, value := range labels {
		result[key] = value
	}
	return result
}

func (m *instanceMetrics) counter(name string, labels map[string]string, delta float64) {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := metricKey(name, labels)
	series := m.series[key]
	if series == nil {
		series = &metricSeries{name: name, kind: "counter", labels: cloneLabels(labels)}
		m.series[key] = series
	}
	series.value += delta
}

func (m *instanceMetrics) gauge(name string, labels map[string]string, value float64) {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := metricKey(name, labels)
	series := m.series[key]
	if series == nil {
		series = &metricSeries{name: name, kind: "gauge", labels: cloneLabels(labels)}
		m.series[key] = series
	}
	series.value = value
}

func (m *instanceMetrics) histogram(name string, labels map[string]string, value float64) {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := metricKey(name, labels)
	series := m.series[key]
	if series == nil {
		series = &metricSeries{name: name, kind: "histogram", labels: cloneLabels(labels), buckets: make([]uint64, len(metricHistogramBounds))}
		m.series[key] = series
	}
	series.count++
	series.sum += value
	for index, bound := range metricHistogramBounds {
		if value <= bound {
			series.buckets[index]++
		}
	}
}

func (m *instanceMetrics) markSuccess(primitive Primitive, at time.Time) {
	m.mu.Lock()
	m.lastSuccess[primitive] = at
	m.mu.Unlock()
}

func (m *instanceMetrics) successTime(primitive Primitive) (time.Time, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	value, ok := m.lastSuccess[primitive]
	return value, ok
}

func (m *instanceMetrics) snapshot() []MetricSample {
	m.mu.Lock()
	defer m.mu.Unlock()
	keys := make([]string, 0, len(m.series))
	for key := range m.series {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	result := make([]MetricSample, 0, len(keys))
	for _, key := range keys {
		series := m.series[key]
		sample := MetricSample{Name: series.name, Kind: series.kind, Labels: cloneLabels(series.labels), Value: series.value}
		if series.kind == "histogram" {
			count, sum := series.count, series.sum
			sample.Count, sample.Sum = &count, &sum
		}
		result = append(result, sample)
	}
	return result
}

func prometheusLabels(labels map[string]string, extraKey, extraValue string) string {
	if len(labels) == 0 && extraKey == "" {
		return ""
	}
	keys := make([]string, 0, len(labels)+1)
	for key := range labels {
		keys = append(keys, key)
	}
	if extraKey != "" {
		keys = append(keys, extraKey)
	}
	sort.Strings(keys)
	parts := make([]string, 0, len(keys))
	for _, key := range keys {
		value := labels[key]
		if key == extraKey {
			value = extraValue
		}
		value = strings.NewReplacer("\\", "\\\\", "\n", "\\n", "\"", "\\\"").Replace(value)
		parts = append(parts, key+"=\""+value+"\"")
	}
	return "{" + strings.Join(parts, ",") + "}"
}

func (m *instanceMetrics) renderPrometheus() string {
	m.mu.Lock()
	defer m.mu.Unlock()
	keys := make([]string, 0, len(m.series))
	for key := range m.series {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	seenType := make(map[string]bool)
	var builder strings.Builder
	for _, key := range keys {
		series := m.series[key]
		if !seenType[series.name] {
			fmt.Fprintf(&builder, "# TYPE %s %s\n", series.name, series.kind)
			seenType[series.name] = true
		}
		if series.kind != "histogram" {
			fmt.Fprintf(&builder, "%s%s %s\n", series.name, prometheusLabels(series.labels, "", ""), strconv.FormatFloat(series.value, 'g', -1, 64))
			continue
		}
		for index, bound := range metricHistogramBounds {
			fmt.Fprintf(&builder, "%s_bucket%s %d\n", series.name, prometheusLabels(series.labels, "le", strconv.FormatFloat(bound, 'g', -1, 64)), series.buckets[index])
		}
		fmt.Fprintf(&builder, "%s_bucket%s %d\n", series.name, prometheusLabels(series.labels, "le", "+Inf"), series.count)
		fmt.Fprintf(&builder, "%s_sum%s %s\n", series.name, prometheusLabels(series.labels, "", ""), strconv.FormatFloat(series.sum, 'g', -1, 64))
		fmt.Fprintf(&builder, "%s_count%s %d\n", series.name, prometheusLabels(series.labels, "", ""), series.count)
	}
	return builder.String()
}

// ProbeOptions bounds dependency checks and optionally selects the readiness set.
type ProbeOptions struct {
	Deadline          time.Duration
	ReadinessBackends []Primitive
}

// IsLive reports whether the in-process Forge handle can accept work.
func (f *Forge) IsLive() bool {
	return !f.closed.Load()
}

// MetricsSnapshot returns deterministic, per-Forge metric samples.
func (f *Forge) MetricsSnapshot() []MetricSample {
	f.refreshRuntimeGauges()
	return f.metrics.snapshot()
}

// RenderPrometheus renders the current metrics in Prometheus text format.
func (f *Forge) RenderPrometheus() string {
	f.refreshRuntimeGauges()
	return f.metrics.renderPrometheus()
}

func (f *Forge) refreshRuntimeGauges() {
	if f.mode == ModePostgres {
		for _, primitive := range allPrimitives {
			stat := f.postgres(primitive).Stat()
			labels := map[string]string{"primitive": string(primitive)}
			f.metrics.gauge("forge_pool_open_connections", labels, float64(stat.TotalConns()))
			f.metrics.gauge("forge_pool_idle_connections", labels, float64(stat.IdleConns()))
		}
	}
	f.metrics.gauge("forge_workers_active", nil, float64(f.activeWorkers.Load()))
}

func primitiveForOperation(operation string) Primitive {
	prefix, _, _ := strings.Cut(operation, ".")
	switch prefix {
	case "kv":
		return PrimitiveKV
	case "queue":
		return PrimitiveQueue
	case "blob":
		return PrimitiveBlob
	case "auth":
		return PrimitiveAuth
	case "config":
		return PrimitiveConfig
	case "ratelimit":
		return PrimitiveRateLimit
	case "schedule":
		return PrimitiveSchedule
	case "pubsub":
		return PrimitivePubsub
	default:
		return Primitive("system")
	}
}

func (f *Forge) recordOperationStart(operation string) {
	f.metrics.counter("forge_operations_total", map[string]string{"operation": operation, "outcome": "started", "primitive": string(primitiveForOperation(operation))}, 1)
}

func (f *Forge) recordProbe(primitive Primitive, duration time.Duration, err error) {
	provider := f.backendProvider(primitive)
	labels := map[string]string{"primitive": string(primitive), "provider": provider}
	f.metrics.histogram("forge_backend_probe_duration_seconds", labels, duration.Seconds())
	outcome := "success"
	if err != nil {
		outcome = "failure"
	} else {
		f.metrics.markSuccess(primitive, f.now())
	}
	f.metrics.counter("forge_backend_probes_total", map[string]string{"outcome": outcome, "primitive": string(primitive), "provider": provider}, 1)
}

func (f *Forge) backendProvider(primitive Primitive) string {
	if primitive == PrimitiveBlob && f.s3Blob != nil {
		return "s3"
	}
	if f.mode == ModePostgres {
		return "postgres"
	}
	return "memory"
}

// Probe performs one bounded real operation against every enabled backend.
func (f *Forge) Probe(ctx context.Context, options ProbeOptions) (HealthReport, error) {
	if options.Deadline <= 0 {
		options.Deadline = 2 * time.Second
	}
	if err := contextReady(ctx, "health.probe"); err != nil {
		return HealthReport{}, err
	}
	started := time.Now()
	probeCtx, cancel := context.WithTimeout(ctx, options.Deadline)
	defer cancel()
	type result struct {
		primitive Primitive
		duration  time.Duration
		err       error
	}
	results := make(chan result, len(allPrimitives))
	for _, primitive := range allPrimitives {
		primitive := primitive
		go func() {
			probeStarted := time.Now()
			err := f.probeBackend(probeCtx, primitive)
			results <- result{primitive: primitive, duration: time.Since(probeStarted), err: err}
		}()
	}

	readiness := make(map[Primitive]bool)
	if len(options.ReadinessBackends) == 0 {
		for _, primitive := range allPrimitives {
			readiness[primitive] = true
		}
	} else {
		for _, primitive := range options.ReadinessBackends {
			if !validPrimitive(primitive) {
				return HealthReport{}, forgeError(CodeInvalid, "health.probe", "unknown readiness backend: "+string(primitive))
			}
			readiness[primitive] = true
		}
	}
	report := HealthReport{Live: f.IsLive(), Ready: f.IsLive(), CheckedAtMs: float64(f.now().UnixMilli()), Backends: make([]BackendHealth, 0, len(allPrimitives))}
	for range allPrimitives {
		result := <-results
		f.recordProbe(result.primitive, result.duration, result.err)
		status, message := "ok", "backend operation succeeded"
		var category *string
		if result.err != nil {
			status, message = "error", "backend operation failed"
			value := string(ErrorCodeOf(result.err))
			category = &value
			if readiness[result.primitive] {
				report.Ready = false
			}
		}
		var lastSuccess *float64
		if value, ok := f.metrics.successTime(result.primitive); ok {
			ms := float64(value.UnixMilli())
			lastSuccess = &ms
		}
		report.Backends = append(report.Backends, BackendHealth{Primitive: string(result.primitive), Provider: f.backendProvider(result.primitive), Status: status, LatencyMs: float64(result.duration.Microseconds()) / 1000, ErrorCategory: category, LastSuccessMs: lastSuccess, Message: message})
	}
	sort.Slice(report.Backends, func(i, j int) bool { return report.Backends[i].Primitive < report.Backends[j].Primitive })
	report.DurationMs = float64(time.Since(started).Microseconds()) / 1000
	return report, nil
}

// Diagnostics runs bounded read-only deployment checks. Applications decide where to expose it.
func (f *Forge) Diagnostics(ctx context.Context, deadline time.Duration) (DiagnosticsReport, error) {
	if deadline <= 0 || deadline > 30*time.Second {
		return DiagnosticsReport{}, forgeError(CodeInvalid, "client.diagnostics", "deadline must be between 1ms and 30s")
	}
	ctx, cancel := context.WithTimeout(ctx, deadline)
	defer cancel()
	checkedAt := time.Now()
	checks := []DiagnosticCheck{{Name: "configuration", Status: "pass", Message: "resolved " + string(f.mode) + " profile"}}
	if f.mode == ModeMemory {
		checks = append(checks,
			DiagnosticCheck{Name: "database_version", Status: "pass", Message: "not applicable to the memory profile"},
			DiagnosticCheck{Name: "schema_state", Status: "pass", Message: "memory profile has no persistent schema"},
			DiagnosticCheck{Name: "permissions", Status: "pass", Message: "memory profile requires no external permissions"},
			DiagnosticCheck{Name: "clock_skew", Status: "pass", Message: "memory profile uses the application clock"},
		)
	} else {
		pool := f.postgres(PrimitiveQueue)
		var version string
		if err := pool.QueryRow(ctx, "SHOW server_version_num").Scan(&version); err != nil {
			checks = append(checks, DiagnosticCheck{Name: "database_version", Status: "fail", Message: "could not read the PostgreSQL server version"})
		} else if parsed, err := strconv.Atoi(version); err != nil || parsed < 170000 {
			checks = append(checks, DiagnosticCheck{Name: "database_version", Status: "fail", Message: "PostgreSQL version is below the supported minimum"})
		} else {
			checks = append(checks, DiagnosticCheck{Name: "database_version", Status: "pass", Message: "PostgreSQL server_version_num " + version + " is supported"})
		}
		schema, err := inspectPostgresSchema(ctx, pool, "system")
		if err != nil {
			checks = append(checks, DiagnosticCheck{Name: "schema_state", Status: "fail", Message: "could not inspect Forge migration history"})
		} else if schema.State != "applied" {
			checks = append(checks, DiagnosticCheck{Name: "schema_state", Status: "fail", Message: "Forge schema is " + schema.State + ": " + schema.Message})
		} else {
			checks = append(checks, DiagnosticCheck{Name: "schema_state", Status: "pass", Message: "Forge schema " + schema.TargetVersion + " is current"})
		}
		var permitted bool
		if err := pool.QueryRow(ctx, "SELECT has_table_privilege(current_user,'forge_jobs','SELECT,INSERT,UPDATE,DELETE')").Scan(&permitted); err != nil || !permitted {
			checks = append(checks, DiagnosticCheck{Name: "permissions", Status: "fail", Message: "runtime role lacks required Forge queue table permissions"})
		} else {
			checks = append(checks, DiagnosticCheck{Name: "permissions", Status: "pass", Message: "runtime role can read and mutate Forge queue tables"})
		}
		var serverEpoch float64
		if err := pool.QueryRow(ctx, "SELECT EXTRACT(EPOCH FROM clock_timestamp())::double precision").Scan(&serverEpoch); err != nil {
			checks = append(checks, DiagnosticCheck{Name: "clock_skew", Status: "fail", Message: "could not compare the database and application clocks"})
		} else {
			skew := math.Abs(serverEpoch - float64(checkedAt.UnixNano())/1e9)
			status := "pass"
			if skew > 30 {
				status = "fail"
			} else if skew > 5 {
				status = "warn"
			}
			checks = append(checks, DiagnosticCheck{Name: "clock_skew", Status: status, Message: fmt.Sprintf("database clock differs from the application by %.3fs", skew)})
		}
	}
	health, err := f.Probe(ctx, ProbeOptions{Deadline: deadline})
	if err != nil {
		return DiagnosticsReport{}, err
	}
	reachability := DiagnosticCheck{Name: "backend_reachability", Status: "pass", Message: "all required backend probes succeeded"}
	if !health.Ready {
		reachability.Status = "fail"
		reachability.Message = "one or more required backend probes failed"
	}
	checks = append(checks, reachability)
	unsafe := f.environment == EnvironmentProduction && f.mode == ModeMemory && f.allowMemoryInProd
	safety := DiagnosticCheck{Name: "unsafe_production_settings", Status: "pass", Message: "no unsafe production override is active"}
	if unsafe {
		safety.Status = "fail"
		safety.Message = "production explicitly permits the non-durable memory profile"
	}
	checks = append(checks, safety)
	ready := true
	for _, check := range checks {
		if check.Status == "fail" {
			ready = false
		}
	}
	return DiagnosticsReport{Ready: ready, CheckedAtMs: float64(checkedAt.UnixMilli()), Checks: checks}, nil
}

func (f *Forge) probeBackend(ctx context.Context, primitive Primitive) error {
	switch primitive {
	case PrimitiveKV:
		_, err := f.KVExists(ctx, "__forge_health__")
		return err
	case PrimitiveQueue:
		_, err := f.Depth(ctx, "__forge_health__")
		return err
	case PrimitiveBlob:
		_, err := f.BlobHead(ctx, "__forge_health__")
		return err
	case PrimitiveAuth:
		_, err := f.VerifyAPIKey(ctx, "__forge_health__")
		return err
	case PrimitiveConfig:
		_, err := f.ConfigGet(ctx, "__forge_health__")
		return err
	case PrimitiveRateLimit:
		_, err := f.RateLimitCheck(ctx, "__forge_health__", "probe", RateLimitOptions{Max: 1, Per: time.Second})
		return err
	case PrimitiveSchedule:
		_, err := f.ScheduleList(ctx, nil, 1)
		return err
	case PrimitivePubsub:
		return f.Publish(ctx, "__forge_health__", nil)
	default:
		return forgeError(CodeInvalid, "health.probe", "unknown backend")
	}
}

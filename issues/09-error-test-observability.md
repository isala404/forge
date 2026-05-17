# Audit: Error Handling, Testing, Observability Maintainability

Scope: `crates/forge-core/src/error.rs`, `crates/forge-core/src/testing/`,
`crates/forge-runtime/src/observability/`, `crates/forge-runtime/src/signals/`.

---

## 1. `ForgeError` discards error chains via `.to_string()` everywhere

**Where:** `crates/forge-core/src/error.rs:14-110`, callers `auth/tokens.rs:152,252,309,321`, `pg/migration/runner.rs:314,357,361,447,451,536`, `pg/pool.rs:144,158,339`, `testing/db.rs:90,95,243`, etc. ~27 call sites use the `.map_err(|e| ForgeError::X(e.to_string()))?` shape.

**Severity:** High.

**Pain:** Every variant is `Variant(String)`. The original `sqlx::Error`, `io::Error`, network failure, parse error, etc. is flattened to its `Display` impl and the source chain is lost. `ForgeError` is `#[derive(Error)]` but only `Database`, `Io` use `#[from]` with a typed inner — the rest are opaque strings, so `err.source()` returns `None` for ~85% of errors. Operator gets `"Internal error: connection refused"` and no way to introspect, `match`, or surface a structured cause.

**Fix:** Add a single typed `Internal { context: String, #[source] source: Box<dyn Error + Send + Sync> }` variant (and similar for `Database`, `Config` if they need context). Replace `.map_err(|e| ForgeError::Internal(e.to_string()))` with `.map_err(|e| ForgeError::with_context("Failed to acquire lock", e))`. Keep the string variants only for cases with no underlying error (`NotFound`, `Validation`).

---

## 2. Variant sprawl with weak categorization — 23 flat variants, no client/server/internal grouping

**Where:** `crates/forge-core/src/error.rs:12-110`.

**Severity:** Medium.

**Pain:** `Function`, `Job`, `Cluster`, `Internal`, `InvalidState`, `Config`, `Serialization`, `Cluster`, `WorkflowSuspended` all collapse to 500 in `http_status()` (line 193: `_ => 500`). Callers can't pattern-match "is this a user error or a server fault" without enumerating every variant. The `#[non_exhaustive]` marker + flat list pushes that burden onto every consumer. `Function(String)` and `Internal(String)` are semantically indistinguishable in practice — both are "something went wrong".

**Fix:** Split into a two-level enum: `ForgeError::Client(ClientError)` (400/401/403/404/409/422/429) vs `ForgeError::Server(ServerError)` (500/503/504). Or add `pub fn is_client_error(&self) -> bool` / `is_retryable(&self)` methods so policy lives in one place. Collapse `Function`, `Job`, `Cluster`, `InvalidState`, `Internal` into one `Internal { kind: Kind, ... }`.

---

## 3. `WorkflowSuspended` is a control-flow sentinel masquerading as an error

**Where:** `crates/forge-core/src/error.rs:84-85`, `http_status()` maps it to 500 implicitly.

**Severity:** Medium.

**Pain:** Comment says "Internal signal for workflow suspension. Never returned to clients." but it goes through the same channel as real errors. If a workflow suspension ever leaks (a bug in router/middleware), the user gets a 500 with the literal text "Workflow suspended" — confusing and unactionable. Tests can't distinguish "workflow correctly suspended" from "workflow died with no message".

**Fix:** Hoist suspension into the workflow executor's own `enum StepOutcome { Completed, Suspended, Failed(ForgeError) }`. Remove from `ForgeError` entirely.

---

## 4. Sensitive data leaks through error `Display` impls

**Where:** `crates/forge-core/src/auth/tokens.rs:152,252,309,321`, `pg/pool.rs:144,158` (the `e.to_string()` includes connection URLs from sqlx), `error.rs:215` (`Forbidden(format!("Outbound request to private host '{host}' blocked"))`).

**Severity:** High.

**Pain:** `sqlx::Error::Configuration` and `sqlx::Error::Io` include the connection string (with password) when displayed. `ForgeError::Database(#[from] sqlx::Error)` then surfaces that string via `Display` — and the gateway returns `error.to_string()` to clients in many response paths. Auth-token errors include `"Failed to rotate refresh token: <sql error>"` and the SQL error commonly echoes parameter values (token IDs). Forbidden message includes the private host the user tried to hit, which is fine to log but unnecessary to return to a client probing SSRF.

**Fix:** Add `pub fn client_message(&self) -> Cow<str>` that returns a sanitized string for `Display`-to-client paths; keep `Display` for logs. Strip passwords from sqlx errors at the `From` boundary. Replace the private-host echo with a generic "Outbound destination not permitted."

---

## 5. `assert_*` macros and string helpers force brittle substring matching

**Where:** `crates/forge-core/src/testing/assertions.rs:178-187` (`error_contains`, `validation_error_for_field`), every variant carries `String` so test code does `err.to_string().contains("email")`.

**Severity:** Medium.

**Pain:** Tests assert on error *messages*, not on structure. Rename "email required" → "email is required" and every test breaks. `validation_error_for_field` is literally substring search over a free-form string (line 185: `msg.contains(field)`). No way to assert "this validation error referenced the `email` field" without coupling to copy.

**Fix:** Either (a) add a `ForgeError::Validation { field: Option<String>, message: String }` shape and an `assert_validation_error!(result, field: "email")` macro, or (b) provide `assert_err_code!(result, "validation.field_required")` against a stable error code. Drop `error_contains` from the public API.

---

## 6. No mock surfaces for Postgres, no `MockJobRunner`, no `MockWorkflowExecutor`

**Where:** `crates/forge-core/src/testing/mod.rs:39-49`. Mocks exist for `MockHttp`, `MockJobDispatch`, `MockWorkflowDispatch` (dispatch only — not execution).

**Severity:** Medium.

**Pain:** `MockJobDispatch` records dispatches but `.complete_job()` / `.fail_job()` just flips an enum field — there's no way to test "the job ran and produced output X". Same for workflows. For DB, there is no mock at all — the docs explicitly say "test against real databases" (mod.rs:11). That's defensible, but it means every CRUD unit test needs a Postgres container (testcontainers feature), which is slow and excludes Windows/sandboxed CI. `db.rs:90-95` quietly returns `ForgeError::Internal` on container start, hiding container-runtime issues behind a generic 500.

**Fix:** Add `MockJobRunner` that executes a registered handler closure when a job is dispatched (lets users test "dispatch_job → handler ran → side effects observable"). Add `MockWorkflowExecutor` for the same. For DB, document the existing `IsolatedTestDb` pattern more prominently rather than building a mock — but acknowledge in the testing docs that there is no in-memory option.

---

## 7. `MockJobDispatch::dispatch_in_conn` silently ignores the connection

**Where:** `crates/forge-core/src/testing/mock_dispatch.rs:213-221`. The `_conn` parameter is dropped on the floor.

**Severity:** Medium.

**Pain:** Real `JobDispatch::dispatch_in_conn` enrolls the job in the caller's transaction (rollback = no job). The mock pretends to participate but actually records into an in-memory `Vec<DispatchedJob>` outside any transaction. A mutation test that rolls back will still see the job "dispatched" in `assert_job_dispatched!`. Silently wrong tests pass.

**Fix:** Either (a) accept a `Transaction` in the mock and track per-transaction state, or (b) document loudly that mock dispatch is non-transactional and add an `assert_job_committed!` that requires explicit confirmation. The current shape gives test authors false confidence.

---

## 8. `MockHttp` pattern matcher has subtle wildcard semantics

**Where:** `crates/forge-core/src/testing/mock_http.rs:184-220`. Pattern `"https://api.example.com/*"` matches *anything* with that prefix including `.../etc/passwd`, but `"https://api.example.com"` requires an exact match.

**Severity:** Low.

**Pain:** First-registered-wins (test at line 572-589). Overlapping patterns silently shadow each other. Pattern-or-path matching (`endpoints.rs` line 172-173) means a mock for `"/health"` matches *any* URL with that path, even cross-origin. Surprising for tests where the same path is served by two different mocked services.

**Fix:** Document the precedence rule in the doc comment on `add_mock_sync`. Consider adding `MockHttp::mock_exact()` vs `mock_glob()` to make intent explicit. Or switch to a real glob crate.

---

## 9. OTLP exporter has no resilience around endpoint unavailability

**Where:** `crates/forge-runtime/src/observability/telemetry.rs:159-216`. `SpanExporter::builder().with_http().with_endpoint(&config.otlp_endpoint).build()` — if the collector is down at startup, `init_tracer()` fails and propagates `TelemetryError::TracerInit` up; the runtime startup aborts.

**Severity:** High.

**Pain:** The default endpoint is `http://localhost:4318` (line 59). If a deploy doesn't have an OTel collector running on the same host, the entire forge service fails to start with `"failed to initialize tracer: ..."`. There's no fallback to fmt-only logging, no exponential backoff, no `try_init_or_warn`. `shutdown_telemetry()` (line 342) logs `warn!` on shutdown failure but startup is fatal. Inconsistent.

**Fix:** Wrap exporter init failures: on `TracerInit` / `MeterInit` / `LoggerInit`, log a `warn!`, disable that specific signal, continue with fmt-only output. The user gets logs locally; the operator fixes the collector. Most production OTel stacks support graceful degradation; ours doesn't.

---

## 10. Cardinality blowup: HTTP `path` label is unbounded

**Where:** `crates/forge-runtime/src/observability/metrics.rs:42-51`.

**Severity:** High.

**Pain:** `record_http_request(method, path, status, duration)` adds `path` as a metric attribute. `path` here is the raw request URL path including IDs (`/_api/rpc/get_user/123e4567-...`). Every distinct user ID, document ID, etc. becomes a new time series. With Prometheus/Cortex/Mimir backends this OOMs the metric backend within hours under load. The function/job metrics correctly use `function: &str` (the handler name) but HTTP uses raw path.

**Fix:** Pass the *matched route template* (`/_api/rpc/{function}`) instead of the resolved path. Axum's `MatchedPath` extractor exposes this. Cap path values at a known set or drop the label entirely (status + method are usually sufficient).

---

## 11. Span naming is inconsistent across modules

**Where:** Compared across:
- `observability/db.rs:141` → `"db.query"` (OTel semconv, lowercased dotted)
- `gateway/server.rs:977` → `"http.request"` (OTel semconv)
- `function/router.rs:264` → `"fn.execute"` (custom)
- `function/router.rs:639` → `"db.transaction"` (mostly conformant)
- `jobs/worker.rs:253` → `"job.execute"` (custom)
- `cron/scheduler.rs:219` → `"cron.tick"` (custom)
- `daemon/runner.rs:96,293,366` → no canonical name shown

**Severity:** Low.

**Pain:** Operators querying traces have to know each module's bespoke name. OTel semantic conventions say verbs are derived (`http.server.request`, `db.query`). Field names mix dot (`db.system`, `db.operation.name`) with underscore (`job_id`, `job_type`, `request_id`, `trace_id`) — Tempo / OTel collectors expect dotted form.

**Fix:** Pick one convention (recommend OTel semconv: dotted, lowercase, noun.verb) and apply across the runtime. Field names: `job.id`, `job.type`, `request.id`, `trace.id`. Codify in a tiny module-level doc comment in `observability/mod.rs`.

---

## 12. Signals channel capacity is a process-wide singleton

**Where:** `crates/forge-runtime/src/signals/collector.rs:33-52`, default `channel_capacity = 10_000` (`config/signals.rs:112`). When full, events are silently dropped with `warn!` (line 59).

**Severity:** Medium.

**Pain:** A single warn-log per dropped event under burst load produces log spam (one line per dropped signal — could be thousands/sec). There's no metric for "signals dropped", so operators don't know how lossy their analytics actually are. Burst test (`collector.rs:670-687`) confirms drops are expected behavior. 10k capacity sounds high but a typical RPC server doing 1k req/s burst-emits ~5k events/sec (RPC + view + diagnostic) and the flush is bound by DB latency.

**Fix:** Replace `warn!` per drop with (a) a rate-limited warn (`warn_every_n!` or `tracing::warn!` gated by `AtomicU64` accumulating dropped count, flushed every 1s), and (b) increment a `forge_signals_dropped_total` counter so dashboards show it. Optionally, raise default to 50k.

---

## 13. Signal partition auto-creation has a one-month edge case

**Where:** `crates/forge-runtime/src/signals/partition.rs:14-34`, scheduler at `crates/forge/src/runtime.rs:875-898` (sleep 86400s loop).

**Severity:** High.

**Pain:** Two issues. First, the scheduler is `tokio::time::sleep(86400)` — if the process restarts at month-end and runs `ensure_partitions` once, then sleeps 24h, partition for the new month exists, but if the process happens to be down on the 1st at midnight, inserts for the first hour land in `forge_signals_events_default` (the catch-all). Second, only "current + next" are pre-created. A long-running deployment that survives a year never proactively creates `+2 months`, so a deploy on the 31st at 23:55 may miss the boundary if `ensure_partitions` hasn't run since 23:00 — the gap is small but real.

**Fix:** Trigger `ensure_partitions` on a cron schedule aligned to the 25th of each month (advisory-lock leader-only) in addition to the daily sleep loop. Pre-create current + next-2 partitions, not next-1. Detect inserts landing in `forge_signals_events_default` and surface as a warning.

---

## 14. `IsolatedTestDb::cleanup` swallows `pg_terminate_backend` errors silently

**Where:** `crates/forge-core/src/testing/db.rs:264-269`. `let _ = sqlx::query("SELECT pg_terminate_backend(...)").execute(&pool).await;`

**Severity:** Low.

**Pain:** If termination fails (permissions, role mismatch), the next `DROP DATABASE` will fail with "database is being accessed by other users". User sees a generic error from line 271-274 — no breadcrumb that the *terminate* step is what failed. Tests then leak databases that pile up across runs.

**Fix:** Log at `warn!` on terminate failure; keep proceeding to `DROP DATABASE`. The `let _ =` is fine; just don't make it silent.

---

## 15. Per-test database creation contends on a single connection pool

**Where:** `crates/forge-core/src/testing/db.rs:144-149`. Each `isolated()` call opens a new pool to the *base* database with `max_connections(1)`, just to issue `CREATE DATABASE`.

**Severity:** Low.

**Pain:** Running N tests in parallel opens N pools to `postgres://.../postgres`. Each pool's TCP setup, auth handshake, and TLS negotiation runs once per test, adding ~50-200ms per test. Connection-spike on the base DB also causes `pg_hba.conf` rejections in some environments. The `from_container()` path (line 80) shares one container but each test still opens its own admin pool.

**Fix:** Cache the admin pool on `TestDatabase` itself; reuse across `isolated()` calls. Or use `sqlx::PgConnection::connect()` (single connection, no pool) for the one-shot DDL.

---

## Top 3 fixes before GA

1. **Stop losing error chains.** Issues #1 + #4. Add a typed `Internal { context, #[source] source }` variant and a `client_message()` separation. This is the single change that most improves "operator gets useful errors" *and* "client never sees a password in a 500 body". Everything downstream — debugging, alerting, security review — depends on it.

2. **Fix observability cardinality + OTLP resilience.** Issues #9 + #10. The HTTP `path` label will OOM any metric backend in production, and a missing OTel collector at startup currently kills the service. Both are pre-GA blockers for anyone running forge in a real environment.

3. **Make signals lossy-by-design visible.** Issues #12 + #13. Today, dropped signals are invisible (per-event warn spam ≠ a metric), and a process restart at month-end can land events in the default partition. Both issues silently corrupt analytics — the kind of bug nobody finds until a quarter-end report comes back wrong. Add a dropped-events counter and a leader-elected, schedule-driven partition refresh.

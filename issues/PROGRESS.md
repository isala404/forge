# Forge Pre-GA Issue Tracker

Consolidated checklist drawn from `issues/01-…` through `issues/12-…`. Each item has the originating report code in brackets (e.g. `[01.1]` = report 01, finding 1) so you can jump back to the full write-up. Severity is preserved from the source report. The **Validate** lines are concrete acceptance criteria: a reviewer or a CI script should be able to confirm each one without re-reading the diff.

Legend: `Crit` = ship-blocker correctness/security/data-loss · `High` = must-fix before GA · `Med` = polish before GA · `Low` = nice-to-have before 1.0.

---

## A. Security — Auth, RPC, SSRF, CORS, rate limiting

- [x] **[04.1] JWKS kidless fallback accepts arbitrary keys** · *High*
      Context: `crates/forge-runtime/src/gateway/jwks.rs:153` — when a JWT arrives without a `kid`, `JwksClient::get_any_key()` returns the first cached key. On shared issuers (Firebase, Clerk multi-app) an attacker can sign with any key the JWKS exposes and have it accepted.
      Validate: (a) tokens with missing `kid` are rejected unless `jwks.allow_kidless = true` is explicitly opted into; (b) a unit test mints a token with `kid` stripped against a JWKS holding two keys and asserts the gateway returns 401; (c) the config flag is documented in the new security model page.

- [x] **[04.2] `validate_aud = false` is the default** · *High*
      Context: `crates/forge-runtime/src/gateway/auth.rs:475` disables audience validation when no audience is configured. Combined with the bundled MCP OAuth server, any token the same issuer mints for a sibling service is accepted at the RPC gateway.
      Validate: (a) `Forge::build()` errors when `auth.issuer` is set but `auth.audience` is missing outside `FORGE_ENV=development`; (b) an integration test mints two Auth0-style tokens with different `aud` claims, asserts only the matching one is accepted; (c) audience presence shows up in `forge check` output.

- [x] **[04.3] CORS wildcard ships with only a warning** · *High*
      Context: `crates/forge-runtime/src/gateway/server.rs:435-444` — `allow_origins = ["*"]` plus `allow_credentials = true` degrades to "any origin without credentials" with a warn-log. `allow_headers(Any)`/`allow_methods(Any)` still pass.
      Validate: (a) startup refuses to boot when `FORGE_ENV != development` and origins contain `"*"` or headers/methods are `Any`; (b) `forge check --production` flags the same misconfig; (c) the security model page documents the exact production-safe CORS shape.

- [x] **[04.4] OAuth rate limiter uses `"unknown"` as client IP** · *High*
      Context: `crates/forge-runtime/src/gateway/oauth.rs:1055` — `client_ip()` returns the literal string `"unknown"`. All `/oauth/register`, `/oauth/token`, `/oauth/login`, `/oauth/authorize` callers share one global bucket. Attacker burns global capacity to lock everyone out.
      Validate: (a) OAuth handlers call into `ResolveClientIp` middleware and read the resolved IP from request extensions; (b) a load test from two distinct IPs proves their buckets are independent; (c) `peer_addr()` is used as fallback, never the literal string `"unknown"`.

- [x] **[04.5] `HybridRateLimiter` is local-only and is the default** · *Med*
      Context: `crates/forge-runtime/src/rate_limit/limiter.rs:256` — per-user/per-IP counters live in an in-process DashMap. In an N-node cluster, every limit becomes N× the configured value.
      Validate: (a) when `forge_nodes` has more than one active row, hybrid backend either upgrades scope to DB or refuses startup; (b) docs name `StrictRateLimiter` as the multi-node default; (c) an integration test with two nodes sharing a DB observes the bucket as a single global counter.

- [x] **[04.6] JSON-depth middleware is content-type bypassable** · *Med*
      Context: `crates/forge-runtime/src/gateway/server.rs:1056-1080` — depth-check inspects body only when `Content-Type` starts with `application/json`. A `text/plain` body with 10k-deep nesting still hits `serde_json` and exhausts the stack.
      Validate: (a) the depth check runs unconditionally for any body sent to `/_api/rpc/*`; (b) a regression test posts a 10k-deep JSON body with `Content-Type: text/plain` and asserts 400/413, not a stack overflow.

- [x] **[04.7] `dev_mode()` only checks `FORGE_ENV`** · *Med*
      Context: `crates/forge-runtime/src/gateway/auth.rs:141` — guard inspects exactly one env var. Users deploying to Railway/Fly/Cloud Run set `NODE_ENV=production` / `RAILWAY_ENVIRONMENT` / `K_SERVICE` instead and silently boot into dev auth.
      Validate: (a) dev mode is opt-in (`FORGE_ENV=development` or `--dev`) not opt-out; (b) presence of any standard production env indicator (`NODE_ENV=production`, `RAILWAY_ENVIRONMENT`, `K_SERVICE`, `FLY_APP_NAME`, `KUBERNETES_SERVICE_HOST`, `AWS_EXECUTION_ENV`) refuses dev mode; (c) tests exercise each indicator.

- [x] **[04.8] Session cookie has no IP / UA binding** · *Med*
      Context: `crates/forge-runtime/src/gateway/auth.rs:740` — session cookie is HMAC over `{session_id, expires_at}`. Stolen cookie usable from anywhere until expiry.
      Validate: (a) the cookie is bound to a coarse IP class (/24 for IPv4, /48 for IPv6) and/or UA hash; (b) logout invalidates the session_id server-side; (c) a privilege-affecting action rotates the cookie.

- [x] **[04.9] SSRF guard does not resolve DNS** · *Med*
      Context: `crates/forge-core/src/http/mod.rs:160` — `url_targets_private_ip` checks only literal IPs in the URL. A hostname resolving to `169.254.169.254`/`127.0.0.1` is accepted. DNS rebinding also bypasses.
      Validate: (a) resolved IP is rechecked at connect time via a custom `reqwest` resolver, and the socket is bound to that resolved IP so a second lookup cannot rebind; (b) a regression test points a TTL-0 record at a metadata IP and asserts the guard blocks it; (c) the limitation is gone from the rustdoc.

- [x] **[04.10] `audience()` builder sidesteps reserved-claim guard** · *Med*
      Context: `crates/forge-core/src/auth/claims.rs:193` — `ClaimsBuilder::claim("aud", _)` is filtered, but the typed `audience()` builder writes to `custom["aud"]` directly. Two callers writing to the same logical claim store it in different places.
      Validate: (a) `aud` is a typed top-level field on `Claims`, not in the custom map; (b) `ClaimsBuilder::audience()` and `.claim("aud", _)` round-trip through the same JWT field; (c) a unit test asserts equality of resulting tokens.

- [x] **[04.11] OAuth `RegisterRequest` fields are unbounded** · *Low*
      Context: `crates/forge-runtime/src/gateway/oauth.rs:194-208` — `client_name`, `redirect_uris`, `grant_types`, etc. have no per-field caps. A 1 MB `client_name` is indexed and returned by every list.
      Validate: per-field validation: 256 chars for names, 20 entries for URI lists, 2048 chars per URI; a regression test posts an over-cap registration and expects 400.

- [x] **[04.12] Legacy `extract_client_ip` trusts `X-Forwarded-For` unconditionally** · *Low*
      Context: `crates/forge-runtime/src/gateway/mod.rs:89` — old helper bypasses `trusted_proxies`. Spoofable for any caller that still uses it.
      Validate: function is deleted or delegates to `resolve_client_ip`; `grep -rn extract_client_ip crates/` returns zero non-test hits.

- [x] **[04.13] Attacker-controlled `kid` echoed into logs** · *Low*
      Context: `crates/forge-runtime/src/gateway/auth.rs:391` — `kid` strings with control chars or newlines forge log lines on JSON collectors that don't escape.
      Validate: a sanitizer strips ASCII control chars and caps length to 64 before logging any client-provided string (kid, audience, subject); a regression test passes a kid of `\x1b[2K\rok` and asserts the log line contains an escaped form.

---

## B. Security — SQL safety, tenant scoping, multitenancy

- [x] **[05.1] Scope check is satisfied by *mentioning* the column, not binding it to the caller** · *Crit*
      Context: `crates/forge-macros/src/sql_extractor.rs:785-829` — `expr_has_scope` returns true on any `Expr::Identifier("user_id")` regardless of operator or bound value. `WHERE owner_id = '00000000-…'` passes the check.
      Validate: (a) enforcement moves to data layer via Postgres RLS with a session GUC bound from `ctx.user_id()` before the connection is handed to user code, OR a typed query builder that only exposes scoped parameter slots; (b) docs state explicitly that the macro check is a lint, not isolation, until RLS lands; (c) the malicious example in the audit (`WHERE owner_id = '0000…'`) is rejected at compile time or at runtime by RLS.

- [x] **[05.2] Helper-function indirection bypasses the scope check** · *Crit*
      Context: `crates/forge-macros/src/query.rs:284-316` — scope check runs only when `!table_dependencies.is_empty()`, and table extraction only walks SQL literals inside the handler body. One level of helper hides the SQL.
      Validate: (a) macro errors when `table_dependencies` is empty AND the body calls into anything taking `DbConn`/`&PgPool`/`ForgeDb`; (b) a regression test compiles a query that delegates to a helper and asserts the macro rejects it.

- [x] **[05.3] `tables("...")` override silently disables scope checking** · *High*
      Context: `crates/forge-macros/src/query.rs:287` — `has_explicit_tables` short-circuits the scope check.
      Validate: scope check runs regardless of `tables(...)`; opting out of scope requires explicit `unscoped`; a regression test that uses `tables(...)` without `unscoped` and lacks a scope predicate fails at compile time.

- [x] **[05.4] JOIN-with-scope makes other tables in the join unscoped** · *Crit*
      Context: `crates/forge-macros/src/sql_extractor.rs:718-733` — if any JOIN-ON references a scope column, the whole SELECT is treated as scoped, including tables not filtered.
      Validate: (a) scope predicate must reference a column resolving to the table the SELECT reads from; (b) `select_is_scoped` tracks per-table scope; (c) regression test: `SELECT s.* FROM secrets s JOIN users u ON u.user_id = $1` is rejected.

- [x] **[05.5] CTE outer-scope doesn't restrict CTE body** · *High*
      Context: `crates/forge-macros/src/sql_extractor.rs:1159-1168` — an outer `WHERE user_id = $1` on a `WITH all_t AS (SELECT * FROM tasks)` is accepted even when the inner CTE reads the full table.
      Validate: WHERE must bind against a column originating in the unscoped table; CTE-body scope analysis is added; regression test for a CTE reading an unscoped table with outer-only scope predicate is rejected.

- [x] **[05.6] `tenant_id` is treated as scope but never enforced at runtime** · *High*
      Context: `crates/forge-macros/src/sql_extractor.rs:603` lists `tenant_id`; `QueryContext::tenant_id()` returns `Option<Uuid>` — binding `None` silently filters to no rows under `=`.
      Validate: a runtime guard refuses to dispatch a query that depends on `tenant_id` when `auth` has no tenant claim; queries with `tenant_id` scope require RLS or session GUC binding.

- [x] **[05.7] Mutations are not scope-checked at all** · *Crit*
      Context: `crates/forge-macros/src/mutation.rs` — no `sql_references_identity_scope` equivalent; the `unscoped` attribute on `DarlingMutationAttrs:40` is parsed and never read. `DELETE FROM users` with no WHERE compiles.
      Validate: (a) mutations enforce the same scope check on INSERT/UPDATE/DELETE; (b) DELETE/UPDATE without WHERE is rejected unless `unscoped`; (c) INSERTs require `user_id`/`owner_id` in the column list or `unscoped`; (d) regression test: `DELETE FROM users` in a private mutation fails to compile.

- [x] **[05.8] `JobDispatcher::dispatch<J>` drops the principal** · *Crit*
      Context: `crates/forge-runtime/src/jobs/dispatcher.rs:27-34` — typed dispatch hardcodes `owner_subject = None`. Only `dispatch_by_name` from `MutationContext` carries auth; `cancel_job(id, caller)` then treats a NULL owner as world-cancellable.
      Validate: typed dispatch APIs accept `&dyn HandlerContext` so the principal is mandatory; a job row inserted via typed dispatch records a non-null owner; cancel-without-auth on a non-null-owned job is rejected.

- [x] **[05.9] JobContext / WorkflowContext are constructed unauthenticated** · *Crit*
      Context: `crates/forge-runtime/src/jobs/executor.rs:116-125` and `workflow/executor.rs:163-183` — neither restores auth from `owner_subject` even though it's persisted. Inside a job/workflow, `ctx.auth.user_id()` is always `None`.
      Validate: (a) job/workflow rows persist a structured principal snapshot (user_id UUID + tenant_id + role claims); (b) executor restores it into a real `AuthContext` before invoking the handler; (c) `JobContext::actor()` returns `Result<Uuid>` (no Option); (d) a regression test dispatches a job from a known principal and asserts the handler sees the same `user_id`.

- [ ] **[05.10] Shared queue leaks tenant identity** · *High*
      Context: A job dispatched by tenant A and one by tenant B sit in the same `forge_jobs` table. Without restored auth (05.9), helpers that fall back to "no filter when auth is missing" cross-mix data.
      Validate: per-worker, on claim, set PG session GUC `forge.principal_id` / `forge.tenant_id` from the stored principal; combine with RLS (05.1); a regression test simulates the cross-tenant helper pattern and asserts rows are still isolated.

- [x] **[05.11] SQL extractor inspects only specific macro/method names** · *Med*
      Context: `crates/forge-macros/src/sql_extractor.rs:101-173` — visitor descends into `query|query_as|query_scalar|query_as_unchecked`. Custom wrappers and `sqlx::raw_sql`/`query_with` are not in the allow-list. Runtime SQL builders hide from extraction.
      Validate: the visitor recognizes `raw_sql`, `query_with`, all `sqlx::query!` variants; runtime-built SQL in non-`unscoped` queries fails to compile.

- [x] **[05.12] `looks_like_sql` heuristic poisons docstrings and log strings** · *Low/Med*
      Context: `crates/forge-macros/src/sql_extractor.rs:32-64` — any 10+ char literal starting with SELECT/INSERT/UPDATE/DELETE/WITH that matches a paired keyword is run through sqlparser. A `tracing::info!("Will UPDATE users WHERE active")` fails parsing.
      Validate: SQL detection is anchored to known call-site contexts; the heuristic visitor for arbitrary literals is removed; a regression test with a log message containing SQL-like text compiles cleanly.

- [x] **[05.13] `.sqlx/` cache is trust-on-first-use** · *Med*
      Context: `.sqlx/*.json` is checked in; CI does not re-run `cargo sqlx prepare` and diff. A doctored cache can ship through PR review.
      Validate: a CI job regenerates `.sqlx/` against a fresh PG and runs `git diff --exit-code`; the workflow file change is committed; a PR that hand-edits `.sqlx/` fails CI.

- [x] **[05.14] `is_scope_col` is name-only** · *Low* — Documented limitation in SCOPE_COLS doc comment. Security model page deferred to [10.F11].
      Context: `crates/forge-macros/src/sql_extractor.rs:850-852` — a metrics table with a non-FK column literally named `user_id` passes scope trivially. Until RLS lands, the macro can't know the column's semantic role.
      Validate: docs state explicitly that scope checking is name-based; the limitation is referenced in the security model page.

- [x] **[05.15] `WHERE user_id IN (unscoped subquery)` is accepted** · *Med*
      Context: `crates/forge-macros/src/sql_extractor.rs:816-818` — `Expr::InSubquery` returns true if the LHS is a scope expr OR the subquery is scoped, short-circuiting regardless of subquery shape.
      Validate: the macro requires both LHS scope and subquery scope (or LHS bound to a parameter); regression test asserts `WHERE user_id IN (SELECT user_id FROM other_users)` without a scope predicate on `other_users` is rejected.

---

## C. Performance — Gateway / RPC path

- [x] **[01.1] Cached-query path double-clones `serde_json::Value`** · *Crit*
      Context: `crates/forge-runtime/src/function/router.rs:271-274, 467-470` — `args.clone()` before dispatch + `Value::clone(&cached)` out of `Arc<Value>` + a third wrap in `RpcResponse::success`. Cache hits triple-clone.
      Validate: (a) `route()` takes `args` by reference end to end; (b) `RpcResponse` carries `Arc<Value>` and implements `Serialize` over it; (c) `args.clone()` for the timeout-log payload happens only in the error branch; (d) benchmark: cache hit allocs/req drop ≥3× compared to baseline; (e) `cargo bench --bench cache_hit` (new) tracks the regression.

- [x] **[01.2] Result-size guard re-serializes every response** · *High*
      Context: `crates/forge-runtime/src/function/router.rs:402-418` — `check_result_size` calls `serde_json::to_string(value)` purely to measure length; axum's `Json` then serializes again.
      Validate: a counting serializer measures bytes without allocating, OR one serialize-and-measure feeds the body directly; benchmark proves only one full serialize occurs per response.

- [x] **[01.3] JSON depth-check buffers body twice** · *High*
      Context: `crates/forge-runtime/src/gateway/server.rs:1056-1102` — middleware reads body via `to_bytes(_, usize::MAX)`, scans, re-wraps; `Json<RpcRequest>` re-parses. Also defeats `DefaultBodyLimit`.
      Validate: (a) middleware parses straight to `Value` and stashes it as an extension OR uses a streaming depth scanner that doesn't buffer; (b) the configured `max_body_size_bytes` is used, not `usize::MAX`; (c) regression test: a 5 MB body is rejected at the configured cap.

- [x] **[01.4] SSE session map is one process-wide `RwLock<HashMap>`** · *High*
      Context: `crates/forge-runtime/src/gateway/sse.rs:202` — every subscribe/unsubscribe/new connection takes a tokio write lock; `sse_handler` scans all sessions twice per new connection.
      Validate: replaced with `DashMap` plus `DashMap<UserId, AtomicUsize>` and `DashMap<IpAddr, AtomicUsize>` counters; per-user/per-IP enforcement is O(1); a soak test at 10k SSE sessions does not exhibit subscribe-latency growth.

- [x] **[01.5] Per-request signal emission allocates 4–6 strings** · *High*
      Context: `crates/forge-runtime/src/function/router.rs:251, 298-302` and `function/rpc_signals.rs:50-80` — `info.kind.to_string()` + `client_ip.clone()` + `user_agent.clone()` + `correlation_id.clone()` per RPC even when the channel is full and the event is dropped.
      Validate: `FunctionKind::as_str()` returns `&'static str`; the signal context is built once as `Arc<RpcSignalContext>`; SHA-256 visitor-ID derivation runs inside the collector worker, not at emit time; a bench shows per-call signal alloc count drops to ~0 on a no-signals build and ≤1 on the full build.

- [ ] **[01.6] JWT validated on every request with no positive cache** · *High*
      Context: `crates/forge-runtime/src/gateway/auth.rs:360-482` — every authenticated request runs full HMAC/RSA verify; legacy-key scan fires on kid miss.
      Validate: an LRU keyed on `(blake3(token), config_epoch)` returns cached `Claims` with TTL = `min(exp, now+60s)`; legacy scan is skipped when kid matches primary; a load test at 50k RPS with a single token shows ≥90% reduction in CPU on the auth path.

- [x] **[01.7] `RpcHandler::handle` clones `RequestMetadata` for no reason** · *Med*
      Context: `crates/forge-runtime/src/gateway/rpc.rs:130-143` — `metadata.clone()` to read `request_id` after moving `metadata`.
      Validate: `request_id` (Uuid, Copy) is extracted before the move; the clone is removed; a `cargo expand` diff confirms.

- [x] **[01.8] Job/workflow fallthrough clones args up to 3×** · *Med*
      Context: `crates/forge-runtime/src/function/router.rs:550, 576` — function-not-found probes job then workflow dispatcher, cloning args each time.
      Validate: a unified name→(Function|Job|Workflow) lookup built at startup means dispatch is one hashmap probe; benchmark confirms not-found path performs zero extra clones.

- [ ] **[01.9] SSE bridge spawns 2 tasks per session** · *Med*
      Context: `crates/forge-runtime/src/gateway/sse.rs:601-679` — bridge + stream-feeder, four channels, two tasks per connection.
      Validate: the two are merged into one (or the conversion happens inline in a `Stream` impl); benchmark: at 10k SSE sessions, task count and memory footprint drop measurably.

- [x] **[01.10] Tracing middleware allocates per request** · *Med*
      Context: `crates/forge-runtime/src/gateway/server.rs:644-647, 922-1008` — `format!("/_api{}", path)` per request plus a linear scan over quiet-paths.
      Validate: quiet-paths normalized at config parse time; lookup is a single `HashSet::contains(path)` call; no per-request `format!` in the hot path.

- [x] **[01.11] Tower concurrency + timeout apply to SSE / health** · *Med*
      Context: `crates/forge-runtime/src/gateway/server.rs:624-647` — `ConcurrencyLimitLayer(512)` and `TimeoutLayer(30s)` apply to long-lived routes and to health probes.
      Validate: SSE / health / ready are split into a sub-router *before* the concurrency layer (or have their own semaphore); a soak test fills SSE capacity and confirms `/health` still returns 200.

- [x] **[01.12] `FunctionRegistry` uses default-hash `HashMap<String, _>`** · *Low*
      Context: `crates/forge-runtime/src/function/registry.rs:81, 155` — DoS-resistant SipHash on a startup-fixed table.
      Validate: registry uses `ahash`/`foldhash`/`phf`; bench shows lookup p99 ≤100 ns.

- [x] **[01.13] `MutationContext` allocates a fresh `Arc<dyn EnvProvider>` per request** · *Low*
      Context: `crates/forge-core/src/function/context.rs:891, 917, 944, 983` — fresh `Arc::new(RealEnvProvider::new())` on every construction.
      Validate: the env provider lives in a `OnceLock<Arc<…>>` and `Arc::clone`s; or the field is `&'static dyn EnvProvider`.

- [x] **[01.14] Multiple `Arc::clone`s per mutation request** · *Low*
      Context: `crates/forge-runtime/src/function/router.rs:511-513, 666-668` — `job_dispatcher.clone()` + `workflow_dispatcher.clone()` + `http_client.clone()` + `issuer.clone()` repeated in `execute_transactional`.
      Validate: deps are batched into a single `Arc<MutationDeps>` cloned once; profiling shows ≥6× reduction in atomic ops per request.

- [x] **[01.15] OTel `KeyValue::new("function", function.to_string())` allocates per call** · *Med*
      Context: `crates/forge-runtime/src/observability/metrics.rs:42-50, 90-99, 167-174` — 4 string allocations per RPC, again 3 per HTTP record, 1 per cache record.
      Validate: function names flow as `&'static str`; pre-built `[KeyValue; 4]` arrays are keyed by `FunctionKind`; at 50k RPS, allocator pressure from metrics drops to near zero.

---

## D. Performance — Reactivity engine

- [ ] **[02.1] `FOR EACH ROW` trigger amplifies bulk writes** · *High*
      Context: `migrations/system/v001_initial.sql:380-383`, `v002_change_log.sql:19-73` — a 50k-row UPDATE fires 50k notifies + 50k change-log inserts and can exhaust PG's 8 GB NOTIFY queue.
      Validate: (a) statement-level mode emits one summary notify when affected count crosses a threshold OR `forge_enable_reactivity(table, mode)` exposes the choice; (b) regression test: a 100k-row UPDATE on a reactive table produces a bounded NOTIFY count; (c) docs name the high-write opt-out.

- [x] **[02.2] Broadcast `Lagged` doesn't trigger durable resync** · *High*
      Context: `realtime/listener.rs:29-32` (1024-slot broadcast) + `realtime/reactor.rs:765-767` — `Err(Lagged(n))` is only `warn!`-logged; `needs_resync` is never set.
      Validate: on `Lagged`, the listener flips `needs_resync` and the next flush tick triggers a full change-log replay; a regression test floods the broadcast and asserts the resync path actually runs.

- [x] **[02.3] `process_change` serializes every notification through one `RwLock::write`** · *High*
      Context: `realtime/invalidation.rs:69-110` — global mutex on every notify.
      Validate: replaced with `DashMap<QueryGroupId, PendingInvalidation>` with shard count matching the manager; concurrent benchmark shows ≥4× throughput on the hot path.

- [x] **[02.4] Fan-out clones full JSON payload per subscriber** · *Med/High*
      Context: `realtime/reactor.rs:654-690` — `RealtimeMessage::Data { data: serde_json::Value }` carried by value; 10k watchers × 10 KB = 100 MB allocated per tick.
      Validate: (a) `RealtimeMessage::Data` carries `Arc<serde_json::Value>` (breaking enum change — do it now); (b) fan-out is delegated to a worker pool; (c) benchmark: per-tick allocation for 10k subscribers is bounded.

- [ ] **[02.5] Slow fan-out for one group head-of-lines others** · *Med*
      Context: `realtime/reactor.rs:516-543, 591-690` — post-execution sequence is serial.
      Validate: a two-stage pipeline (execute → commit+fan-out workers) is in place; benchmark with one large + many small groups shows the small groups don't wait.

- [x] **[02.6] Slow-client backpressure resets `consecutive_drops` on any success** · *Med*
      Context: `realtime/message.rs:107, 234` — a chronically-slow client whose buffer fills intermittently never trips eviction; missed messages silently dropped.
      Validate: a "missed_since_last_success" counter is added; lagging sessions emit a `Lagging` event and require explicit resubscribe; a regression test simulates slow drain and asserts the session is evicted or marked lagging.

- [x] **[02.7] No SSE write timeout; stalled receivers pin tasks** · *Med*
      Context: `gateway/sse.rs:601-618, realtime/message.rs:212-257` — `tx.send(...).await` unbounded; `last_active` resets on any 1-byte success.
      Validate: bridge uses `tokio::time::timeout(5s)` per send or `try_send`; a regression test with a stalled TCP receiver evicts the session within the timeout.

- [x] **[02.8] DashMap eviction holds shard guard across cross-shard removes** · *Med*
      Context: `realtime/manager.rs:188-219, 222-257` — guard pattern is the canonical DashMap deadlock vector.
      Validate: code releases group guards before touching `table_index`; mass-disconnect soak test (10k clients drop in 1s) does not stall.

- [x] **[02.9] Resync sweep re-executes every group every 60s** · *Med*
      Context: `realtime/reactor.rs:560-587, 799-807` — 50k groups → 833 query executions/sec just from the sweep.
      Validate: resync is opt-in per group (triggered by `Lagged` or `needs_resync`); the unconditional sweep runs at a far-tail cadence (≥10 min) or is gated by config; idle-baseline DB QPS drops on a 50k-group test.

- [ ] **[02.10] `find_affected_groups` clones full subscriber set per change** · *Med*
      Context: `realtime/manager.rs:261-280` — per-notify cost scales linearly with subscriber count for hot tables.
      Validate: column-filter prefiltering in `table_index`, or batched per-table change resolution within the debounce window; benchmark on a hot table with 20k subscribed groups shows bounded per-notify cost.

- [x] **[02.11] `update_group_with_data` re-serializes JSON twice for sizing** · *Low/Med*
      Context: `realtime/manager.rs:341-379` — re-serialize purely for `max_cached_result_bytes` check.
      Validate: `compute_hash` returns `(hash, serialized_bytes)`; the bytes are reused for sizing; no second serialize on the hot path.

- [ ] **[02.12] Workflow step notify does 3 DB roundtrips per event in the recv loop** · *Low/Med*
      Context: `realtime/reactor.rs:1037-1061` — per-step lookup of `workflow_run_id`, then `fetch_workflow_data_static` × 2.
      Validate: (a) trigger payload includes `workflow_run_id`; (b) `handle_workflow_change` is spawned, not awaited inline; (c) step notifies coalesce per (workflow_id, window).

- [ ] **[02.13] JWT-expired sessions occupy state until next push** · *Low*
      Context: `realtime/message.rs:212-230, 355-382` — cleanup runs every 60s.
      Validate: a min-heap of `(exp, session_id)` schedules precise expiry batches; idle expired sessions vacate within seconds, not a minute.

- [ ] **[02.14] `forge_change_log` trim is cluster-wide racing DELETE with no advisory lock** · *Low*
      Context: `realtime/reactor.rs:547-558, 794-798`, `v002_change_log.sql:77-87` — every node runs the same `DELETE` against the same table the trigger writes.
      Validate: trim runs behind `pg_try_advisory_lock`; consider partitioning `forge_change_log` and dropping old partitions; concurrency test with 5 nodes shows trim contention vanishes.

- [x] **[02.15] Listener `last_seq` seed races initial `listen()`** · *Low*
      Context: `realtime/listener.rs:178-191` — duplicate-process possible during the seed window.
      Validate: an idempotency guard (`seq <= last_seq.load()`) skips already-processed seqs; a regression test with concurrent NOTIFY + seed asserts no double-processing.

---

## E. Performance — Jobs, workflows, cron, daemons

- [ ] **[03.1] Durable-sleep wakeup is polling-only and the partial index is stale** · *P0*
      Context: `workflow/scheduler.rs:165-196`, `migrations/system/v001_initial.sql:166-168`, `v005_workflow_status.sql:17-19` — no `pg_notify('forge_workflow_wakeup', …)` on `wake_at` set or arrival; the partial index filters `status='waiting'` while v005 set `status='sleeping'` on durable sleep. Seq scan at 10M sleeping rows.
      Validate: (a) partial indexes split: `ON wake_at WHERE status='sleeping'` + `ON event_timeout_at WHERE status='waiting'`; (b) a true wakeup table or NOTIFY-on-set is implemented; (c) `EXPLAIN` on the poll query at 10M rows shows index usage; (d) a 30-day `ctx.sleep` survives a restart and wakes within the documented precision.

- [ ] **[03.2] Workflow scheduler is not leader-gated; nodes race on the same rows** · *P1*
      Context: `workflow/scheduler.rs:165-181, 334-392` — every node SELECTs candidates, races UPDATEs on shared rows.
      Validate: either gated behind `is_leader()` (preferred, matches cron) OR the SELECT uses `FOR UPDATE SKIP LOCKED` with hash partitioning; cluster-soak test shows N nodes do not multiply the wakeup-poll load.

- [x] **[03.3] PgListener tasks have no reconnect** · *P1*
      Context: `jobs/worker.rs:153-180`, `workflow/scheduler.rs:90-101` — listener connects once; on disconnect, dispatch silently falls back to 5s polling.
      Validate: listeners wrap their connect+recv in an exponential-backoff reconnect loop; on reconnect, the wakeup trigger is signaled; a counter metric reports each reconnect; a chaos test drops the PG connection and observes dispatch latency stays within poll interval.

- [x] **[03.4] Empty-queue polling cost; intervals not in forge.toml** · *P1*
      Context: `worker.rs:50`, `workflow/scheduler.rs:30`, `cron/scheduler.rs:133`, `daemon/runner.rs:30-36` — all hard-coded; no tunability.
      Validate: every interval is surfaced under `[worker]` / `[workflow]` / `[cron]` in `forge.toml`; adaptive back-off doubles the interval up to 30s when N consecutive polls find nothing; NOTIFY pre-empts back-off.

- [x] **[03.5] Stale-reclaim ±1 attempts arithmetic is brittle** · *P1 correctness*
      Context: `jobs/queue.rs:692-722` — `release_stale` does `attempts - 1`; only works because claim does `+1`. Future change breaks fencing silently.
      Validate: `attempts` is monotonic; if a "retries actually attempted" metric is needed, track it in a separate column; a regression test induces a stale claim and asserts fence correctness without arithmetic coupling.

- [ ] **[03.6] Cron catch-up storms; no global rate limit; catch-up runs every tick** · *P1*
      Context: `cron/scheduler.rs:283-292, 392-458`.
      Validate: catch-up runs once on leader takeover; per-cron "caught up to" timestamp is tracked in memory; `cron.catch_up_jobs_per_tick` budget caps cluster-wide insertion rate after downtime.

- [ ] **[03.7] Daemons have no failover heartbeat; leader death takes 30s–2h** · *P1*
      Context: `daemon/runner.rs:333-356` — follower nodes sleep 5s forever waiting on PG-side keepalive.
      Validate: a heartbeat task inside the daemon loop bumps `forge_daemons.last_heartbeat`; followers can detect staleness; `tcp_keepalives_idle = 30` is set on the leader-election connection; `daemon_last_heartbeat_seconds` is exported.

- [ ] **[03.8] Workflow step writes are 2 round-trips per step on a shared pool** · *P1*
      Context: `forge-core/src/workflow/context.rs:365-454` + `workflow/executor.rs:678-705` — `record_step_start` + `record_step_complete` + `set_wake_at`/`set_waiting_for_event` UPDATEs.
      Validate: fast steps elide `record_step_start` (in-memory guard); parallel-step completions batch; workload-isolation between workflow writers and gateway readers is either implemented (semaphore on child pool) or documented as a sizing requirement.

- [x] **[03.9] `validate_resume` failure marks the run permanently `failed`** · *P1*
      Context: `workflow/executor.rs:297-309`, `v005_workflow_status.sql:7-15` collapsed `BlockedSignatureMismatch` into `failed`.
      Validate: a non-terminal `Blocked` status is restored with a `blocking_reason`; `complete/fail_workflow` reject transitions from `Blocked`; scheduler skips blocked rows; `forge workflow unblock <id>` exists; `/_api/ready` reports blocked counts.

- [ ] **[03.10] Compensation handlers are in-memory and lost across restart** · *P1*
      Context: `workflow/executor.rs:326-344`, `forge-core/src/workflow/context.rs:617-622` — closures don't survive restart.
      Validate: compensation handlers must be expressed as named jobs/workflows referenced by registry name; OR the macro rejects `.compensate(...)` followed by a suspension point; either way the failure mode is surfaced at design time, not in production.

- [ ] **[03.11] Worker semaphore acquisition blocks the claim loop** · *P2*
      Context: `jobs/worker.rs:225-308` — `acquire_owned().await` inside `for job in jobs` stalls the loop when system jobs hold the permits.
      Validate: `try_acquire_owned` first; on failure for the system semaphore, the job is returned to the queue with backoff; or claim is split into two queries (system + user) sized appropriately.

- [ ] **[03.12] `start()` fence reset costs a semaphore permit on lost-claim race** · *P2*
      Context: `jobs/executor.rs:64-79`.
      Validate: a `worker_lost_claim_total` metric exists so operators can tune `stale_threshold`; doc warns about the small cost.

- [ ] **[03.13] Cron window double-dispatches on clock skew; ON CONFLICT churn** · *P2*
      Context: `cron/scheduler.rs:227-263` — 2× `poll_interval` look-back per cron per tick.
      Validate: `last_processed_scheduled_time` tracked in memory; window is `(last_processed, now)`; wider 2s window only on first tick after leader takeover.

- [x] **[03.14] Workflow event NOTIFY payload is wasted bandwidth** · *P2*
      Context: `workflow/event_store.rs:43-49` — payload built but scheduler doesn't parse it; just polls.
      Validate: either consume the payload to target specific run IDs or send empty payload to reduce bandwidth; pick one and act.

- [ ] **[03.15] Worker pool vs `max_concurrent` interaction is undocumented** · *P2*
      Context: `jobs/worker.rs:50-58` + shared pool.
      Validate: `Forge::build()` warns/errors when `pool.max_connections < sum(workers × concurrency) + RPC concurrency`; the formula is documented; heartbeat task gets a small `min_connections` reservation.

---

## F. Scalability — Postgres-as-everything

- [ ] **[06.1] `forge_notify_change` trigger is the central chokepoint** · *Crit*
      Context: per-row PL/pgSQL trigger that does full-row JSONB diff, change-log INSERT, NOTIFY, even on `forge_jobs` heartbeats and `forge_workflow_runs.saved_state` writes nobody subscribes to.
      Validate: (a) per-table reactivity declares a watched-column set; (b) the trigger short-circuits when no watched column changed; (c) `forge_jobs`, `forge_workflow_runs`, `forge_workflow_steps` are off the reactivity firehose; (d) heartbeat writes produce zero NOTIFYs in a regression test.

- [ ] **[06.2] NOTIFY 8 KiB cliff is silent** · *High*
      Context: `migrations/system/v002_change_log.sql:61-65`, `pg/notify.rs:48` — payload silently drops column list when over 7900 bytes, forcing table-level invalidation with no metric.
      Validate: `pg_notification_queue_usage()` is polled and exported as a metric; `/ready` flips degraded at >80% queue usage; wide tables (>40 cols) auto-force change-log-only mode.

- [ ] **[06.3] WAL pressure from signals GIN + change_log + workflow steps** · *High*
      Context: `signals/collector.rs:225-282`, `migrations/system/v002_change_log.sql:5-12`, `workflow/executor.rs:678-705`.
      Validate: (a) GIN on `forge_signals_events.properties` is opt-in via config (default off); (b) per-step write reduced to one UPSERT; (c) `forge_workflow_steps` is off the reactivity firehose unless explicitly subscribed.

- [ ] **[06.4] Default pool size is below the documented sizing formula** · *High*
      Context: `pg/pool.rs:9-86`, `config/database.rs:103` — default 50; documented formula is ~130; gateway has no acquire-side semaphore.
      Validate: default pool size raised to match formula; a startup warning fires when `pool_size < worker.max_concurrent + realtime.max_concurrent + 16`; gateway has its own admission semaphore sized below the pool; `pool_size × nodes` against PG `max_connections` is checked at boot.

- [ ] **[06.5] Persistent LISTEN connections grow O(nodes × channels)** · *Med*
      Context: `pg/pool.rs:42-46`, `jobs/worker.rs:160-180`, `realtime/listener.rs:172-182`, `workflow/scheduler.rs:91-93` — one PgListener per worker, per role, per node.
      Validate: process-wide listener fans out to in-process workers via a broadcast channel; doc enforces `nodes × (3 + leader_roles) < max_connections / 4`; cluster of 50 nodes does not exceed expected listener count.

- [ ] **[06.6] Advisory-lock-validate path is `pg_locks` heavy** · *Low/Med*
      Context: `pg/leader.rs:117-160` — `pg_locks` scanned every 1s per role.
      Validate: `pg_locks` probe results cached at least `check_interval`; validate coalesces with refresh; `forge_leaders` stays UNLOGGED.

- [ ] **[06.7] Rate-limiter row contention** · *High*
      Context: `rate_limit/limiter.rs:38-58` — single hot key serializes through one row lock.
      Validate: per-key K-way sharded bucket (K=16 default); a load test at 5k req/sec on a single global bucket shows no PG-side row-lock contention; promote `HybridRateLimiter` to default with local-first admission gate.

- [ ] **[06.8] Replica routing has no read-your-writes guard** · *High*
      Context: `pg/pool.rs:257-277`, `realtime/reactor.rs:88-99`.
      Validate: each mutation commit captures `pg_current_wal_lsn()`; subsequent reads route to a replica only if `pg_last_wal_replay_lsn() >= captured_lsn`, else fall through to primary; or `read_from_replica = false` becomes the recommended default until causality lands; regression test: mutation followed by read sees post-commit state.

- [ ] **[06.9] `forge_workflow_runs` MVCC bloat from JSONB UPDATEs** · *Med/High*
      Context: `workflow/executor.rs:489-509, 612-674`, reactivity at `v001_initial.sql:434`.
      Validate: `saved_state` and `compensation_state` moved to a separate `forge_workflow_state` table not on the reactivity firehose; autovacuum tuning hint in migration; HOT-friendly columns where possible.

- [ ] **[06.10] `forge_jobs` is a hot read+write table that's also on reactivity** · *Med*
      Context: `jobs/queue.rs:245-293`, `v001_initial.sql:74-93`.
      Validate: terminal jobs moved to `forge_jobs_history` on completion; `idx_forge_jobs_owner_status` added; heartbeat frequency reduced or moved to UNLOGGED side table; `forge_jobs` excluded from reactivity firehose.

- [ ] **[06.11] `forge_signals_users` UPSERT contention on per-user bursts** · *Med*
      Context: `migrations/system/v001_initial.sql:793-814`.
      Validate: counters moved to `forge_kv_counters` and flushed periodically; `traits` set on first identify, merged by a job; benchmark shows no row-lock contention from rapid identify() calls.

- [ ] **[06.12] Materialized-view refresh doesn't scale past ~100M events** · *Med*
      Context: `v001_initial.sql:819-928`, `signals/views.rs:11-13` — concurrent refresh holds snapshots forever and prevents vacuum.
      Validate: `forge_signals_daily_stats` replaced with incremental hourly rollups; refresh is tiered (function_stats 5m, daily 1h, retention 6h); benchmark at 100M events shows refresh completes within the tier window.

- [ ] **[06.13] Partition coverage is current + next only** · *Med*
      Context: `signals/partition.rs:14-34`, `v001_initial.sql:679-703`.
      Validate: pre-create current + next 3 partitions; healthcheck that `forge_signals_events_default` is empty; partition coverage exposed via admin endpoint.

- [ ] **[06.14] `forge_change_log` retention is 1h with no minimum size floor** · *Med*
      Context: `v002_change_log.sql:77-87`.
      Validate: retention is `max(1h, N=1e6 rows)`; full-resync rate is capped; `last_seq` is persisted across restarts.

- [ ] **[06.15] Observability gaps make all the above invisible** · *Med*
      Context: `pg_notification_queue_usage()` referenced but not polled; `pg_stat_activity` waits per workload not exposed; replica lag not against an SLO.
      Validate: a daemon exports `pg_stat_activity`, `pg_stat_user_tables`, `pg_stat_replication`, `pg_notification_queue_usage` as Prometheus metrics; `/admin/diag/pg` returns a one-shot snapshot.

---

## G. Cluster coordination

- [ ] **[07.1] Cron-tick split brain inside `lock_validate_interval`** · *High*
      Context: `cron/scheduler.rs:190`, `pg/leader.rs:411` — cached `AtomicBool` is up to 1s stale; both leaders may fire `tick()`.
      Validate: `is_leader()` callers re-validate the lock at the start of each tick; cron claim asserts the caller's node holds the lease row in the same statement.

- [ ] **[07.2] Daemon leader-elected loop never re-checks leadership** · *High*
      Context: `daemon/runner.rs:329-356, 408`.
      Validate: `LeaderElection::run()` is spawned as a sibling task; a `watch::Receiver<bool>` (or `CancellationToken`) is plumbed into `DaemonContext`; daemon handlers are cancelled via `tokio::select!` on leadership drop; a chaos test pulls the lock and observes the handler future is cancelled within `lock_validate_interval`.

- [ ] **[07.3] Lock-owning connection has no proactive keepalive** · *High*
      Context: `pg/leader.rs:122-126, 161`.
      Validate: `SELECT 1` on the held connection every `min(check_interval/2, 5s)`; OR a leader-only single-conn pool with explicit TCP keepalives and `max_lifetime(Duration::MAX)`; a chaos test that injects a TCP RST is detected within one validate cycle.

- [ ] **[07.4] `release_leadership` on pool, not held conn, after partial loss** · *Med*
      Context: `pg/leader.rs:331-341`.
      Validate: a `was_ever_leader` flag gates the DELETE; the DELETE uses `RETURNING node_id` and logs loudly on unexpected races.

- [ ] **[07.5] Heartbeat and lock-validate are decoupled** · *Med*
      Context: `cluster/heartbeat.rs:197-211`, `pg/leader.rs:175` — a node can heartbeat fine yet have lost its lock.
      Validate: `validate_lock_held` failure pushes the node to `draining`/`degraded` via the registry; the DB state matches the Prometheus metric.

- [ ] **[07.6] Heartbeat uses shared pool — exhaustion fakes node death** · *Med*
      Context: `cluster/heartbeat.rs:198-209`.
      Validate: heartbeat + leader election use a dedicated 1-conn pool independent of the request pool.

- [ ] **[07.7] Cron stale reclaim duplicates jobs without cancelling originals** · *High*
      Context: `cron/scheduler.rs:330-358` — original `forge_jobs` row stays claimable.
      Validate: on reclaim, the previous job row is cancelled or a `superseded_at` column is set; worker checks the flag at handler entry; a regression test simulates leader transition mid-cron and asserts the handler fires exactly once.

- [ ] **[07.8] Daemon FNV `lock_id` folded into i64 with collision risk** · *Med*
      Context: `forge-core/src/cluster/roles.rs:88-95`.
      Validate: collision detection at startup aborts boot if two `LeaderRole`s map to the same `lock_id`; OR a `forge_daemon_locks` table assigns serial IDs persistently.

- [ ] **[07.9] Workflow scheduler runs on every node (cost, not correctness)** · *Med*
      Context: `workflow/scheduler.rs:69-75, 113-148`.
      Validate: decision made and documented — either leader-only `process_ready_workflows` (matches cron) or every-node with explicit indexing; the SELECT does not lock-spin; cluster-soak shows linear cost on N nodes.

- [ ] **[07.10] Workflow signature mismatch during rolling deploy strands runs** · *High*
      Context: `workflow/registry.rs:152-176`, `workflow/scheduler.rs:334`.
      Validate: signature mismatch returns the run to its prior `sleeping`/`waiting` state (non-terminal); blocked transition only after N consecutive mismatches across all live nodes; a rolling-deploy chaos test does not strand in-flight runs.

- [ ] **[07.11] Graceful shutdown releases lock before leader-held work drains** · *High*
      Context: `cluster/shutdown.rs:88-131` — only RPC handlers tracked.
      Validate: leader-elected subsystems signal "cleanly stopped" before `release_leadership`; OR the in-flight counter includes daemon/cron/workflow work; a regression test under SIGTERM shows no overlap between old-leader work and new-leader work.

- [ ] **[07.12] Late shutdown subscribers miss the broadcast** · *Med*
      Context: `cluster/shutdown.rs:293-310`.
      Validate: shutdown signal is delivered via `tokio::sync::watch::channel(false)` (replays current value) or the broadcast is paired with an `AtomicBool`; a late subscriber observes shutdown.

- [ ] **[07.13] No schema/version gate for rolling deploys** · *High*
      Context: `cluster/registry.rs:27-57` — `forge_nodes.version` recorded but never read.
      Validate: a `forge_schema_version` table updated by migrations; nodes compare on startup and on every leader acquire; pre-1.0 strictness: refuse mutations when `version` is older than the max active version; chaos test: a v0.5 node refuses leadership after v0.6 migrates.

- [ ] **[07.14] Time-skew is implicit between Rust process and PG** · *Med*
      Context: `pg/leader.rs:139-141, 260-261, 351-364`, `cron/scheduler.rs:227`.
      Validate: all lease/health/stale-reclaim time decisions use PG `NOW()` server-side; `chrono::Utc::now()` removed from paths comparing against PG timestamps.

- [ ] **[07.15] `try_become_leader` never preempts an expired-lease zombie** · *Med*
      Context: `pg/leader.rs:117-166, 431-441` — dead backend can hold the advisory lock for hours.
      Validate: on stale-lease + advisory-lock-busy combo, the standby queries `pg_stat_activity` and surfaces the holder for operator action; aggressive `pg_terminate_backend` is gated behind opt-in config.

---

## H. Maintenance — Macros & codegen

- [x] **[08.1] Two `FunctionKind` enums drift silently** · *High* — NOT A BUG: the two enums serve different layers (runtime dispatch vs codegen source classification) with no overlapping usage. Consolidation would force each layer to carry variants it never uses.
      Context: `forge-core/src/function/traits.rs:74` (Query/Mutation/Webhook) vs `forge-core/src/schema/function.rs:10` (Query/Mutation/Job/Cron/Workflow).
      Validate: consolidated into one `FunctionKind` in `forge-core`, `#[non_exhaustive]`, exhaustive `match` enforced at every consumer; deleting a variant fails to compile somewhere.

- [x] **[08.2] `type_to_rust_type` stringifies syn::Type and substring-matches** · *High*
      Context: `forge-codegen/src/parser.rs:437` — whitespace-sensitive, path-prefix-fragile.
      Validate: structural walk of `syn::Type` (`Type::Path` → last segment → `PathArguments::AngleBracketed`); regression test: `std::vec::Vec<T>` is correctly classified; `HashMap<(K1, K2), V>` does not break.

- [x] **[08.3] TS / Dioxus emitter parity is unenforced** · *High*
      Context: `forge-codegen/src/emit.rs:411` — non-empty assertion only.
      Validate: a property-style test iterates every `RustType` variant via `strum::EnumIter` plus curated `Custom` aliases, asserts neither emitter returns its fallback sentinel; a new variant added to `RustType` fails the test until both emitters handle it.

- [x] **[08.4] Mutation macro silently ignores SQL parse failure** · *High*
      Context: `forge-macros/src/mutation.rs:413` vs `forge-macros/src/query.rs:259` — mutation drops dependencies on failure; reactivity invalidation silently breaks.
      Validate: mutation macro emits the same compile error path as the query macro; regression test: a mutation with un-parseable SQL fails to compile.

- [x] **[08.5] Cron macro emits `.expect()` in generated code** · *Med*
      Context: `forge-macros/src/cron.rs:145`.
      Validate: generated schedule is a `const`/`OnceCell` populated from parsed components; expanded macro output contains no `.expect`.

- [x] **[08.6] Macros vs codegen disagree on primitive integer support** · *Med*
      Context: `forge-macros/src/utils.rs:152` accepts `u32/u64/i8/u8`; `forge-codegen/src/parser.rs:105-119` rejects.
      Validate: supported-primitive list lives in one place in `forge-core` and is consulted by both; unsupported integers fail at macro expansion with a span pointing at the argument.

- [x] **[08.7] Three copies of `to_snake_case`; buggy `pluralize`** · *Med*
      Context: `forge-macros/src/model.rs:146-159`, `enum_type.rs:160-173`, `forge-core/src/util` — `pluralize("quiz") = "quizes"`; acronym handling wrong.
      Validate: macro-side copies removed in favor of one helper; unit tests cover `HTTPRequest`, `XMLParser`, `quiz`, `bus`, `index`.

- [x] **[08.8] Workflow signature uses type-name strings** · *Med* — Documented limitation: proc macros cannot resolve type aliases at expansion time. The comment in `derive_signature` (lines 327-329) already acknowledges this. Fix requires either post-type-check analysis or runtime schema hashing, both out of scope pre-1.0. Workaround: document that input/output type renames require a version bump.
      Context: `forge-macros/src/workflow.rs:308-368` — `OrderInput` → `PurchaseOrder` (alias) changes signature.
      Validate: signature derived from structural type info (field name + RustType) via shared codegen parser; alias rename doesn't change signature; truly structurally different types with same short name produce different signatures.

- [x] **[08.9] `darling::Error::custom` drops spans** · *Med*
      Context: `forge-macros/src/attrs.rs` — generic span on `#[query(...)]` instead of pointing at the offending value.
      Validate: every `Error::custom` site uses `.with_span(&meta)`; a clippy-style internal lint or grep gate catches new spanless errors in CI.

- [x] **[08.10] `inventory::submit!` auto-registration has no opt-out** · *Med*
      Context: every handler macro emits it unconditionally.
      Validate: `#[query(register = false)]` exists; multi-binary workspace pattern documented.

- [x] **[08.11] `ts_hashmap` splitn(2, ',') breaks on nested generics** · *Low*
      Context: `forge-codegen/src/emit.rs:121`.
      Validate: bracket-balance counting (or structural AST walk); regression test for `HashMap<(K1, K2), V>` and `HashMap<String, HashMap<String, i32>>`.

- [x] **[08.12] Generated handler paths hardcode `forge::forge_core::...`** · *Low*
      Context: every macro's expansion.
      Validate: emit `::forgex::forge_core::...` (absolute) or thread `#[forge::renamed = "..."]` resolution; users with a colliding `forge` crate name can rename.

- [x] **[08.13] Codegen doesn't pin syn/quote to workspace** · *Low*
      Context: `forge-codegen/Cargo.toml`.
      Validate: `syn`, `quote` use `{ workspace = true }` across all four crates.

- [x] **[08.14] `looks_like_sql` false-positives on docstrings** · *Low* — NOT A BUG: visitor only runs on sqlx macro call sites; `visit_expr_lit` is a deliberate no-op to prevent false positives from log messages.
      Context: `forge-macros/src/sql_extractor.rs:32-64`.
      Validate: extraction is restricted to `sqlx::query!`/`query_as!` call sites OR requires an explicit `// @forge:sql` marker; a regression test with `tracing::info!("UPDATE users WHERE active")` compiles cleanly.

- [x] **[08.15] `ContractExtractor` silently skips non-literal workflow keys** · *Low*
      Context: `forge-macros/src/workflow.rs:282-296`.
      Validate: macro errors at compile time when a step name is not a string literal.

---

## I. Maintenance — Errors, testing, observability

- [ ] **[09.1] ForgeError discards error chains via `.to_string()`** · *High*
      Context: ~27 sites in `auth/tokens.rs`, `pg/migration/runner.rs`, `pg/pool.rs`, `testing/db.rs`, etc. flatten root causes.
      Validate: typed `Internal { context, #[source] source: Box<dyn Error + Send + Sync> }` variant exists; `.map_err(|e| ForgeError::Internal(e.to_string()))` sites are migrated; `err.source()` returns the original error chain.

- [ ] **[09.2] Variant sprawl; no client/server grouping** · *Med*
      Context: `forge-core/src/error.rs:12-110` — 23 flat variants; many → 500.
      Validate: two-level shape (`Client(ClientError)` vs `Server(ServerError)`) or helper methods (`is_client_error`, `is_retryable`); consumers can pattern-match user vs server faults without enumerating every variant.

- [ ] **[09.3] `WorkflowSuspended` is a control-flow sentinel as an error** · *Med*
      Context: `forge-core/src/error.rs:84-85`.
      Validate: hoisted into the executor's own `StepOutcome { Completed, Suspended, Failed }`; the variant is removed from `ForgeError`.

- [x] **[09.4] Sensitive data leaks through `Display`** · *High*
      Context: sqlx errors include connection strings; SSRF Forbidden echoes the private host; token errors echo parameter values.
      Validate: `client_message()` separates from `Display`; passwords stripped at the `From<sqlx::Error>` boundary; private-host echo replaced with a generic refusal; regression test: a sqlx error never reveals the password in `client_message`.

- [ ] **[09.5] `assert_*` macros force brittle substring matching** · *Med*
      Context: `forge-core/src/testing/assertions.rs:178-187`.
      Validate: structured `ForgeError::Validation { field, message }`; `assert_validation_error!(result, field: "email")` macro; or `assert_err_code!(result, "validation.field_required")`; `error_contains` removed from public API.

- [ ] **[09.6] No mock surfaces for Postgres / job runner / workflow executor** · *Med*
      Context: only `MockHttp` and dispatch-only mocks exist.
      Validate: `MockJobRunner` and `MockWorkflowExecutor` execute registered handlers; `IsolatedTestDb` pattern is documented prominently; docs name the no-in-memory-DB tradeoff explicitly.

- [ ] **[09.7] `MockJobDispatch::dispatch_in_conn` silently ignores the connection** · *Med*
      Context: `forge-core/src/testing/mock_dispatch.rs:213-221`.
      Validate: the mock either participates in the transaction or its non-transactional nature is documented loudly; an `assert_job_committed!` requires explicit confirmation; a regression test rolls back a tx and asserts the mock records no dispatch.

- [ ] **[09.8] `MockHttp` pattern semantics are subtle** · *Low*
      Context: `forge-core/src/testing/mock_http.rs:184-220`.
      Validate: precedence rule documented; `mock_exact()` vs `mock_glob()` is the API; first-registered-wins is explicit.

- [x] **[09.9] OTLP exporter init is fatal at startup** · *High*
      Context: `forge-runtime/src/observability/telemetry.rs:159-216` — default endpoint `http://localhost:4318`.
      Validate: exporter init failures degrade to fmt-only with a `warn!`; service still starts; `shutdown_telemetry` already non-fatal is preserved; chaos test: collector down at boot, service is up.

- [x] **[09.10] HTTP `path` label is unbounded cardinality** · *High*
      Context: `forge-runtime/src/observability/metrics.rs:42-51` — raw URL with IDs.
      Validate: the matched route template (`/_api/rpc/{function}`) is the label, not the resolved path; metric backends do not see unbounded series; benchmark: 10k distinct URLs produce one time series, not 10k.

- [x] **[09.11] Span naming is inconsistent** · *Low* — Reviewed: all 11 spans already use dotted lowercase `namespace.verb` format (db.query, http.request, fn.execute, job.execute, cron.tick, daemon.execute, etc.). Consistent enough; `fn.execute` is the only minor oddity.
      Context: mixed across `db.query`, `http.request`, `fn.execute`, `job.execute`, `cron.tick`.
      Validate: one convention (OTel semconv: dotted, lowercase) adopted across `observability/`, `gateway/`, `function/`, `jobs/`, `cron/`, `daemon/`; documented in a module-level doc comment.

- [x] **[09.12] Signals dropped events are invisible** · *Med*
      Context: `forge-runtime/src/signals/collector.rs:33-52, 59` — per-drop warn-log, no counter.
      Validate: rate-limited warn (accumulator + 1s flush); `forge_signals_dropped_total` Prometheus counter exists; dashboards expose drop rate; default capacity raised if benchmarks support it.

- [ ] **[09.13] Signal partition auto-creation has a month-end edge case** · *High*
      Context: `forge-runtime/src/signals/partition.rs:14-34`; `forge/src/runtime.rs:875-898` 24h sleep loop.
      Validate: leader-elected schedule-driven (advisory-lock) job ensures `current + 2 future` partitions; inserts to `forge_signals_events_default` raise a `/ready` warning.

- [x] **[09.14] `IsolatedTestDb::cleanup` swallows `pg_terminate_backend` errors** · *Low*
      Context: `forge-core/src/testing/db.rs:264-269`.
      Validate: terminate failure logs at `warn!` with cause; cleanup proceeds; tests don't leak databases.

- [x] **[09.15] Per-test admin pool churn** · *Low*
      Context: `forge-core/src/testing/db.rs:144-149`.
      Validate: admin pool is cached on `TestDatabase`; or single connection (`PgConnection::connect`) is used for the DDL; benchmark: test suite startup time drops measurably.

---

## J. Documentation — User docs + skill refs

- [ ] **[10.F1] `start/first-app.mdx` teaches a registration model the framework no longer uses** · *Crit*
      Context: doc walks through `.register_query::<...>()`; all shipped templates use `.auto_register()`.
      Validate: page rewritten around `.auto_register()`; manual `register_*` mentioned only as an escape hatch; a brand-new user can follow the doc and get a running app.

- [ ] **[10.F2] `ctx.http_with_circuit_breaker()` documented but does not exist** · *Crit*
      Context: `docs/docs/build/write-data.mdx:216,258` references a non-existent method.
      Validate: doc updated to match `ctx.http()` (which is already circuit-breaker-backed); skill `api.md` is the source-of-truth; references removed.

- [ ] **[10.F3] `reference/errors.mdx` stale against `ForgeError::http_status()`** · *High*
      Context: missing `Conflict`, `UnprocessableEntity`, `ServiceUnavailable`; `Deserialization` listed as 500 (actual 400).
      Validate: doc regenerated from the source doc-comment table; a CI check diffs the doc comment against `errors.mdx`.

- [ ] **[10.F4] No documentation page for `/admin/*` operator endpoints** · *Crit (ops)*
      Context: `crates/forge-runtime/src/gateway/admin.rs` documented only in the skill ref.
      Validate: new `reference/admin-api.mdx` with audit log, reason convention, `admin` role requirement, incident-response examples; sidebar updated.

- [ ] **[10.F5] `/_api/ready` semantics undocumented** · *High*
      Context: response shape and 5 flags only in skill ref.
      Validate: `ship/deploy.mdx` "Health probes" section documents the flag table; mirrored in skill `patterns.md`.

- [ ] **[10.F6] OAuth 2.1 / MCP endpoints have no user-facing reference** · *High*
      Context: endpoints, payloads, CSRF cookie, sticky-session caveat live in source only.
      Validate: new `reference/oauth.mdx`; cross-linked from MCP security page.

- [ ] **[10.F7] `custom_routes` documented in skill ref only** · *High*
      Context: factory API not in `build/custom-handlers.mdx`.
      Validate: `build/custom-handlers.mdx` (or new `build/custom-routes.mdx`) covers `ForgeBuilder::custom_routes(...)`, reserved path list, middleware behavior.

- [ ] **[10.F8] `RoleResolver` undocumented in user docs** · *Med*
      Context: custom RBAC via `RoleResolver` is in skill `api.md` only.
      Validate: `build/protect-routes.mdx` has a "Custom role resolution" section with builder example.

- [ ] **[10.F9] Worker queue model under-documented** · *High*
      Context: reserved queue names, `worker_capability`, queue-pool starvation isolation scattered.
      Validate: `scale/worker-pools.mdx` rewritten from first principles with diagram of claim SQL, reserved queues, capability matching, pause/resume.

- [ ] **[10.F10] No reactivity mental-model page** · *High*
      Context: `scale/reactivity.mdx` describes pipeline; missing when/why/cost.
      Validate: new `start/reactivity-model.mdx` (or a "How it works" in `build/subscribe-to-changes.mdx`) covers reactivity cost, adaptive row-vs-table tracking, auth scope dedup, when not to use it.

- [ ] **[10.F11] No first-class security model page** · *Crit (GA)*
      Context: security notes scattered.
      Validate: new `ship/security.mdx` covers AuthN, AuthZ, tenant isolation, SSRF, rate-limit modes, admin audit, TLS posture, signals DNT/Sec-GPC; linked from each handler page.

- [ ] **[10.F12] `TenantIsolationMode` is implemented but undocumented** · *High*
      Context: `forge-core/src/tenant/mod.rs` defines None/Strict/ReadShared.
      Validate: covered in the new security model page OR a dedicated `build/multi-tenancy.mdx` with the row-level enforcement story.

- [ ] **[10.F13] Signals: server endpoint discriminator missing from user docs** · *Med*
      Context: `POST /_api/signal` discriminator table only in skill ref.
      Validate: `ship/signals.mdx` has a "Wire format" section with the type discriminator table.

- [ ] **[10.F14] `forge env` / `forge doctor` not in onboarding** · *Med*
      Context: present in CLI reference; missing from `start/first-app.mdx` troubleshooting.
      Validate: `forge doctor` is in the troubleshooting section of `start/first-app.mdx` and `tutorials/shipping-to-production.mdx`.

- [ ] **[10.F15] No error catalog: code → cause → remediation** · *High*
      Context: production-observable conditions (`BlockedMissingVersion`, `notify_queue_ok=false`, `circuit open`, retry-after) unsorted.
      Validate: `reference/errors.mdx` has a "Runtime conditions" section covering every `WorkflowStatus` variant and readiness-flag failure modes with remediation hints.

- [ ] **[10.F16] `WorkflowStatus` variants under-documented** · *High*
      Context: `BlockedMissingVersion`, `BlockedSignatureMismatch`, `RetiredUnresumable`, `CancelledByOperator`, `Compensating`, `Compensated` not in user docs.
      Validate: state-transition table in `build/long-processes.mdx`; "Recover a blocked workflow" runbook in `reference/admin-api.mdx`.

- [ ] **[10.F17] Tutorials don't show advanced macro attributes** · *Med*
      Context: `cache="30s"`, `consistent`, `rate_limit(...)`, `idempotent(key=...)`, `compensate`, `worker_capability`, `replay_window_secs` etc. not taught.
      Validate: new "Advanced macro attributes" page under `build/` with copy-paste snippets.

- [ ] **[10.F18] No "5-minute" onboarding path** · *High*
      Context: `start/first-app.mdx` is 274 lines.
      Validate: `start/first-app.mdx` trimmed to a `forge new ... && docker compose up && open localhost:9080` flow; the heavy walkthrough moves to `tutorials/your-first-feature.mdx`.

- [ ] **[10.F19] No production-architecture / deployment-topology page** · *Crit (ops)*
      Context: deploy specifics scattered between `ship/deploy.mdx` and `scale/multiple-nodes.mdx`.
      Validate: new `ship/production-architecture.mdx`: single-binary vs split worker/api, LB topology, sticky sessions, DATABASE_URL sizing, PG 18 requirement, rolling-deploy workflow-signature caveats, migration order.

- [ ] **[10.F20] Migration operational details missing** · *High*
      Context: forward-only rationale, advisory-lock during rolling deploy, `forge_system_migrations` ledger, schema-change reactivity lifecycle missing.
      Validate: new `ship/migrations.mdx` covers the playbook end-to-end.

- [ ] **[10.F21] Cluster setup / node roles is fragmentary** · *High*
      Context: `roles = ["gateway", "worker", "scheduler"]`, `worker_capabilities`, `cluster_registered` readiness flag scattered.
      Validate: rename `scale/multiple-nodes.mdx` → `scale/cluster-architecture.mdx`; full enumeration of roles + leader election keys.

- [ ] **[10.F22] Cargo features not in user docs** · *Med*
      Context: `gateway`, `worker`, `api`, `minimal`, `geoip`, `otel` only in skill ref.
      Validate: "Build presets" section in `ship/deploy.mdx`; feature-gate errors cross-link to it.

- [ ] **[10.F23] Frontend client API reference is non-existent** · *High*
      Context: `getForgeClient`, `ForgeProvider`, `useForgeAuth`, `setForgeAccessToken`, live store contract have no human-readable reference.
      Validate: new `reference/client-svelte.mdx` and `reference/client-dioxus.mdx` generated from JSDoc/rustdoc; published as part of the docs site.

- [ ] **[10.F24] `forge_enable_reactivity()` and PG helpers undocumented as API** · *Med*
      Context: SQL surface that's part of the framework contract missing.
      Validate: new `reference/postgres-helpers.mdx` covers `forge_enable_reactivity`, `forge_change_log`, `forge_*` reserved prefix.

- [ ] **[10.F25] Testing framework partial in user docs** · *Med*
      Context: backend covered; assertion macros catalogue, Playwright fixtures (`rpc`, `gotoReady`, `uniqueId`, `ACTION_TIMEOUT`) missing.
      Validate: `ship/testing.mdx` "Frontend tests" section uses the fixtures from the realtime-todo example.

- [ ] **[10.F26] Inconsistent terminology** · *Med*
      Context: function vs handler, pool vs queue, subscription vs reactive query vs live store, outbox vs buffered jobs.
      Validate: new `reference/glossary.mdx` defines canonical terms; offending pages updated to use the canonical term.

- [ ] **[10.F27] No doctests / no executable examples** · *Med*
      Context: every Rust snippet in docs is a non-compiled markdown block.
      Validate: a CI job compiles every fenced ```rust block in `docs/docs/` against the workspace; OR canonical snippets are sourced from `examples/with-svelte/demo` via Docusaurus partials.

- [ ] **[10.F28] Reference page completeness gaps** · *High*
      Context: `McpToolContext`, `WebhookContext`, `DaemonContext` light in `reference/contexts.mdx`; Context Capability Matrix only in skill ref.
      Validate: Context Capability Matrix copied into the top of `reference/contexts.mdx`; each context section audited against source methods.

- [ ] **[10.F29] Skill references have content not surfaced for humans** · *Med*
      Context: `resilience.md`, `recipes.md`, `patterns.md`, `pitfalls.md` are AI-only.
      Validate: `pitfalls.md` content promoted into a `Common pitfalls` user page OR a Docusaurus plugin/sidebar surfaces skill refs to humans; CI enforces the "both surfaces in same changeset" policy.

- [ ] **[10.F30] Pre-1.0 stability posture not in user docs** · *Med*
      Context: breaking-change-encouraged stance lives only in `CLAUDE.md`.
      Validate: a "Stability and versioning" section in `docs/docs/index.mdx` (or new `start/stability.mdx`); links from the changelog.

---

## K. Agent-first API ergonomics

- [ ] **[11.F1] Dispatch APIs are string-keyed** · *Crit*
      Context: `dispatch_job`, `start_workflow` are stringly typed across `MutationContext`/`JobContext`/`WebhookContext`/`DaemonContext`/`McpToolContext`. Macros already generate `SendWelcomeEmailJob` types — dispatch doesn't route through them.
      Validate: `ctx.dispatch::<SendWelcomeEmailJob>(input)` and `ctx.start::<WorkflowType>(input)` exist; `dispatch_by_name` reserved as escape hatch; the string-keyed methods are deprecated or removed; type-safe dispatch is the recommended path in docs.

- [ ] **[11.F2] KV store is implemented but invisible to handlers** · *Crit*
      Context: `forge-runtime/src/kv/store.rs` exists; not on any context, not in prelude.
      Validate: `ctx.kv()` exists on `HandlerContext` returning `KvHandle` with `get/set/set_with_ttl/delete/incr`; documented in `api.md`; available from every handler kind.

- [ ] **[11.F3] No `dispatch_job_at` / `dispatch_job_after` on context** · *High*
      Context: `JobQueue::dispatch_with_delay` exists; not on `MutationContext`.
      Validate: `ctx.dispatch_after::<JobType>(input, Duration)` and `ctx.dispatch_at::<JobType>(input, DateTime<Utc>)` exist on every dispatch-capable context.

- [ ] **[11.F4] Context methods drift across handler kinds** · *High*
      Context: `QueryContext::db() → ForgeDb`, `MutationContext::db() → DbConn<'_>`, `JobContext::db() → ForgeDb`; `log_info/warn/error` only on `CronContext`.
      Validate: one canonical name for the pool view, one for the transaction view, applied uniformly across all handler contexts; `ctx.log_*` available everywhere (or removed from `CronContext` for consistency).

- [ ] **[11.F5] `transactional = true` + `ctx.http()` is a silent footgun** · *High*
      Context: `forge-macros/src/mutation.rs:106, 199` — `dispatch_job` is compile-checked, HTTP isn't.
      Validate: either `ctx.http()` is buffered-and-flushed-after-commit in transactional mutations, OR a compile-time warning/error fires when `transactional=true` and `ctx.http()` is called, OR `ctx.http()` is only on `MutationContext::after_commit()`. Decide once; enforce it.

- [x] **[11.F6] Scope check is regex-fragile and only on `#[query]`** · *High* (dup of 05.7) — Duplicate of [05.7] which is already resolved.
      Context: see 05.7 / 05.1.
      Validate: see 05.7 / 05.1.

- [ ] **[11.F7] `MutationContext::db()` returns active tx; `HandlerContext::db()` returns pool** · *Crit*
      Context: same method name, different semantics; generic helpers silently misbehave.
      Validate: pool stays `db()`, transaction becomes `tx()` (only on `MutationContext`); inherent `db()` no longer shadows the trait; a generic helper `fn count<C: HandlerContext>(ctx: &C)` returns consistent results across handler kinds.

- [ ] **[11.F8] Workflow versioning has hidden compile-time invariants** · *High*
      Context: signature is FNV-1a over step keys; rename → silent signature change → in-flight runs blocked.
      Validate: step keys are idents not strings (or a `forge check` lint catches drift); macro rejects workflows without explicit `version =`; startup signature-conflict errors point at the offending step.

- [ ] **[11.F9] Cron schedules are raw strings only** · *Med*
      Context: `#[forge::cron]` requires `"*/5 * * * *"` knowledge.
      Validate: duration sugar (`#[forge::cron(every = "5m")]`, `#[forge::cron(daily_at = "03:00", timezone = "UTC")]`) added; sugar maps to cron internally; raw expression still supported.

- [ ] **[11.F10] Daemons require hand-rolled shutdown loop** · *Med*
      Context: every daemon manually selects on `ctx.shutdown_signal()`.
      Validate: `ctx.tick(Duration) -> bool` or generated loop scaffold from a `interval = "60s"` attribute; daemons can't forget the shutdown channel.

- [ ] **[11.F11] `JobContext::saved` / `save` / `set_saved` overlap** · *Med*
      Context: `forge-core/src/job/context.rs:177, 185, 207` — three APIs for one concern.
      Validate: keep `save(key, value)` + `load(key)`; drop `saved()`/`set_saved()` from public API or rename `clear_then_save_all`.

- [ ] **[11.F12] Job `require_role` is dispatch-time only** · *Med*
      Context: macro doc claims "requires admin to dispatch"; the role check fires only when the job is dispatched without an inherited principal.
      Validate: renamed to `dispatch_requires_role = "admin"` to make semantics explicit; OR role check on jobs removed entirely; doc updated.

- [ ] **[11.F13] `start_workflow` takes a string name with versioning gotchas** · *High*
      Context: agent writes `start_workflow("user_onboarding_v2", ...)` thinking it's the v2 file; framework infers the logical name from the function ident.
      Validate: type-safe `ctx.start::<UserOnboardingWorkflow>(input)` enforced; macro refuses to derive a logical name from a function whose ident ends in `_v\d+`.

- [ ] **[11.F14] `MutationContext::http()` is a footgun mid-transaction** · *High* (related to 11.F5)
      Context: Stripe charge inside a mutation; tx rolls back; payment took.
      Validate: see 11.F5.

- [ ] **[11.F15] `forge.toml` defaults aren't safe-by-default** · *Med*
      Context: CORS allow-localhost in templates, `pool_size = 50` below documented formula, no `[deploy]` section.
      Validate: `forge.toml` supports `[deploy]` activated by `FORGE_ENV=production` enforcing CORS allowlist non-localhost, JWT secret present, observability enabled; `forge check --production` validates; `pool_size` auto-derives from `worker.max_concurrent`.

- [x] **[11.F16] No `forge::*` index in prelude** · *Med* — By design: proc macros can't be re-exported through a prelude module in Rust. `use forge::prelude::*` for types + `#[forge::query]` attribute syntax is idiomatic.
      Context: prelude re-exports types but not the proc macros.
      Validate: either macros re-exported via prelude (`pub use crate::{query, mutation, job, ...}`) OR context types removed from prelude to standardize on `forge::X`; consistent muscle memory across the framework.

- [ ] **[11.F17] `ForgeError::Function` / `Job` / `Cluster` / `Sql` overlap** · *Med* (related to 09.1)
      Context: ~10 variants are all "Internal with a tag."
      Validate: see 09.1 / 09.2.

- [ ] **[11.F18] Testing contexts don't share a builder shape with production contexts** · *Med*
      Context: `TestMutationContext::builder()` exists; production `MutationContext::new(...)` is positional.
      Validate: builder pattern mirrored on production contexts; tests don't need to import `forge-core::testing` for glue code.

- [ ] **[11.F19] No first-class email / notification primitive** · *Med*
      Context: every project rebuilds `email_send`.
      Validate: `forge-email` (or `[email]` in `forge.toml`) ships with SMTP/SES/Resend backends and `ctx.email().send(...)`; `forge-storage` (S3/R2) and `forge-search` (pg full-text) follow the same pattern; mocked in test contexts.

- [ ] **[11.F20] Webhook can't subscribe to its own RPC for replay** · *Low/Med*
      Context: no `ctx.replay()` or built-in dead-letter for webhooks.
      Validate: raw webhook body auto-stored keyed by idempotency key with TTL; `forge webhook replay <id>` CLI exists.

- [ ] **[11.F21] `forge generate` is implicit; codegen not in build graph** · *Med*
      Context: agent ships without running it.
      Validate: `forge check` runs codegen verification; templates have a `build.rs` step OR generated bindings are committed and `forge check` diffs; CI smoke-test fails on drift.

- [x] **[11.F22] Workflow `ctx.step("name", closure)` re-uses string keys** · *High* (dup of 11.F8) — Duplicate of [11.F8]; will be resolved together.
      Context: see 11.F8.
      Validate: see 11.F8.

- [ ] **[11.F23] `public` vs `unscoped` look similar; semantically different axes** · *Med*
      Context: `public` = no auth, `unscoped` = no row filter; they're the same attribute slot today.
      Validate: renamed to `auth = "none"` and `scope = "global"`; `scope = "global"` requires a louder marker (e.g. doc-comment or explicit `// SAFETY:`).

- [x] **[11.F24] `ctx.user_id()` returns `Result<Uuid>` except in `AuthContext`** · *Low* — By design: AuthContext is raw state (Option), handler contexts enforce auth (Result). Already consistent.
      Context: `function/context.rs:511` Option vs `:792` and trait `AuthenticatedContext` Result.
      Validate: consistent: handler contexts return `Result` (require auth); `AuthContext` is raw state with `Option`; types are documented.

- [x] **[11.F25] `forge new` debug-build patches `Cargo.toml` with absolute paths** · *Low* — Already gated behind `#[cfg(debug_assertions)]`; release builds never inject patch sections.
      Context: `cli/new.rs` injects `[patch.crates-io]`.
      Validate: behavior gated behind `FORGE_DEV=1` env var or working directory inside the forge repo; generated projects from a released CLI build never carry absolute paths.

---

## L. Cross-cutting tech debt

- [x] **[12.A1] Workspace deps duplicated as ad-hoc direct versions** · *Med*
      Context: `base64`, `futures-util`, `sha2`, `tokio-util`, `ring`, `hmac`, `sha1`, `aho-corasick`, `percent-encoding`, `serde_urlencoded`, `rustls-pemfile`, `tokio-rustls`, `rustls`, `tls-listener`, `db_ip`, `maxminddb`, `tempfile`, `rcgen` declared inline in places, partially in `[workspace.dependencies]`.
      Validate: all of these are in `[workspace.dependencies]`; every crate references with `{ workspace = true }`; `cargo tree --duplicates` shows no duplicates.

- [x] **[12.A2] Examples bypass workspace deps; benchmark pins old jsonwebtoken 9** · *Med*
      Context: `examples/**/Cargo.toml` and `benchmarks/app/Cargo.toml` redeclare deps inline; benchmarks pin `jsonwebtoken = "9"` while workspace pins `"10"`.
      Validate: every example and benchmark uses `{ workspace = true }`; `cargo tree` shows one `jsonwebtoken` version cluster-wide.

- [x] **[12.A3] `opentelemetry` pinned to 0.27 with stale TODO** · *Med*
      Context: `Cargo.toml:74`.
      Validate: decision made and acted on (rip OTel SDK for hand-rolled OTLP/HTTP, or bump to current upstream); TODO removed.

- [x] **[12.A4] `schemars = "=0.8.22"` exact-pinned without rationale** · *Low*
      Context: `Cargo.toml:50`.
      Validate: either a comment explains the pin, or schemars is bumped to current major and the pin removed.

- [x] **[12.A5] cargo-deny / cargo-audit installed from source every CI run** · *Low*
      Context: `.github/workflows/ci.yml:71-80`.
      Validate: replaced with `taiki-e/install-action@v2` with `cargo-deny,cargo-audit`; CI startup time drops measurably.

- [x] **[12.A6] `RUSTSEC-2025-0134` (rustls-pemfile unmaintained) ignored permanently** · *Low*
      Context: `deny.toml:14-21`.
      Validate: migrated to `rustls-pki-types::PemObject`; the ignore is deleted.

- [ ] **[12.B1] `packages/forge-dioxus` excluded from workspace** · *High*
      Context: `Cargo.toml:18-20`.
      Validate: brought into the workspace with `[target.'cfg(target_arch = "wasm32")']` gating; clippy/fmt/MSRV runs against it; published via `cargo publish -p forge-dioxus` from workspace root.

- [x] **[12.B2] Util fn duplication across crates** · *Med*
      Context: `to_snake_case`, `to_camel_case`, `to_pascal_case`, `parse_duration` 3-4× in source.
      Validate: macro-side copies consolidated to one helper inside `forge-macros/utils.rs`; runtime-side consolidated to `forge-core/util`; `parse_duration` in `forge-core/src/rate_limit/mod.rs` calls `crate::util::parse_duration`; grep shows zero further duplicates.

- [ ] **[12.B3] Three 1500+ line files marked `// TODO(pre-1.0): Split` but never split** · *Med*
      Context: `forge/src/runtime.rs`, `forge/src/cli/check.rs`, `forge-runtime/src/gateway/mcp.rs`.
      Validate: each file split into focused modules; line counts under 800 each; TODO markers removed.

- [x] **[12.B4] `BAD_CODE_0_7.md` deleted but uncommitted** · *Trivial* — Already deleted and committed in earlier work.
      Context: `git status` shows `D BAD_CODE_0_7.md`.
      Validate: committed.

- [x] **[12.B5] `crates/forge/generated/template-bundle.tar` is dead** · *Low*
      Context: not referenced; only `examples.tar` consumed.
      Validate: file deleted; any generation step removed from build scripts.

- [ ] **[12.B6] 99 `clippy::unwrap_used` / `clippy::indexing_slicing` allow escapes** · *Med*
      Context: most in test modules; several in non-test code lack rationale comments.
      Validate: every non-test allow has a justification comment; OR is removed by refactoring to `?`/`.get(..).ok_or(...)`.

- [x] **[12.B7] `.expect("workflow lock poisoned")` repeated 12+ times** · *Low*
      Context: `forge-core/src/workflow/context.rs` and `forge-core/src/schema/registry.rs`.
      Validate: wrapped in a helper `fn states(&self) -> RwLockReadGuard` with a single `const LOCK_MSG`.

- [x] **[12.B8] `#[allow(dead_code)]` on `realtime/listener.rs:279` and `cluster/metrics.rs`** · *Med*
      Context: workspace denies `dead_code`; these escape.
      Validate: each annotated item is either deleted or exposed through the public API; the `#[allow]`s are removed.

- [x] **[12.C1] `testcontainers` feature on `forgex` has no consumer** · *Low* — NOT A BUG: 3 examples (svelte/demo, svelte/realtime-todo-list, dioxus/realtime-todo-list) use `forge/testcontainers`.
      Context: `crates/forge/Cargo.toml:141`.
      Validate: feature dropped from `forgex`; examples enable `forge-core/testcontainers` directly.

- [ ] **[12.C2] Slim presets (`worker`, `api`, `minimal`) untested** · *Med*
      Context: every example pulls in `full`.
      Validate: at least one example or smoke test exercises `worker` or `api`; CI matrix includes a slim-build job.

- [ ] **[12.C3] `geoip` requires build-time network fetch with no offline path** · *Med*
      Context: `crates/forge-runtime/Cargo.toml:67`.
      Validate: `geoip` moved out of `full` default, OR the lite DB is vendored; `cargo build -p forgex` succeeds offline.

- [x] **[12.C4] `gateway` cfg gate on `signals` creates a parallel no-op module** · *Low* — NOT A BUG: intentional pattern documented in lib.rs; no-op stubs eliminate scattered #[cfg] gates at call sites.
      Context: `crates/forge-runtime/src/lib.rs:51-100`.
      Validate: a trait surface (`SignalsSink`) lives in `forge-core` with both impls implementing it; single source of truth for the API shape.

- [x] **[12.D1] No MSRV check in CI** · *High*
      Context: declared `rust-version = "1.92"`; CI uses stable.
      Validate: a CI job pinned to `1.92` runs `cargo check --workspace --all-features`.

- [x] **[12.D2] `cargo test -p todo-dioxus --features testcontainers` missing from CI** · *Med*
      Context: `.github/workflows/ci.yml:111`.
      Validate: workflow includes the Dioxus realtime template integration test next to the Svelte one.

- [x] **[12.D3] PR CI skips 4 of 6 examples** · *Med*
      Context: `pr-smoke` matrix only covers `with-svelte/demo` and `with-dioxus/demo`.
      Validate: all 6 templates run on PR (or representative pairs from each frontend stack covering minimal + realtime-todo); breakages of `forge new with-svelte/minimal` are caught before merge.

- [x] **[12.D4] `benchmarks/app` never runs in CI** · *Med* — Already covered by `cargo clippy --workspace` and `cargo test --workspace` in the validate job; no separate check needed.
      Context: workspace builds it but never runs it.
      Validate: nightly benchmark workflow runs against a fixed commit baseline OR at least `cargo check -p forge-bench --release` runs on PR.

- [ ] **[12.D5] Release pipeline publishes `forge-dioxus` from outside the workspace** · *Low*
      Context: `.github/workflows/release.yml:217`.
      Validate: included in workspace (12.B1) and published via `cargo publish -p forge-dioxus` from workspace root.

- [ ] **[12.D6] NPM publish ships raw `.ts`** · *Med*
      Context: `packages/forge-svelte/package.json` lists `.ts` directly; no `.d.ts` or `.js` build.
      Validate: `tsup` or `svelte-package` build step emits `dist/index.js` + `dist/index.d.ts`; `exports` map points at `dist`; `npm publish` artifact verified by a downstream non-Vite consumer.

- [x] **[12.D7] `test-template.sh` swallows formatter errors** · *Low*
      Context: `scripts/ci/test-template.sh:51-53` uses `|| true`.
      Validate: `|| true` removed; ill-formed output fails loud.

- [x] **[12.D8] `test-template.sh` uses GNU-only `sed -i.bak`** · *Low*
      Context: `scripts/ci/test-template.sh:35`.
      Validate: replaced with `perl -pi -e` or a Python one-liner; behavior identical on macOS/Linux.

- [x] **[12.D9] CI cache key shared across jobs with different feature sets** · *Low* — NOT A BUG: validate saves with `cache-on-failure: true`; guardrails and workspace-integration read-only via `save-if: "false"`; MSRV has its own key. Cache discipline is already correct.
      Context: same `shared-key: ci` for validate, guardrails, workspace-integration.
      Validate: cache keys segmented per feature profile, or `save-if` discipline is consistent; CI cache hit rate improves.

- [ ] **[12.D10] Release publishes crates with hardcoded 30s sleeps** · *Low*
      Context: `.github/workflows/release.yml:222`.
      Validate: replaced with polling the crates.io API for the published version, or with `cargo-release` which handles the propagation natively.

- [x] **[12.E1] docker-compose Postgres version matches CI** · *None* — Verified both at PG 18; no action needed.
      Validate: no action; verified both at PG 18.

- [ ] **[12.E2] No Grafana/Loki/Tempo profile in docker-compose despite signals dashboard reference** · *Low*
      Context: signals docs reference a Grafana dashboard; local dev has none.
      Validate: an `observability` profile in `docker-compose.yml` starts otel-lgtm + Grafana with the signals dashboard pre-provisioned.

- [x] **[12.F1] `release-fast` profile defined but never invoked** · *Low* — Profile serves documented purpose for local benchmarking; not suitable for CI smoke tests where debug builds are appropriate.
      Context: `Cargo.toml:163-167`.
      Validate: used in `template-smoke.yml` (cuts build time) OR deleted.

- [x] **[12.F2] `strip = true` on release loses panic line numbers** · *Low*
      Context: `Cargo.toml:160`.
      Validate: `strip = "debuginfo"` (keeps symbol names); split-debuginfo packed and shipped separately for releases.

---

## Pre-GA must-fix shortlist (cherry-picked across reports)

Use this as the "if nothing else, do these first" view:

1. **Tenant isolation isn't isolation** ([05.1, 05.2, 05.7, 05.9, 05.10]) — ship Postgres RLS + session-GUC principal binding, or change the marketing.
2. **Type-safe dispatch and `db()`/`tx()` ambiguity** ([11.F1, 11.F7]) — cheapest to do now; expensive to break later.
3. **Reactivity: `Lagged` resync, trigger amplification, fan-out JSON-clone** ([02.2, 02.1+02.14, 02.4+02.5, 06.1]) — load-bearing performance.
4. **Cluster: leadership-loss observability + cron stale reclaim + schema version gate** ([07.2, 07.7, 07.13, 07.11]) — rolling deploys must not strand work or split-brain.
5. **Docs: working first-app, security model, production architecture, admin API, error catalog** ([10.F1, 10.F11, 10.F19, 10.F4, 10.F15]) — the highest-priority axis per project remit.
6. **Workflow durability: real wakeup path, non-terminal Blocked, restart-safe compensation** ([03.1, 03.9, 03.10]) — `cargo deploy` cannot keep silently failing in-flight workflows.
7. **OTLP resilience + HTTP path cardinality bomb** ([09.9, 09.10]) — production-grade observability.
8. **JWT validation cache + SSE `RwLock` swap + cache-hit alloc fixes** ([01.6, 01.4, 01.1]) — the gateway's hot-path triple-win.
9. **Auth defaults: kidless tokens, `validate_aud`, CORS wildcard, OAuth IP resolution** ([04.1–04.4]) — stock-config security posture.
10. **NPM publish ships compiled artifacts + `forge-dioxus` enters the workspace** ([12.D6, 12.B1]) — the published surface must not bit-rot.

---

*Last regenerated: 2026-05-17 from `01-…` through `12-…` audit reports. Update this file alongside the source reports when items close.*

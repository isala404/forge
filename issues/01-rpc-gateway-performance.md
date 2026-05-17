# RPC Gateway & Function Executor — Performance Audit

Scope: `crates/forge-runtime/src/gateway/` + `crates/forge-runtime/src/function/`, the middleware stack, the dispatch path, and adjacent shared state (auth, rate limit, observability, mutation context). The reactivity engine, jobs, workflows, and security model are explicitly out of scope.

Findings are ordered by impact on steady-state throughput / tail latency. Severity is the production cost, not the engineering cost.

---

## 1. Cached-query path double-clones the `serde_json::Value` result — Critical

Location: `crates/forge-runtime/src/function/router.rs:271-274` and `:467-470`.

The router calls `self.route(function_name, args.clone(), ...)` — every request clones the parsed argument tree before dispatch, even for cache hits. Then on a cache hit, `Value::clone(&cached)` deep-clones the cached response out of the `Arc<Value>` before returning, defeating the whole point of storing it as `Arc<Value>` in `cache.rs:82`. For a 1 MB cached query result, each cache hit is two large `serde_json::Value` deep-clones plus a third when `RpcResponse::success` wraps it.

Why it matters: cached queries are the workload that should be cheapest. At 10 k RPS with a modest 4 KB cached payload that's ~80 MB/s of allocator churn from cloning alone, and the `Arc` design says the author knew this. Allocator pressure shows up as p99 latency under load, not throughput dropouts.

Fix: 
- Change `route()` to take `args` by reference (it's already `&Value` everywhere downstream except the dispatcher `Arc<dyn JobDispatch>` calls, where it can clone lazily).
- Return `Arc<Value>` (or `Cow<'_, Value>`) all the way out of `route()`/`execute()` and serialize that into the HTTP body directly. The `RpcResponse::success` path can hold an `Arc<Value>` and impl `Serialize` over it. No clone required for the hit path.
- The `args.clone()` at `:273` is for the timeout-error log payload. Defer cloning until the error branch actually fires.

---

## 2. Result-size guard re-serializes every response — High

Location: `crates/forge-runtime/src/function/router.rs:402-418`, called at `:477`, `:494`, `:523`, `:694`.

When `max_result_size_bytes` is set (10 MiB by default per `server.rs:149`), `check_result_size` calls `serde_json::to_string(value)` on the full response purely to measure its length. Axum's `Json(...)` extractor then serializes the same `Value` a second time when building the response body. Two full JSON serializations per request, every request, with the byte-count throwaway being completely discarded.

Why it matters: with a 100 KB response that's an extra 100 KB allocation, full tree walk, and Vec growth per call. Dominant cost on read-heavy workloads where the cached-query path doesn't apply (e.g. paginated lists). The default is non-zero, so this is on by default.

Fix:
- Walk the `Value` without serializing — `serde_json::to_writer(io::sink().count(), &value)` style, or a custom `Serializer` that only counts bytes.
- Better: serialize once into a `Vec<u8>`, check the length, and hand that buffer to axum as `Response::builder().body(Body::from(buf))`. One serialization, exact accounting.
- Even better: bound the response by streaming serialization with an early abort when the byte count exceeds the cap (currently the work is wasted if the body is over the limit).

---

## 3. JSON body buffered twice on the depth-check path — High

Location: `crates/forge-runtime/src/gateway/server.rs:1056-1102` (`json_depth_check_middleware`) + axum's `Json` extractor in `rpc.rs:187`.

The middleware reads the entire body via `axum::body::to_bytes(body, usize::MAX)`, scans it for nesting depth, then rebuilds the request with `Body::from(bytes)` so the downstream `Json<RpcRequest>` extractor reads and parses *the same bytes again*. Every POST /rpc request: one Bytes allocation + scan, one Body re-wrap, one full parse.

Why it matters: this is the universal cost on every RPC request, not a feature for big payloads. With a 1 MB JSON body the middleware buffers 1 MB into a `Bytes` and the extractor allocates another 1 MB `Value` tree off of it. The scan itself is fine; the issue is the unnecessary buffering when the extractor would buffer anyway.

Also note `to_bytes(body, usize::MAX)` here defeats the `DefaultBodyLimit::max(DEFAULT_MAX_JSON_BODY_SIZE)` configured on the same router (`:521`) — order of layers means the body limit applies after the depth check. The depth check should reuse the same limit.

Fix:
- Parse straight to `serde_json::Value` here, get depth from the parsed tree's actual structure, and stash the parsed value as a request extension so the handler reads `Extension<Value>` instead of `Json<...>` re-parsing. Or:
- Replace the byte-scan with a `serde_json::de::Deserializer::from_slice(&bytes).disable_recursion_limit()` style streaming parser that fails on depth without allocating the tree. Or:
- Drop this layer entirely. `serde_json` already has a 128-deep recursion limit by default; the OWASP "stack-busting" risk this middleware claims to defend against is largely a serde-json-pre-1.0 concern. If you keep it, at minimum use the *configured* `max_body_size_bytes` instead of `usize::MAX`.

---

## 4. SSE session lookup is a single `RwLock<HashMap>` for the whole process — High

Location: `crates/forge-runtime/src/gateway/sse.rs:202` (`sessions: Arc<RwLock<HashMap<SessionId, SseSessionData>>>`).

Every subscribe (`:780`), unsubscribe (`:967`), session lookup (`:134`), and *every new SSE connection* (`:517`) takes a `tokio::sync::RwLock` write across the entire session map. The map can be up to `sse_max_sessions = 10_000` entries. The handlers under contention:

- `sse_handler` holds a write lock while iterating the entire map twice to count per-user and per-IP sessions (`:520-549`). That's an O(N) scan under exclusive lock per new connection, blocking every other SSE handler.
- `sse_subscribe_handler` holds a write lock and iterates *all* values summing subscription counts to enforce `max_subscriptions_per_user` (`:796-800`). Quadratic at scale: each subscribe is O(active_user_sessions), and every subscribe contends with every other.
- `unsubscribe` and `validate_session` take a read lock then upgrade with a separate write lock — TOCTOU window is comment-flagged but the contention pattern is what bites first.

Why it matters: 10 k SSE sessions × ~5 subscriptions each × periodic reconnects = the rwlock becomes the gateway's central bottleneck. Subscribe latency scales with active session count rather than being O(1).

Fix:
- Replace with `DashMap<SessionId, SseSessionData>`. The TOCTOU defenses already in the code (`:840-862`) handle non-atomic insert/check correctly.
- Maintain explicit `DashMap<UserId, AtomicUsize>` and `DashMap<IpAddr, AtomicUsize>` counters so per-user/per-IP enforcement is O(1) instead of a full scan. Increment under fence, check-and-decrement on rollback. Same for `max_subscriptions_per_user`.
- The cleanup-guard `Drop` (`:261-283`) spawns a fresh task on every disconnect to async-write-lock + remove. Under churn this is a thundering herd of write locks. Counter maps make the cleanup near-free.

---

## 5. Per-request signal emission allocates 4–6 strings on every RPC call — High

Location: `crates/forge-runtime/src/function/router.rs:251` and `:298-302`, plus `crates/forge-runtime/src/function/rpc_signals.rs:50-80`.

- `:251`: `info.map(|i| i.kind.to_string())` — heap-allocates a string for the function kind on every call, even though `FunctionKind` has 7 known variants and could be `&'static str`.
- `:288`, `:301`: `format!("Timeout after {:?}", fn_timeout)` and `format!("Function '{}' timed out ...", ...)` — two formatted strings on the timeout path (rare). Acceptable.
- `RpcSignalsEmitter::emit` allocates `client_ip.clone()`, `user_agent.clone()`, `correlation_id.clone()` for *every* signal event, then `SignalEvent::rpc_call` likely allocates more. Then `try_send` pushes it through an mpsc.

Why it matters: signals are configured on by default. Even on a no-signals build (`#[cfg(feature = "gateway")]` off), `let kind = info.map(|i| i.kind.to_string())` still fires for the local span/observability path.

Fix:
- Add `FunctionKind::as_str() -> &'static str` and use it for the span attribute, `result_kind` matching, and observability. The `result_kind` is already `&'static str` in the success arms (`"query"`, `"mutation"`, etc.) but the error/timeout arms allocate via `i.kind.to_string()`.
- Build `RpcSignalContext` as `Arc<RpcSignalContext>` once and pass the `Arc` to `emit()` — currently `ctx.client_ip.clone()` deep-clones the `Option<String>` even when signals are dropped at `try_send` (buffer full).
- The visitor-id derivation runs SHA-256 on every emit even when the event will be dropped at the channel boundary. Compute lazily inside the collector worker, not at emit time.

---

## 6. Auth middleware validates JWTs on the request thread with no caching — High

Location: `crates/forge-runtime/src/gateway/auth.rs:360-482` (validate_token_async), called from `auth_middleware:666`.

Every request that carries a bearer token runs `jsonwebtoken::decode_header` + `decode::<Claims>`. For HMAC tokens this is HMAC-SHA256 over the whole token. For RSA tokens it's a JWKS lookup (potentially with network IO via `jwks.get_key(&kid).await`) plus RSA verification. There is no positive-result cache. Two clients spamming the same valid token = full crypto on every request.

Worse, `validate_hmac` (`:375-406`) does a full *scan* over `legacy_hmac_keys` whenever the `kid` doesn't match and the primary key fails verification — this is the *normal path* for tokens issued by other services that don't set kid. Each scan attempt does another HMAC verify.

Why it matters: HMAC-SHA256 on a typical JWT is ~5-15 µs but it's CPU on the request thread and it blocks the executor. At 50 k RPS with the same handful of tokens, you're burning ~0.5-1 full cores doing identical work. JWKS RSA verify is order-of-magnitude worse (200-500 µs each) and the JWKS cache here is on `kid` lookup, not on the token-to-claims mapping.

Fix:
- LRU cache keyed by `(blake3(token), config_epoch)` -> `Result<Claims, AuthError>` with a TTL tied to `claims.exp` minus a safety margin. Bound at ~10 k entries. The token itself shouldn't be the key (memory) — hash it.
- Skip the legacy-key scan when the token kid is set and matched the primary kid (the current code does this, but falls through on signature failure; the fallthrough should be cheap, not another scan).
- Make JWKS hot-key access lock-free: today every RSA verify hits the `jwks.get_key(&kid).await` path which (in `jwks.rs`) is presumably also locked.

---

## 7. `RpcHandler::handle` clones `RequestMetadata` for no reason — Medium

Location: `crates/forge-runtime/src/gateway/rpc.rs:130-143`.

```
.execute(&request.function, request.args, auth, metadata.clone())
.await
...
.with_request_id(metadata.request_id().to_string())
```

`metadata` is moved into `execute`, but the handler then needs `request_id` for the response so it clones the whole struct (5 fields including a `String` trace_id, `String` user_agent, `String` correlation_id, `String` client_ip, `chrono::DateTime`). Easy to fix: extract the request_id `Uuid` (Copy) before the move.

Why it matters: trivial per-call cost but it's on every single RPC request. With a 32-byte trace_id and 100-byte user agent that's ~200 bytes of allocation per call. At 50 k RPS, 10 MB/s of allocator chatter for no value.

Fix: `let request_id = metadata.request_id();` before passing `metadata` into `execute()`. Then `RpcResponse::success(value).with_request_id(request_id.to_string())`.

---

## 8. `args.clone()` for job/workflow fallthrough — Medium

Location: `crates/forge-runtime/src/function/router.rs:550, 576`.

```
job_dispatcher.dispatch_by_name(function_name, args.clone(), auth.principal_id())
```

When a function name doesn't match the registry, the router tries the job dispatcher with `args.clone()` and then if that returns NotFound, tries the workflow dispatcher with `args.clone()` again. So every job dispatch clones args once (consumed by dispatch), but every workflow dispatch clones args twice (the failed job attempt + the workflow). Worst case a not-found function clones args three times before erroring out.

Why it matters: most RPC payloads are small (KB-range), so the absolute cost is modest, but it's wasteful and points at a registry-lookup design problem. The router already knows whether a name is a function, job, or workflow at registration time but consults them sequentially at dispatch.

Fix: build a unified name → (Function | Job | Workflow) lookup at startup so dispatch is a single hashmap probe followed by a single move of `args` into the right handler. The `inventory` crate already gives you everything you need to do this at boot.

---

## 9. `SSE` bridge spawns *two* tokio tasks per session — Medium

Location: `crates/forge-runtime/src/gateway/sse.rs:601-679`.

Every SSE connection spawns:
1. A "bridge" task converting `RealtimeMessage` → `SseMessage`,
2. A "stream feeder" task converting `SseMessage` → `Event` and writing to an mpsc.

The events flow `reactor → rt_rx → tx → rx → event_tx → event_rx → axum Stream`. Four channels and two tasks per connection just to do format conversion that's a pure function.

Why it matters: at 10 k SSE sessions that's 20 k spawned tasks, 40 k channels (each `mpsc::channel(buffer_size=256)` pre-allocates the slot ring), and a lot of `tokio::select!` machinery. Memory per session is much higher than necessary.

Fix: merge the two tasks into one. Even better, implement `Stream` directly over the reactor's `rt_rx` with the conversion happening inline in `poll_next`. Removes two channels and one task per session. The `ReceiverStream<T>` wrapper at the top of the file already shows the pattern.

---

## 10. Middleware stack uses `from_fn_with_state` with `Arc<Vec<String>>` for quiet-paths — Medium

Location: `crates/forge-runtime/src/gateway/server.rs:644-647`, used in `tracing_middleware:922-1008`.

The quiet-paths list is rebuilt as `Arc::new(self.config.quiet_paths.clone())` on every `router()` call (fine — startup-only) but used like this per request:

```
let full_path = format!("/_api{}", path);          // allocates on every request
let is_quiet = quiet_paths.iter().any(|r| *r == full_path || *r == path);
```

Two equality comparisons per quiet-path entry per request, linear scan. With 10-20 quiet paths this is ~40-80 string comparisons per request — fine, not great, but the `format!("/_api{}", path)` allocates a `String` per request unconditionally.

Why it matters: every request pays this even when there are zero quiet paths configured.

Fix: precompute `HashSet<&'static str>` (or `HashSet<String>` once) and check `quiet_paths.contains(path) || quiet_paths.contains(full_path)`. Skip the `/_api` prepend by *removing* the `/_api` prefix from quiet paths at config parse time, so the runtime check is a single hash lookup against `path`.

---

## 11. Tower `ConcurrencyLimitLayer` + `TimeoutLayer` apply to all routes including SSE/health — Medium

Location: `crates/forge-runtime/src/gateway/server.rs:624-647`.

The service builder applies `ConcurrencyLimitLayer::new(self.config.max_connections)` (default 512) and `TimeoutLayer::new(Duration::from_secs(self.config.request_timeout_secs))` (default 30s) to the *merged* router (`:650`). This includes `/events` (SSE — long-lived), `/health`, `/ready`, `/subscribe`, the multipart upload route, and so on.

Consequences:
- SSE connections count against `max_connections`. With default 512 you can serve ~500 SSE clients and zero RPC headroom.
- The 30s timeout fires on SSE streams unless `KeepAlive` keeps the underlying response writer warm; long-poll patterns silently die.
- Health checks share the concurrency budget with RPC.

Why it matters: a busy gateway will start rejecting `/health` probes (LB marks node unhealthy) when the SSE-fleet fills the connection pool.

Fix: split SSE/health/ready out into a sub-router *before* the concurrency layer, or use a separate semaphore for the long-lived routes. Tower's `Steer` or just two `serve` calls on different routers.

---

## 12. `FunctionRegistry` lookups go through `HashMap<String, ...>` — Low

Location: `crates/forge-runtime/src/function/registry.rs:81` + `:155`.

Standard `HashMap<String, FunctionEntry>` keyed by string with default SipHash. Every dispatch does at minimum one `get(function_name)` here. Default hasher is DoS-resistant but slow; the function set is fixed at startup.

Why it matters: it's not on the critical path versus the JSON work, but it's measurable. Profiling will show ~50-100 ns per lookup.

Fix: use `ahash::AHashMap` or `foldhash` for the registry. Even better, since registration is one-shot at startup, build a perfect hash map (`phf` crate) or an interned `&'static str` keyset with linear probing. The string `function_name` from the request still has to be hashed once but the lookup table itself can be hash-free.

---

## 13. `MutationContext` carries an `Arc<dyn EnvProvider>` constructed per call — Low

Location: `crates/forge-core/src/function/context.rs:891, 917, 944, 983`.

Every `MutationContext::with_dispatch` / `with_env` / `with_transaction` does `env_provider: Arc::new(RealEnvProvider::new())`. Same for `QueryContext::new` at `:743`. A fresh `Arc` allocation per RPC call to wrap a stateless provider that reads from `std::env`.

Why it matters: small but unconditional. Roughly 32 bytes of heap per request just to wrap a zero-sized type.

Fix: store a static `Arc<dyn EnvProvider>` in a `OnceLock` and clone the `Arc` instead of allocating. Or change the field to `&'static dyn EnvProvider` and use `&RealEnvProvider` as a ZST static.

---

## 14. Workflow / job dispatcher `Arc::clone` per mutation construction — Low

Location: `crates/forge-runtime/src/function/router.rs:511-513, 666-668`.

Every non-cached mutation path clones `self.job_dispatcher.clone()`, `self.workflow_dispatcher.clone()`, `self.http_client.clone()`, and the token issuer `issuer.clone()` (`:515, 671`). The HTTP client is a `CircuitBreakerClient` which is presumably internally `Arc`-y, but each Arc clone is a relaxed atomic increment — cheap individually, but each mutation construction does 3-4 of them, plus another 3-4 in `execute_transactional`.

Why it matters: not a hot-path killer, but the design forces `MutationContext` to *own* these `Arc`s rather than borrow them. With ~6 atomic increments per request at 50 k RPS that's 300 k atomic ops/sec just on dispatcher refcount management.

Fix: `MutationContext` could borrow `&'a FunctionRouter`-style state (the router lives as long as the executor, the context lives only for the request). Lifetimes get harder; trade-off is real. At minimum, batch the issuer + dispatcher into a single `Arc<MutationDeps>` struct so it's one `Arc::clone`, not four.

---

## 15. Metrics record path allocates 4 `String`s per call via `KeyValue::new("function", function.to_string())` — Medium

Location: `crates/forge-runtime/src/observability/metrics.rs:42-50, 90-99, 167-174`.

Every `record_fn_execution` call materializes a `[KeyValue; 4]` array where each `KeyValue::new("function", function.to_string())` allocates a fresh `String`. Same for kind, status, path, method. That's 4 string allocations per RPC call, then again 3 per HTTP record, then again 1 per cache record — for *every* request, regardless of OTLP exporter being configured.

OpenTelemetry's API requires owned values here, but the function names and kinds are `&'static str`. `KeyValue::new` accepts `Cow<'static, str>` via `Value::from`, so `KeyValue::new("function", Cow::Borrowed(function))` (if function is `&'static str`) avoids the allocation.

Why it matters: at 50 k RPS, the metric layer alone is 200 k string allocations per second. This is the single highest source of allocator pressure in steady state per-request.

Fix:
- For `function_name`, register the function name as `&'static str` (already is on `FunctionInfo::name`). Pass it through as `&'static str` rather than `&str`. `KeyValue::new("function", function_name)` will then use `Cow::Borrowed` if you pass a `'static` lifetime — but the OTel SDK type signature may force a `String`. Verify with `opentelemetry::Value::from(&'static str)`.
- For dynamic strings (kind), pre-build a static map of `FunctionKind` → `KeyValue` arrays at startup.

---

## Top 3 fixes before GA

1. **Cut allocations from the cached-query path** (Issue #1 + #2). Move `Arc<Value>` end-to-end through `route()`/`execute()`/`RpcResponse`, and replace `serde_json::to_string` size-check with a counting serializer or single serialize-and-measure. These two together are the difference between "cache hits are free" and "cache hits triple-clone a megabyte". Highest leverage, smallest blast radius.

2. **Replace the SSE-session `RwLock<HashMap>` with `DashMap` + atomic counters** (Issue #4). At any non-trivial SSE fan-out the current design is the bottleneck. The fix is mechanical, the TOCTOU invariants are already documented, and the test surface barely changes. Without this, the 10 k SSE target is fiction.

3. **Cache JWT validation results** (Issue #6). Auth is the unconditional first thing every authenticated request does. With no positive cache, every request that reuses a still-valid token redoes the full crypto. An LRU keyed on `blake3(token)` with TTL = `min(exp, now + 60s)` removes the most predictable CPU cost in the entire stack. Combine with the metric-allocation fix (Issue #15) and per-request CPU drops by an integer factor.

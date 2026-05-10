# Bad Code Audit: Forge v2 Rewrite Phases 0-7

This document catalogs issues discovered during a deep audit of the Forge v2 rewrite from baseline to Phase 7. Each finding includes file, line numbers, severity, and expected behavior.

**Status: IN PROGRESS** — agents are appending findings as they audit each phase.

## Severity Legend

- **CRITICAL**: Security vulnerability, data loss, or correctness bug that breaks documented invariants
- **HIGH**: Scaling bottleneck, race condition, or significant design flaw
- **MEDIUM**: Sloppy code, design smell, or maintainability issue
- **LOW**: Style nit, minor inefficiency, or unclear naming
- **REGRESSION**: Behavior worse than v1 baseline

## Table of Contents

1. [Phase 0-1: Baseline + Cleanup](#phase-0-1-baseline--cleanup)
2. [Phase 2: Postgres Doctrine](#phase-2-postgres-doctrine)
3. [Phase 3: Runtime Core](#phase-3-runtime-core)
4. [Phase 4: Reactivity](#phase-4-reactivity)
5. [Phase 5: Jobs / Cron / Daemons / Workflows](#phase-5-jobs--cron--daemons--workflows)
6. [Phase 6: Gateway / Auth / MCP / Signals](#phase-6-gateway--auth--mcp--signals)
7. [Phase 7: Config / KV / Cache / Rate Limits](#phase-7-config--kv--cache--rate-limits)
8. [Cross-Cutting Concerns](#cross-cutting-concerns)
9. [Regressions vs v1](#regressions-vs-v1)

---

## Phase 0-1: Baseline + Cleanup

<!-- AGENT_PHASE_0_1_START -->
### Findings

### [CRITICAL] `dev_mode()` silently demotes instead of failing closed in production

**File:** `crates/forge-runtime/src/gateway/auth.rs:97-125`

**Issue:** When `FORGE_ENV=production` is set, `AuthConfig::dev_mode()` logs an error and returns a default config with `skip_verification = false`. Per the security carry-forward (item 3), the v2 plan explicitly requires "FAIL CLOSED — refuse to start if `skip_verification = true` and `FORGE_ENV = production`. v2 strengthening: not just refuse to enable — refuse to even parse the config. Make it a startup error with a loud log line."

**Expected:** Return `Result<Self, ForgeError::Config>` and refuse to construct (fail closed). Or panic loudly. Silent demotion violates fail-closed.

**Impact:** A misconfigured deployment thinks dev mode is on but actually has the default secret-less config which itself is broken; this silent fallback masks misconfiguration. The startup must abort, not auto-correct.

### [CRITICAL] PKCE comparison still uses hand-rolled XOR; subtle crate not adopted

**File:** `crates/forge-core/src/oauth/pkce.rs:27-36`

**Issue:** Item 33 (and item 9) of the security carry-forward explicitly mandates `subtle::ConstantTimeEq` for PKCE verifier comparison. The code still uses a hand-rolled byte XOR loop with an early-return on length mismatch (which itself is *not* constant-time — leaks length).

**Expected:** `use subtle::ConstantTimeEq; computed.as_bytes().ct_eq(code_challenge.as_bytes()).into()`. The subtle crate is not even in `Cargo.toml` workspace deps.

**Impact:** Timing oracle reveals code_challenge byte by byte. Combined with intercepted authorization codes, breaks PKCE protection.

### [CRITICAL] CSRF validation uses non-constant-time `==` comparison

**File:** `crates/forge-runtime/src/gateway/oauth.rs:527-532`

**Issue:** `cookie_csrf == form.csrf_token` is a plain `String == String` equality check that short-circuits on first mismatch. Item 33 requires constant-time comparison for security tokens.

**Expected:** Use `subtle::ConstantTimeEq` or hmac's `verify_slice` semantics for CSRF token comparison.

**Impact:** Timing-based CSRF token recovery (theoretically) — small risk but the audit specification explicitly forbids `==` here.

### [CRITICAL] Legacy JWT secrets have no TTL-bound retirement

**File:** `crates/forge-core/src/config/auth.rs:86-91`, `crates/forge-runtime/src/gateway/auth.rs:261-270`

**Issue:** The security carry-forward (item 6) requires "v2 ADDS: TTL-bound retirement (each legacy key has a `valid_until` timestamp; expired ones are silently dropped)". Current implementation: `legacy_secrets: Vec<String>` — no per-secret TTL.

**Expected:** `legacy_secrets: Vec<LegacySecret { secret, valid_until }>` with automatic expiry.

**Impact:** Old keys never auto-retire. Operators forget to remove them, defeating the rotation purpose. An old secret leaked years ago still validates tokens forever.

### [HIGH] JWT issued by Forge does not include `kid` header

**File:** `crates/forge-runtime/src/gateway/auth.rs:209-219`

**Issue:** `HmacTokenIssuer::sign` uses `jsonwebtoken::Header::new(self.algorithm)` without setting a `kid`. The security carry-forward (items 6 and 43) require "v2: every JWT issued by Forge includes a `kid` header so clients can disambiguate during rollover."

**Expected:** Header includes a stable kid derived from the secret (e.g., short hash of the secret bytes).

**Impact:** Clients cannot disambiguate between active vs legacy secrets during rollover; rotation feature is half-broken.

### [CRITICAL] Webhook Ed25519 and HmacSha256 paths have no replay window

**File:** `crates/forge-runtime/src/webhook/handler.rs:370-392, 448-478`

**Issue:** `validate_signature` dispatches `Ed25519`, `HmacSha256`, and `HmacSha256Base64` to validators that take only the body and signature — there is no timestamp parsing and no replay window. Only `validate_stripe_webhooks` enforces a 300s window (line 421). Security carry-forward item 12 mandates a configurable replay window (default 300s) **for all signature schemes**.

**Expected:** Every signature scheme reads a `WebhookSignature::*` timestamp header (or designated body field) and rejects requests outside `replay_window_secs`. Config field needs to live on `WebhookSignature` / `SignatureConfig`.

**Impact:** Captured Ed25519 / HMAC requests can be replayed forever. v1 had this; v2 lost it for non-Stripe schemes.

### [HIGH] `replay_window_secs` config absent from `SignatureConfig`

**File:** `crates/forge-core/src/webhook/signature.rs` (and `crates/forge-macros/src/webhook.rs`)

**Issue:** No `replay_window_secs` field exists on `SignatureConfig` or `WebhookSignature`, no macro attribute parses it, and no field reaches the runtime. The carry-forward calls this out as a v2 must-have alongside the actual replay enforcement.

**Expected:** Add `replay_window_secs: Option<u64>` (default 300) to the signature config, surface it via the `#[webhook(...)]` attribute, and pipe it into `validate_signature` so each scheme can apply it.

**Impact:** Even if the validators were patched, operators have no knob to tune or harden the window.

### [REGRESSION] `webhook/handler.rs` allowlisted in raw SQL lint and uses runtime `sqlx::query()`

**File:** `scripts/ci/lint-raw-sql.sh:29` and `crates/forge-runtime/src/webhook/handler.rs:526, 555, 576`

**Issue:** Item 17 of the security carry-forward and the project doctrine demand compile-time SQL via `sqlx::query!`. The webhook handler instead issues three runtime `sqlx::query(...)` calls against `forge_webhook_events` (a system table that lives in the .sqlx cache today), and the lint script papers over it by adding `webhook/handler.rs` to `ALLOWED_FILES`.

**Expected:** Convert all three queries to `sqlx::query!` and remove `webhook/handler.rs` from `ALLOWED_FILES`. The schema is well-known — there is no excuse for runtime queries here.

**Impact:** Schema drift, typo bugs, and column-rename bugs ship undetected. The lint allowlist is now load-bearing, which is the opposite of the v2 doctrine.

### [HIGH] `lint-raw-sql.sh` regex misses comment styles and over-filters

**File:** `scripts/ci/lint-raw-sql.sh:42-50`

**Issue:** Comment detection uses `^\s*(//|/\*\*)`, missing `//!` module docs, `/*` block comments, and any inline `// ... sqlx::query(...)` that prefixes the call. The pre-grep filter `| grep -v 'sqlx::query!' | grep -v 'sqlx::query_as!'` drops *the entire line* if any macro form appears anywhere in it, so a line that contains both `sqlx::query!()` and `sqlx::query()` is silently passed.

**Expected:** Use a Rust-aware tooling step (clippy lint or syn-based scanner) or, at minimum, anchor the macro check to the actual matched call and broaden the comment regex.

**Impact:** Accidental runtime SQL slips past CI under the very files we're allowlisting.

### [REGRESSION] Batch RPC endpoint still wired up despite removal in spec

**File:** `crates/forge-runtime/src/gateway/server.rs:34, 496` and `crates/forge-runtime/src/gateway/rpc.rs:242`

**Issue:** `.agents/rewrite/04-WIRE-PROTOCOL.md:137-146` explicitly states "Batch RPC — REMOVED" with rationale (HTTP/2 multiplexing, atomicity confusion). The runtime still imports `rpc_batch_handler` and registers `POST /_api/rpc/batch`. The handler module still exists.

**Expected:** Delete `rpc_batch_handler`, remove the route, drop the `requests`/`results` envelope types, and clean up any imports.

**Impact:** Pre-1.0 doctrine says "no compat shims, no old way / new way coexistence" — having the deprecated endpoint live in v2 contradicts the rewrite plan.

### [HIGH] Wire-protocol docs document removed batch endpoint and wrong subscription paths

**File:** `docs/docs/reference/wire-protocol.mdx:5-19, 53-76`

**Issue:** The endpoint table lists `POST /_api/rpc` for "Batch call" (which the spec removed) and `POST /_api/subscribe/job/{job_id}` (actual route is `POST /_api/subscribe-job` per `gateway/server.rs:533`). The multipart upload route `/_api/rpc/{function}/upload` is missing from the table.

**Expected:** Match the table to the actual registered routes once batch is removed. Add the upload endpoint. Drop the batch-call section.

**Impact:** Public docs lie about the wire shape; integrators following them will hit 404s and ship batch clients that v2 doesn't support.

### [REGRESSION] `js_sys::eval` still used in forge-dioxus signals tracker

**File:** `packages/forge-dioxus/src/signals.rs:746, 954`

**Issue:** Two `js_sys::eval(...)` calls remain (history-pushState patcher and web-vitals factory loader), despite progress tracker claiming `js_sys::eval` was removed. Item 33 of the carry-forward bans dynamic eval from frontend runtimes; arbitrary string-evaluated JavaScript is a CSP nightmare and an XSS surface.

**Expected:** Replace both with `wasm_bindgen` JS imports (`#[wasm_bindgen(inline_js = ...)]` pre-compiled at build time) or remove the auto-instrumentation entirely.

**Impact:** Strict CSP (`script-src 'self'`) breaks the analytics tracker; users embedding Forge into hardened pages fail at runtime.

### [HIGH] `cargo audit` runs without `--deny warnings` flag

**File:** `.github/workflows/ci.yml:80-83`

**Issue:** Carry-forward item 36 says cargo-audit must fail the build on any warning, not just on critical advisories. Today it runs `cargo audit` with no flags so unmaintained / yanked / RUSTSEC notices return exit code 0.

**Expected:** `cargo audit --deny warnings` (or `--deny unmaintained --deny yanked --deny notices`).

**Impact:** Yanked or unmaintained crates can silently land on `main` without breaking CI.

### [HIGH] `guardrails` job is not a dependency of integration jobs

**File:** `.github/workflows/ci.yml:59-83, 87, 118, 133`

**Issue:** `guardrails` (cargo deny, raw SQL lint, cargo audit) depends on nothing, and no later job has `needs: [validate, guardrails]`. `workspace-integration`, `pr-smoke`, `integration` only need `validate`. So a PR with a banned dependency or a yanked crate can still merge if validate passes.

**Expected:** Add `guardrails` to `needs:` of every job that gates merging, or set required-status-checks in branch protection.

**Impact:** Security guardrails are advisory, not enforced — defeats the point.

### [MEDIUM] `deny.toml` uses deprecated cargo-deny v0.16+ schema and lacks `version`

**File:** `deny.toml:4-8, 11`

**Issue:** Keys `vulnerability`, `unmaintained`, `yanked`, `notice` under `[advisories]`, and `unlicensed` under `[licenses]`, are deprecated in cargo-deny v0.16. New schema uses `version = 2` and a different shape (e.g. `[licenses] allow = [...]`, `[advisories] yanked = "deny"`). Running fresh cargo-deny prints warnings and may drop entire keys silently.

**Expected:** Migrate to v2 schema and add `version = 2` at the top.

**Impact:** Future cargo-deny upgrades will silently disable rules without breaking builds, eroding the guardrail.

### [MEDIUM] `IdempotencySource::parse` is a public API only used by tests

**File:** `crates/forge-core/src/webhook/signature.rs:65-74, 210-219`

**Issue:** The function `Box::leak`s strings (intentionally — for `'static` lifetimes). It's `pub` and exposed in the API surface, but the only callers are this module's own tests. Pre-1.0 doctrine: no dead public API.

**Expected:** Either downgrade to `pub(crate)` and call it from the macro / runtime where it belongs, or delete it entirely (the macros can build `IdempotencySource` directly).

**Impact:** Leaks API surface and an `unsafe`-adjacent leak primitive (`Box::leak`) that downstream users could call in a hot path.

### [HIGH] Webhook macro signature parsing uses fragile substring matching

**File:** `crates/forge-macros/src/webhook.rs:50-66`

**Issue:** `parse_signature_from_attr_str` calls `remaining.contains("hmac_sha256")`, `contains("stripe_webhooks")`, etc. Any user comment, identifier, secret env name, or doc string containing these substrings flips the algorithm classification. Example: `WebhookSignature::ed25519("X-Sig", "MY_HMAC_SHA256_KEY_NAME")` parses as HmacSha256 because the env name contains `hmac_sha256`.

**Expected:** Parse with `syn` (this is a Rust expression in the attribute) and match on the real path/method.

**Impact:** Misclassified algorithm at compile time → wrong validator at runtime → signatures silently reject (or, worse, accept) traffic.

### [MEDIUM] `change_tracking_row_threshold` is dead config

**File:** `crates/forge-core/src/config/realtime_config.rs:50-55, 101-103, 130`

**Issue:** No code reads `change_tracking_row_threshold` (or its `adaptive_row_threshold` alias) anywhere in the workspace. The adaptive tracker was removed but the knob — including its serde alias — was left in `RealtimeConfig`.

**Expected:** Delete the field, the default fn, the alias, and the entry from `RealtimeConfig::default()`. Mention the removal in CHANGELOG.

**Impact:** Operators who set this in `forge.toml` get silent no-op behavior. Pre-1.0 doctrine: zero tech debt.

### [LOW] Macro doc comment still references removed `WebhookSignature::hmac_sha1`

**File:** `crates/forge-macros/src/lib.rs:350`

**Issue:** Doc comment lists `WebhookSignature::hmac_sha1("X-Signature", "SECRET") - Legacy SHA1` as a constructor option, but `HmacSha1` was removed from `SignatureAlgorithm` in Phase 0-1.

**Expected:** Drop the `hmac_sha1` line. Optionally replace with a note that SHA1 is unsupported.

**Impact:** Misleads developers; copy-paste from doc fails to compile.

### [MEDIUM] `docker-compose.yml` lacks restart policy, shm_size, and binds to all interfaces

**File:** `docker-compose.yml:1-19`

**Issue:** No `restart: unless-stopped`, no `shm_size: 1gb` (Postgres benchmarks misbehave), and `ports: "5432:5432"` binds to `0.0.0.0` so any other process / VM on the dev box (or any laptop on a public WiFi) can reach the dev DB.

**Expected:** `ports: "127.0.0.1:5432:5432"`, add `shm_size: 1gb`, add `restart: unless-stopped`. Optionally drop the published port entirely and only expose to a `forge_net` network.

**Impact:** Dev DB exposed to LAN; dev environments occasionally OOM under load; container exits don't auto-restart.

### [LOW] `dev_mode` vs `skip_verification` semantics still ambiguous

**File:** `crates/forge-runtime/src/gateway/auth.rs:97-125` and `crates/forge-core/src/config/auth.rs:80-100`

**Issue:** Even setting aside the silent-demotion bug above, two adjacent flags (`skip_verification`, `dev_mode`) overlap in meaning. v2's stated direction is one explicit `mode: Production | Development` enum that the type system can exhaust on.

**Expected:** Replace the booleans with `enum AuthMode { Production, Development }` parsed once at startup; production refuses skip_verification at parse time, period.

**Impact:** Operator confusion ("which one do I set?"), and the boolean pair cannot be made invariant by the type system.

### Summary: 21 findings (5 CRITICAL, 7 HIGH, 4 MEDIUM, 2 LOW, 3 REGRESSION)

<!-- AGENT_PHASE_0_1_END -->

---

## Phase 2: Postgres Doctrine

<!-- AGENT_PHASE_2_START -->
### Findings

### [CRITICAL] Fencing-token column kept in `forge_leaders` and incremented on every acquisition

**File:** `crates/forge-runtime/migrations/system/v001_initial.sql:28-38`, `crates/forge-runtime/src/pg/leader.rs:114-140`

**Issue:** Phase 2 explicitly removes fencing tokens. The migration still creates `forge_leaders.term BIGINT NOT NULL DEFAULT 0` (with the comment "term is a fencing token...") and `LeaderElection::try_become_leader` still does `term = forge_leaders.term + 1` on the `ON CONFLICT` branch and `RETURNING term as "term!: i64"`. The whole point of "no fencing tokens" was that the advisory lock alone is the source of truth. Bumping a counter on every leader acquisition is the exact pattern that was supposed to disappear; leaving it in the schema invites callers to read it and add false-confidence checks.

**Expected:** Drop `term` from `forge_leaders` (or drop the table outright and synthesise leader info from `pg_locks`). Remove the `term + 1` arithmetic and `RETURNING term` from the INSERT. Use a single source of truth: the advisory lock.

**Impact:** Plan-violating dead column. Any future code that re-introduces a fence "because the column is there" is a footgun.

### [CRITICAL] Cron scheduler still carries dead fencing-term SQL

**File:** `crates/forge-runtime/src/cron/scheduler.rs:299-359`

**Issue:** `try_claim` builds its INSERT with `WHERE ($6::bigint) = -1 OR EXISTS (SELECT 1 FROM forge_leaders ... AND term = $6)` and binds a hard-coded `fence_term: i64 = -1`. This is a dead branch that exists only because Phase 2 left the column in place. The check is never meaningful (sentinel always picks the `-1` arm), but the fact that the cron scheduler still references `forge_leaders.term` defeats the deletion in spirit.

**Expected:** Delete the fence parameter, the `($6::bigint) = -1 OR ...` predicate, and the dependency on `forge_leaders.term`. Claim ownership through the advisory lock + `forge_cron_executions` UNIQUE constraint and nothing else.

**Impact:** Confusing dead code; readers will assume the fence does something. Prevents schema simplification (cannot drop `term` while this query still references it).

### [HIGH] `forge_migrations` table has wrong shape vs. plan

**File:** `crates/forge-runtime/src/migrations/runner.rs:239-258, 322-339, 460-498`, plus comment in `CLAUDE.md` describing the schema

**Issue:** Plan calls for `forge_system_migrations(version VARCHAR PRIMARY KEY, applied_at, checksum)`. Instead the runner creates `forge_migrations` with `id SERIAL PRIMARY KEY`, `version VARCHAR DEFAULT ''`, `name VARCHAR UNIQUE`, `execution_time_ms`, `down_sql` and binds `migration.name` to *both* `version` and `name` slots. The `version` column is effectively dead (default empty string), uniqueness is enforced on `name` not `version`, and the helper drags around `down_sql`/`execution_time_ms` columns that the doctrine doesn't ask for.

**Expected:** Rename to `forge_system_migrations`, drop the SERIAL PK, make `version` the PRIMARY KEY (TEXT NOT NULL), keep just `applied_at` + `checksum`. Bind `migration.version` (parsed from filename, e.g. `v001`) to the version column, not the name.

**Impact:** Schema drift from the design doc; future doctrine changes (re-running by `(version, checksum)` mismatch, etc.) collide with the existing `name` UNIQUE constraint.

### [HIGH] Migration runner uses runtime SQLx APIs instead of compile-time macros

**File:** `crates/forge-runtime/src/migrations/runner.rs:240, 308, 324, 467`

**Issue:** `ensure_migrations_table`, `apply_migration`, `record_migration`, and `status` all use `sqlx::query(...)` / `sqlx::query_as::<_, (...)>(...)` runtime builders. Workspace rule (CLAUDE.md): "Always use `sqlx::query!()` and `sqlx::query_as!()` macros for compile-time checking. Never use runtime `sqlx::query()` or `sqlx::query_as::<_, T>()`." The runner is the one place we *can* afford to use the macros (the table shape is fixed); doing it in raw mode skips the compile-time check that the doctrine cares about.

**Expected:** Convert all four call sites to `sqlx::query!`/`sqlx::query_as!`. The bootstrap CREATE TABLE can stay in a `.sql` migration file and be applied via macro-checked statements.

**Impact:** Loses the compile-time safety net the rest of the workspace enforces. A schema typo here only fails at runtime in production.

### [HIGH] `DatabaseConfig` still exposes `jobs`/`observability`/`analytics` pool slots

**File:** `crates/forge-core/src/config/database.rs:50-156`

**Issue:** Phase 2 collapses to one pool; `PoolsConfig` continues to ship `jobs: Option<PoolConfig>`, `observability: Option<PoolConfig>`, `analytics: Option<PoolConfig>` with full `min/max/idle/timeout` structures. They are silently ignored at runtime. Operators reading the docs will assume isolation works and configure these pools, then be surprised when the workload all hits the default pool.

**Expected:** Delete `PoolsConfig`'s isolated pool fields. Keep only `default: PoolConfig`. Reject unknown keys via `#[serde(deny_unknown_fields)]` so an old `[database.pools.jobs]` block fails loudly during config load.

**Impact:** Operator confusion, false sense of pool isolation, dead config surface.

### [HIGH] `forge/runtime.rs` still aliases `pool.clone()` as `jobs_pool`/`observability_pool`

**File:** `crates/forge/src/runtime.rs:378-379, 554-688, 1124`

**Issue:** Even though only one pool exists now, the orchestration in `runtime.rs` keeps `let jobs_pool = pool.clone(); let observability_pool = pool.clone();` and threads those aliases into 11+ subsystem builders. This recreates the four-pool mental model in code shape: future readers will think isolation matters, and the next person who tries to "make jobs use a separate pool" will reintroduce the rejected design without touching the call sites.

**Expected:** Drop the aliases. Pass `pool` directly. If a subsystem genuinely needs its own pool later, that decision should be loud and explicit.

**Impact:** Misleading code shape; the file pretends the four-pool design is still alive.

### [HIGH] Advisory lock refresh never validates the lock is still held via `pg_locks`

**File:** `crates/forge-runtime/src/pg/leader.rs:151-175`

**Issue:** Phase 2 explicitly says "fix advisory lock refresh: validate via `pg_locks`." The current `refresh_lease` blindly UPDATEs `forge_leaders.lease_until` without checking whether the lock is still held by this session. If the connection underlying `lock_connection` was reset (e.g. PG terminated the backend, sqlx reconnected to a fresh socket on a later acquire), the advisory lock is gone but the lease keeps refreshing — split brain.

**Expected:** Before each refresh, run `SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND objid = $1 AND pid = pg_backend_pid())` *on the same connection* that holds the lock. Bail out (drop leadership, log loud, alert) if the row is gone.

**Impact:** Two leaders can run cron/scheduler logic simultaneously after a transient PG hiccup, which is the headline failure mode the doctrine was supposed to remove.

### [HIGH] `try_become_leader` records the leader row using a *different* connection from the one holding the lock

**File:** `crates/forge-runtime/src/pg/leader.rs:97-149`

**Issue:** Lock is acquired on `let mut conn = self.pool.acquire().await?` via `pg_try_advisory_lock` and that connection is parked in `self.lock_connection`. The follow-up `INSERT INTO forge_leaders ... ON CONFLICT DO UPDATE` runs against `&self.pool`, which checks out a *different* connection. If the lock-holding connection dies between the lock acquire and the INSERT, the lock is released by PG but the INSERT still succeeds, leaving a leader row pointing at a node that holds nothing. The fix is trivial: run the INSERT on the same `conn` (in a transaction with a lock-presence check).

**Expected:** Reuse `&mut *conn` for the INSERT; or wrap the lock-acquire and the INSERT inside one connection scope and verify `pg_locks` before committing the INSERT.

**Impact:** Stale `forge_leaders` rows after connection blips; `get_leader()` will lie about who the leader is.

### [HIGH] `MigrationRunner::run` blocks indefinitely on `pg_advisory_lock`

**File:** `crates/forge-runtime/src/migrations/runner.rs:209-223`

**Issue:** `acquire_lock_connection` calls the *blocking* `pg_advisory_lock`, not `pg_try_advisory_lock_timeout` or a polled try. If a previous deploy is mid-migration, the new deploy will block on the lock with no timeout, no log, no graceful failure path. CI will time out without telling you why.

**Expected:** Use `pg_try_advisory_lock` in a loop with a configurable overall deadline (e.g. 5 minutes) and emit a `WARN` log every 30s with the holder's PID via `pg_locks`.

**Impact:** Deploys hang silently; debugging requires PG-side `pg_stat_activity` digging.

### [HIGH] User migrations wrapped in `pool.begin()`, breaks `CREATE INDEX CONCURRENTLY` and other DDL that cannot run in TX

**File:** `crates/forge-runtime/src/migrations/runner.rs:271-350`

**Issue:** `apply_migration` always opens a transaction, runs `SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='5min'`, then executes the user's SQL. Several common Postgres operations are explicitly disallowed inside a transaction: `CREATE INDEX CONCURRENTLY`, `ALTER TYPE ... ADD VALUE` on existing types, `VACUUM`, `REINDEX CONCURRENTLY`. Forcing every user migration into a TX makes those operations impossible without contortions.

**Expected:** Per-migration metadata flag (e.g. `-- @transactional false` directive parsed by the loader). When non-transactional, run statements directly on a checked-out connection with the same SET LOCAL knobs but no surrounding `BEGIN`.

**Impact:** Forces users into anti-patterns (drop and rebuild indexes inside locked TXs) for any sizeable production migration.

### [MEDIUM] Migration re-run detection keys on `name` not `(version, checksum)`

**File:** `crates/forge-runtime/src/migrations/runner.rs:142-199, 261-269`

**Issue:** `applied_versions` returns a `HashSet<String>` of names; `apply_migration` skips when the name is present. Plan asks for `(version, checksum)` semantics so an edited migration body fails fast. Currently editing a migration in-place + redeploying silently no-ops.

**Expected:** Compare `(version, checksum)`; on checksum mismatch error out with the diff, not silently skip.

**Impact:** Schema drift between environments goes undetected; rollbacks become guesswork.

### [MEDIUM] `forge_migrations` bootstrap is in Rust, not in `system/v001_initial.sql`

**File:** `crates/forge-runtime/src/migrations/runner.rs:239-258`

**Issue:** `ensure_migrations_table` runs a Rust-side `CREATE TABLE IF NOT EXISTS` before the system migration applies. This bypasses the migration runner's own contract: the table the runner consults is created outside the runner. A test that drops the database and applies system migrations from scratch ends up with two creation paths (Rust + SQL), one of which is authoritative.

**Expected:** Put the table definition in `system/v000_bootstrap.sql` (or fold it into v001) and have the runner only INSERT/SELECT against it. The runner's first action should be `SELECT 1 FROM forge_system_migrations LIMIT 0` and fail loudly if the table is missing.

**Impact:** Two truths for the same table; future schema changes have to be made in two places.

### [MEDIUM] No `validate_identifier` helper for dynamic SQL identifiers

**File:** `crates/forge-runtime/migrations/system/v001_initial.sql:286-312`, runtime callers TBD

**Issue:** Phase 2 calls for a `validate_identifier()` Rust helper used by every code path that interpolates a table/column name into SQL. The reactivity DDL `forge_enable_reactivity(table_name TEXT)` uses `format('%I', table_name)` (which quotes correctly) but there is no Rust-side allow-list that prevents callers from passing arbitrary user input. Without the helper, every new caller has to remember to whitelist on its own.

**Expected:** Add `pub fn validate_identifier(s: &str) -> Result<&str>` to `pg/mod.rs` matching `^[a-z_][a-z0-9_]{0,62}$` and require all dynamic-identifier sites to call it.

**Impact:** Soft injection risk if a future caller forgets PG's `%I`. Locks the team into ad-hoc validation per call site.

### [MEDIUM] `release_leadership` ignores the result of `pg_advisory_unlock`

**File:** `crates/forge-runtime/src/pg/leader.rs:177-216`

**Issue:** `pg_advisory_unlock` returns `false` if the session never held the lock; that's a meaningful signal during shutdown (we lost the lock earlier and didn't notice). The code uses `query_scalar!` without inspecting the bool, then proceeds to delete the row regardless.

**Expected:** Branch on the unlock result; if `false`, log at `WARN` (or `ERROR`) and skip the row delete since some other node likely owns it now.

**Impact:** Releases other nodes' leader rows after a stale unlock no-op, briefly leaving the cluster leaderless until the next election cycle.

### [MEDIUM] One-pool concurrency model has no priority/throttling story

**File:** `crates/forge-runtime/src/pg/pool.rs` (entire file)

**Issue:** Phase 2 mandates a single pool; that's accepted. But the pool currently has no per-workload classifier (jobs vs gateway vs observability). A burst of slow jobs can trivially starve gateway requests because both reach for the same finite connection budget. Doctrine still demands a single pool, but the runtime has to provide *some* primitive (semaphore? bounded queue per workload?) that prevents starvation.

**Expected:** Wrap connection acquisition in workload-tagged semaphores so the gateway has a guaranteed reservation. Or document the policy choice loud (one pool, no isolation, here's the contention model) so it's a deliberate trade-off.

**Impact:** First production load test will surface gateway timeouts under job load; operators have no knob.

### [MEDIUM] `LeaderConfig` lacks a knob for `pg_locks` validation interval

**File:** `crates/forge-runtime/src/pg/leader.rs:18-25, 266-307`

**Issue:** Tied to the validation gap in finding above. Even once `pg_locks` validation is added, the loop uses `check_interval` for everything (refresh, lock validate, leader-health). They should be tunable separately so a long lease (60s) can still be validated quickly (1s).

**Expected:** `LeaderConfig { check_interval, lease_duration, lock_validate_interval }`.

**Impact:** No fine-grained control over how fast we detect a lost lock.

### [LOW] `LeaderGuard` doesn't actually own anything

**File:** `crates/forge-runtime/src/pg/leader.rs:50, 311-330`

**Issue:** It's just `&'a LeaderElection` plus a `try_new`. No `Drop`, no scoped behavior, no compile-time guarantee that the user can't do leader-only work outside of it. The plan called this an RAII guard; today it's a `bool` wearing a hat.

**Expected:** Either delete it (call `is_leader()` directly) or make it actually take an `&mut PoolConnection` representing the lock-holding connection so leader-only DB work cannot bypass the lock.

**Impact:** Misleading abstraction; provides false confidence to readers.

### [LOW] `Database::from_pool` is gated behind `#[cfg(test)]`

**File:** `crates/forge-runtime/src/pg/pool.rs:248-257`

**Issue:** Several legitimate non-test consumers (CLI helpers, integration test harnesses outside the crate) cannot construct a `Database` from an existing pool. Hiding the constructor behind `cfg(test)` forces hacks like spinning a second pool just to wrap one.

**Expected:** Make it `pub(crate)` or `pub` with a `#[doc(hidden)]` marker. Don't gate on test config.

**Impact:** Test infrastructure friction; harnesses end up duplicating pool config.

### [LOW] `start_health_monitor` has no shutdown channel

**File:** `crates/forge-runtime/src/pg/pool.rs:219-246`

**Issue:** The replica health monitor task spawns and never stops. On graceful shutdown the task keeps running until process exit, holding a clone of the pool open. Minor leak in tests, irritation in docker-compose teardown.

**Expected:** Accept a `watch::Receiver<bool>` (or a `CancellationToken`) and terminate cleanly when shutdown is requested.

**Impact:** Slow test teardown, potential connection-keepalive races.

### [LOW] `ForgePool` newtype duplicates the role of `Database`

**File:** `crates/forge-core/src/db.rs` (entire file, 47 lines)

**Issue:** `forge-core` defines `ForgePool(Arc<sqlx::PgPool>)` while `forge-runtime::pg::Database` is the canonical wrapper. Two truths means callers have to know which to thread through. The doctrine consolidates on one pool; the type system should mirror that with one type.

**Expected:** Delete `ForgePool` (or make it a transparent re-export of `Database`); update call sites accordingly.

**Impact:** Conceptual noise; double accounting in API surface.

### [REGRESSION] `pg/migration.rs` is a re-export-only stub

**File:** `crates/forge-runtime/src/pg/migration.rs:1-6`

**Issue:** Phase 2 calls for the migration runner to live *under* `pg/`. The actual runner is still in `crates/forge-runtime/src/migrations/runner.rs` (945 lines) and `pg/migration.rs` is `pub use crate::migrations::runner::*;`. The work was renamed but not actually moved.

**Expected:** Move the file. Delete `crates/forge-runtime/src/migrations/`. All public types live under `forge_runtime::pg::migration`.

**Impact:** False signal of completion; the doctrine-shaped layout is one search away from being undone.

### [REGRESSION] No `NotifyChannel<T>` typed wrapper around `LISTEN`/`NOTIFY`

**File:** `crates/forge-runtime/src/pg/` (missing module)

**Issue:** Phase 2 doctrine is "centralize Postgres primitives in `forge-runtime::pg` including `NotifyChannel<T>`." There is no such type; reactivity, jobs, and the cron runner each call `pg_notify` / `PgListener` directly with bespoke serialization. Without a typed channel that takes `T: Serialize + DeserializeOwned`, payload size limits (8 KiB) and JSON contract drift have to be re-discovered per call site.

**Expected:** `pub struct NotifyChannel<T> { name: &'static str, _marker: PhantomData<T> }` with `publish(&self, conn, payload: &T)` and `subscribe(&self, listener) -> impl Stream<Item = T>`. Enforce `serde_json::to_string(payload).len() <= 7 * 1024` (leave headroom for PG framing) at compile/runtime; for larger payloads, write to `forge_change_log` and emit just the row id in the notify body.

**Impact:** Every consumer reinvents serialization, payload-size guards, and listener loop ergonomics. Drift between sites is inevitable.

### [REGRESSION] Change-log recovery helpers missing from `pg/`

**File:** `crates/forge-runtime/src/pg/` (missing module)

**Issue:** Plan calls for change-log recovery helpers in `pg/`. The `forge_change_log` table appears only in v002+ user migrations; there are no Rust helpers in `forge-runtime::pg` that drain or trim the table. Reactivity reads it ad-hoc in `realtime/`. Without centralised helpers, every consumer that needs at-least-once semantics reimplements polling, deduplication, and trim.

**Expected:** `pub fn drain_change_log(pool, since: i64) -> impl Stream<Item = ChangeRow>` and `pub fn trim_change_log(pool, before: DateTime<Utc>)` colocated with `NotifyChannel`.

**Impact:** Reactivity recovery logic is duplicated/missing; payload-size workaround story is incomplete.

### Summary: 21 findings (2 CRITICAL, 8 HIGH, 5 MEDIUM, 3 LOW, 3 REGRESSION)

<!-- AGENT_PHASE_2_END -->

---

## Phase 3: Runtime Core

<!-- AGENT_PHASE_3_START -->
### Findings

### [CRITICAL] OutboxBuffer pattern still in use; plan explicitly rejected this

**File:** `crates/forge-core/src/function/context.rs:451-463, 1247-1393`, `crates/forge-runtime/src/function/router.rs:768-849`

**Issue:** The Phase 3 plan says: "MutationContext::dispatch_job and start_workflow insert into mutation's `&mut Transaction<'_, Postgres>`. NOTIFY publishes happen post-commit." It also explicitly calls out OutboxBuffer/`flush_outbox` as the rejected pattern. The code keeps the entire OutboxBuffer machinery: `OutboxBuffer { jobs, workflows }` plus an `Arc<Mutex<OutboxBuffer>>` plumbed through `MutationContext`, plus a flush loop in `execute_transactional` (router.rs:818-835) that walks the buffer and runs INSERTs *after* the handler returns. The plan asked for the handler to call sqlx directly against `&mut Transaction`; this is the exact "buffered + late flush" shape the plan wanted gone.

**Expected:** `dispatch_job`/`start_workflow` should write `INSERT INTO forge_jobs ...` against the live transaction handle inside the call, not push a `PendingJob` onto a `Mutex<OutboxBuffer>` for replay. Delete `OutboxBuffer`, `PendingJob`, `PendingWorkflow`, `flush`-style logic.

**Impact:** The supposed simplification didn't happen. Two divergent enqueue paths remain (the buffer-flush in router.rs vs. `JobQueue::enqueue` in queue.rs), and the differences below cause real bugs.

### [CRITICAL] Outbox-flushed jobs lose idempotency_key, dedup, and ON CONFLICT DO NOTHING

**File:** `crates/forge-runtime/src/function/router.rs:851-881` vs `crates/forge-runtime/src/jobs/queue.rs:136-202`

**Issue:** `insert_job` in router.rs is a fresh hand-rolled INSERT that hardcodes `attempts = 0` and **omits `idempotency_key` entirely**. `JobQueue::enqueue` in queue.rs does the proper idempotency dance (pre-check, INSERT ON CONFLICT DO NOTHING, post-check). `PendingJob` doesn't even carry an `idempotency_key` field (`context.rs:417-427`).

**Expected:** Either route the outbox flush through `JobQueue::enqueue`, or stop having two enqueue paths. The plan's "in-TX dispatch_job" already eliminated this duplication.

**Impact:** Calling `ctx.dispatch_job()` in a transactional mutation silently drops idempotency guarantees. Two retries of the same mutation enqueue two jobs, and a UNIQUE-key collision (which queue.rs handles gracefully) becomes a database error that rolls back the whole mutation.

### [CRITICAL] Outbox-flushed workflows skip dispatcher.start_by_name → no scheduling/notify

**File:** `crates/forge-runtime/src/function/router.rs:822-835, 883-913`

**Issue:** `insert_workflow` only writes a row to `forge_workflow_runs` with `status = 'created'`. It never calls `WorkflowDispatch::start_by_name`, never enqueues anything that wakes the executor, and skips whatever side effects `start_by_name` performs (likely a `pg_notify('forge_workflow_wakeup', ...)` or similar). Workflow rows just sit there until the executor's poll loop spots them — if it polls at all.

**Expected:** Either keep using the dispatcher (`start_by_name`) or make sure every effect of dispatcher.start happens on flush (signature pinning, version freeze, NOTIFY, scheduler hand-off).

**Impact:** Workflows started inside transactional mutations may be dead-on-arrival. At minimum, the wakeup latency goes from "immediate" to "next poll cycle".

### [HIGH] FunctionRouter is now a god-object with seven concerns

**File:** `crates/forge-runtime/src/function/router.rs:74-92, 260-379, 404-527`

**Issue:** The Phase 3 plan said "one routing layer handles auth, rate limit, cache, timeout, telemetry, outbox, and signals." The struct lives up to that — and that is not a good thing. `FunctionRouter` owns: registry, db, http_client, two dispatchers, rate_limiter, role_resolver, query_cache, table_to_queries reverse index, token_issuer, token_ttl, default_timeout, signals_collector, signals_server_secret. `execute()` does timeout + log + observability + signals; `route()` does auth + rate-limit + cache + dispatch + transactional flow; `execute_transactional()` does TX begin + outbox flush + commit. Constructor proliferation (`new`, `with_http_client`, `with_dispatch`, `with_dispatch_and_issuer`, plus 8 `with_*` builders) is a smell.

**Expected:** Compose middleware layers (Tower-style or hand-rolled): `AuthLayer -> RateLimitLayer -> TimeoutLayer -> CacheLayer -> TelemetryLayer -> Handler`. The router becomes a thin orchestrator. Each concern owns its own state and tests independently.

**Impact:** Every refactor to one concern risks breaking others. The `execute → route → execute_transactional` triple-call introduces overhead and obscures the actual request path. Already producing real bugs (duplicate enqueue paths above).

### [HIGH] No DB-level cancellation when timeout fires; in-flight work continues

**File:** `crates/forge-runtime/src/function/router.rs:292-324`

**Issue:** `tokio::time::timeout(fn_timeout, ...)` only drops the future. The pool connection holding an in-flight `SELECT pg_sleep(60)` keeps running until PG finishes. No `pg_cancel_backend` or statement_timeout. The mutation TX path is worse: the connection is still owned by `Arc<AsyncMutex<Transaction>>`, and on drop sqlx will issue a ROLLBACK *eventually*, but the row locks may persist.

**Expected:** Set `statement_timeout` per-connection from the function timeout, or call `pg_cancel_backend` on the underlying pid. Otherwise pin a timeout to the SQL itself.

**Impact:** A buggy 30s query under a 5s function timeout returns 504 to the client but keeps a backend busy for 25 more seconds, with locks held. Easy DoS.

### [HIGH] Router.execute → route → handler triple-await loses span context to the cache hit

**File:** `crates/forge-runtime/src/function/router.rs:286-296, 426-458`

**Issue:** The `fn.execute` span is built in `execute()` but applied via `.instrument(span)` only on the inner `route()` future. Inside `route()`, on a cache hit, the `Ok(RouteResult::Query(...))` returns without ever entering the DB span (good) but the cached value path doesn't emit a `cache.hit` span attribute or counter. There's no `cache_hits` / `cache_misses` metric. The `record_fn_execution` call in `execute()` doesn't differentiate between a cache hit (sub-millisecond) and a real handler invocation.

**Expected:** Either record `cache.hit = true` on the span and a `forge_cache_hits_total` counter, or split the metric into `forge_fn_handler_seconds` (real work) vs `forge_fn_total_seconds` (including cache).

**Impact:** Operators can't see cache effectiveness. Latency p99 looks great because it's polluted by cache hits.

### [HIGH] auth_cache_scope hashes all claims including request-time fields

**File:** `crates/forge-runtime/src/function/router.rs:615-645`

**Issue:** `auth_cache_scope` includes the *entire* claims map in the hash. For tokens that include claims like `iat`, `nbf`, `exp`, or any timestamp/jti field, every refresh issues a token with different values, so every subsequent request misses the cache. Even worse: claims like `correlation_id` (if anyone added one to claims) silently fragment the cache by request.

**Expected:** Whitelist the claims that actually scope identity (sub, tenant_id, role-bearing claims). Never hash exp/iat/nbf/jti or anything that varies across token refreshes for the same logical principal.

**Impact:** Cache hit rate craters for any system using token refresh. This silently destroys the supposed performance benefit of `cache_ttl`.

### [HIGH] Invalidate-on-mutation strategy invalidates by query name only — works only because cache key is also keyed by name

**File:** `crates/forge-runtime/src/function/router.rs:592-613`, `crates/forge-runtime/src/function/cache.rs:152-159`

**Issue:** `invalidate_cache_for_mutation` collects *query names* whose `table_dependencies` overlap with the mutation's writes, then `invalidate_by_tables(&[query_name])` removes every cached entry whose `function_name == query_name`. This is "invalidate every variant of this query for every user/tenant/auth scope". Combined with the `selected_columns` field that exists on `FunctionInfo` (line 49-52, `forge-core/function/traits.rs`) but is **never used by the invalidation logic**, the system is doing a coarse-grained invalidate-all-cached-instances when v1 supposedly used selected_columns to skip when changed columns don't intersect.

**Expected:** Use `selected_columns` to skip invalidation when the mutation's changed columns don't intersect with the query's selected columns. The metadata is already extracted and stored in FunctionInfo — Codex just didn't wire it.

**Impact:** Per-cache-TTL gains evaporate after every mutation. The whole `cache_ttl` feature's value drops dramatically.

### [HIGH] Reactivity cache invalidation does not survive cluster — local cache only

**File:** `crates/forge-runtime/src/function/cache.rs:50-203`, `crates/forge-runtime/src/function/router.rs:482-484`

**Issue:** `QueryCache` is a local `RwLock<HashMap<...>>`. `invalidate_cache_for_mutation` only fires on the node where the mutation ran. Any other node in the cluster keeps serving stale cached query results until TTL expires. There's no LISTEN on `forge_changes` to invalidate the local cache from peer mutations.

**Expected:** Subscribe the local cache to the existing `forge_changes` channel and evict on every cluster-wide change event. Or document the cache as single-node-only.

**Impact:** In a multi-node deployment, mutations on node A do not invalidate cached queries on node B. Cache becomes a correctness hazard.

### [HIGH] `Arc::try_unwrap(tx_handle)` is a runtime guess; can return Internal error spuriously

**File:** `crates/forge-runtime/src/function/router.rs:814-816`

**Issue:** After `drop(ctx)`, the code does `Arc::try_unwrap(tx_handle)` to extract the transaction. If anything else has held an `Arc<AsyncMutex<Transaction>>` clone — for example, a long-running task spawned from inside the handler that captured the ctx, or any future bug where `Mutation::execute` accidentally tees the handle — `try_unwrap` returns `Err(Arc<...>)` and the framework returns `ForgeError::Internal("Transaction still in use")` while leaking the open transaction.

**Expected:** Make the transaction ownership topology unambiguous: handler borrows the TX through `&mut`, framework owns it directly, no `Arc` escape valve. The `Arc<AsyncMutex<Transaction>>` shape exists only because the buffered-outbox plumbing demanded shared mutability.

**Impact:** Subtle handler patterns produce mysterious 500s with no useful diagnostic. Connection leak on every such failure.

### [HIGH] Mutation rollback path is only `drop(ctx)` — no explicit rollback, no log

**File:** `crates/forge-runtime/src/function/router.rs:841-844`

**Issue:** On handler `Err`, the code does `drop(ctx)` and returns the error. It never explicitly calls `tx.rollback()`. sqlx's `Transaction::drop` impl does issue a rollback async, but only after a `block_on`-style spawn and only with `tracing::debug!`-level logging. There's no guarantee the rollback completes before the next request reuses the connection (it does in practice for sqlx, but the ordering relies on internals).

**Expected:** Explicitly `tx.rollback().await` and propagate any error. At minimum, log at `info!` so operators can see TX rollback latency.

**Impact:** Hidden rollback latency, debugging surprise when rollbacks fail, no observability for transaction abort rates.

### [HIGH] insert_workflow stores trace_id = workflow_id.to_string() — destroys distributed tracing

**File:** `crates/forge-runtime/src/function/router.rs:894-906`

**Issue:** When flushing the outbox, the workflow row is written with `trace_id = workflow.id.to_string()`. The mutation that started this workflow has its own `trace_id` in `RequestMetadata`, which is what the gateway's tracing middleware emitted to OTel. By overwriting it with the workflow_id, the workflow run loses correlation with the originating request.

**Expected:** Pass the mutation's `trace_id` through `PendingWorkflow` and use it here. Or use OpenTelemetry's `parent_span_id` propagation.

**Impact:** You cannot trace a request from gateway → mutation → workflow in any observability tool. The trace breaks at the workflow boundary.

### [HIGH] Mutation macro never auto-extracts SQL tables; relies entirely on explicit `tables(...)`

**File:** `crates/forge-macros/src/mutation.rs:388-391`

**Issue:** `let table_deps_tokens = match &attrs.tables { Some(tables) => quote! { &[#(#tables),*] }, None => quote! { &[] } };`. The mutation macro never runs `SqlStringExtractor` on the function body. Every mutation without explicit `tables(...)` reports zero `table_dependencies`, which means `invalidate_cache_for_mutation` does *nothing* for it. The query macro does extract tables (query.rs:248-268), so the asymmetry is silent.

**Expected:** Mutations need extraction too. INSERT/UPDATE/DELETE targets should be auto-detected and used to drive cache invalidation.

**Impact:** Cache invalidation is effectively broken for any mutation that doesn't manually declare `tables(...)`. Every example in the repo would need to be patched. Looking at examples will likely find none of them have it.

### [HIGH] MutationContext.dispatch_job in non-transactional mode bypasses lookup → can dispatch unknown jobs

**File:** `crates/forge-core/src/function/context.rs:1281-1288, 1322-1330`

**Issue:** In transactional mode, `dispatch_job` looks up `job_info_lookup(job_type)` and errors with NotFound if the job isn't registered. In non-transactional mode it just calls `dispatcher.dispatch_by_name` with no validation. Same problem on `dispatch_job_with_context`.

**Expected:** Use the same `job_info_lookup` in both branches, or push the lookup into the dispatcher.

**Impact:** Non-transactional mutations silently dispatch to non-existent jobs. The job rows pile up as `pending` until expiry.

### [MEDIUM] start_workflow validates dispatcher.get_info inside outbox push but flush also re-checks — double work

**File:** `crates/forge-core/src/function/context.rs:1356-1366`, `crates/forge-runtime/src/function/router.rs:822-833`

**Issue:** `MutationContext::start_workflow` pre-resolves `version` + `signature` via `workflow_dispatch.get_info(...)` to surface "no active version" eagerly. Then `execute_transactional` re-runs the same `workflow_dispatcher.as_ref().and_then(|d| d.get_info(&workflow.workflow_name))` check on flush. If the active version flips between these two calls, the workflow row gets pinned to the older signature but the dispatcher claims a newer one is active, with no consistency check.

**Expected:** Pin once, at flush time, and use that snapshot for both the row and the auth check. Don't TOCTOU.

**Impact:** Cosmetic in steady state, race-window bug during workflow deploys.

### [MEDIUM] AuthHandler ergonomics — single fn pointer means each macro encodes which sub-registry to use

**File:** `crates/forge/src/auto_register.rs:54-67`, `crates/forge-macros/src/query.rs:586-588`, `crates/forge-macros/src/mutation.rs:517-519`

**Issue:** Collapsing 8 Auto* types into one `AutoHandler(pub fn(&mut HandlerRegistries))` looks clean but pushes the dispatch into every macro: each query expands to `|registries| registries.functions.register_query::<X>()`, every job to `|registries| registries.jobs.register::<X>()`, etc. The HandlerRegistries struct must know about every kind. Adding a new kind means: edit AutoHandler users, edit every macro, edit HandlerRegistries fields, edit auto_register_all. Eight types had clearer separation.

**Expected:** Either keep per-kind submit types (8 small inventories, no shared struct), or use a sealed trait `Register: Fn(&mut HandlerRegistries)` so individual handlers register themselves without naming the sub-registry inline.

**Impact:** Slightly worse separation of concerns. Adding a new handler kind churns more files.

### [MEDIUM] Duplicate constructors with copy-paste defaults — easy to drift

**File:** `crates/forge-runtime/src/function/router.rs:110-161, 163-192`

**Issue:** `FunctionRouter::new` and `FunctionRouter::with_http_client` repeat 17 lines of default initialization each, plus another 14 lines of `with_dispatch` -> `with_dispatch_and_issuer` chaining. They MUST stay in sync (e.g., `default_timeout: Duration::from_secs(30)`). One was already drifted from the other in this audit's nearby code.

**Expected:** Single private `new_with(http_client: CircuitBreakerClient)` constructor; public surface is one `new(...)` plus chainable setters.

**Impact:** Maintenance trap. Adding a new field to FunctionRouter forces editing multiple constructors and easy to miss one.

### [MEDIUM] Cache key is `function_name: String` — clones per get/set

**File:** `crates/forge-runtime/src/function/cache.rs:55-60, 178-184`

**Issue:** `CacheKey { function_name: String, args_hash: u64, auth_scope_hash: u64 }`. `make_key` clones the function name into a String per cache lookup. With function_name being a `&'static str` everywhere upstream (FunctionInfo::name is `&'static str`), this is allocation per request.

**Expected:** Intern names in an `Arc<str>` table, or hash the name to u64 too — `CacheKey` becomes `(u64, u64, u64)` and is `Copy`.

**Impact:** Hot-path allocation. Not catastrophic but unnecessary.

### [MEDIUM] LRU eviction sorts entire HashMap each time, O(n log n)

**File:** `crates/forge-runtime/src/function/cache.rs:191-202`

**Issue:** `evict_oldest` collects every key-timestamp pair into a Vec, sorts the whole thing, then takes `count` items. With max_entries = 10_000, every set when at capacity allocates 10k key clones plus an n log n sort. There's no proper LRU/heap.

**Expected:** Use a `BinaryHeap` keyed by created_at, or a proper LRU cache crate.

**Impact:** Mild memory churn under steady-state cache pressure. Worse with larger caches.

### [MEDIUM] Sha256Hasher::finish swallows hasher state and returns wrong value

**File:** `crates/forge-runtime/src/function/cache.rs:30-46`

**Issue:** Standard `Hasher::finish` is non-consuming (`&self`), so the impl clones the SHA256 state, finalizes the clone, returns its first 8 bytes, and the original hasher continues. It works for the immediate use case but any caller that calls `.finish()` mid-stream then keeps writing will get a result that does NOT include the post-finish writes — actually it will, because the hasher state isn't reset. This is subtle and "works by accident" which is the worst kind of code.

**Expected:** Drop `Hasher` impl entirely; expose a custom trait with `consume(self) -> u64` semantics. Then there's no false interface to misuse.

**Impact:** Future maintainer assumes Hasher contract holds, gets surprising bugs.

### [MEDIUM] `looks_like_sql` heuristic uses `to_uppercase()` and unbounded `contains` — false positives, false negatives

**File:** `crates/forge-macros/src/sql_extractor.rs:31-38`

**Issue:** `s.to_uppercase().contains("SELECT")` matches docstrings, log messages, and any variable named `selection`. The `(upper.contains("FROM") && !upper.contains("import"))` branch matches "the FROM keyword in your import" or any prose with `FROM` in it. SQL detection should be strict (literal in a `sqlx::query!` macro context) to avoid extracting random strings.

**Expected:** Only treat as SQL when found inside the recognized macro/method context. The visit hooks for `visit_expr_macro` already gate by macro name; relying on that alone is enough.

**Impact:** Spurious table_dependencies extracted from log strings or doc strings, leading to spurious cache invalidation. Or missed SQL when the query lacks literal SELECT/INSERT keywords (e.g., `WITH foo AS (...) ...`, dynamic CTEs).

### [MEDIUM] `extract_string_content` does ad-hoc unescape — not Rust string literal semantics

**File:** `crates/forge-macros/src/sql_extractor.rs:67-100`

**Issue:** Hand-rolled raw string and regular string parsing. Replaces `\n`, `\t`, `\"`, `\\` only. Misses `\r`, `\0`, `\x...`, `\u{...}`. Off-by-one risk on raw strings with mismatched `#`. The proc-macro ecosystem already has `proc_macro2::Literal::span()` plus `litrs` for this — there's no reason to reimplement string-literal parsing.

**Expected:** Use `syn::Lit::Str::value()` (already used in `visit_expr_lit`) or the `litrs` crate. Don't parse `lit.to_string()` back into source form.

**Impact:** Missed escapes on non-trivial SQL strings. Edge cases break extraction silently.

### [MEDIUM] sql_references_identity_scope ignores PostgreSQL-specific JSON/jsonb operators and PG syntax

**File:** `crates/forge-macros/src/sql_extractor.rs:543-721`

**Issue:** The scope check parses with `PostgreSqlDialect` so jsonb operators (`->`, `->>`, `@>`, `#>`) are tokenized, but `expr_has_scope` doesn't traverse most of the variants — only BinaryOp, Subquery, etc. PG features that *do* parse but aren't visited: `Expr::JsonAccess`, `Expr::Cast`, `Expr::AnyOp`, `Expr::AtTimeZone`, `Expr::Tuple`, `Expr::Position`, `Expr::Lambda`, `Expr::Substring`. A WHERE clause like `WHERE (claims->>'user_id')::uuid = $1` would not register as scoped.

**Expected:** Walk every variant or add a `_ => false` only after visiting common nested-expression cases. Or invert the check: walk all `Expr::Identifier`/`CompoundIdentifier` nodes via a generic traversal.

**Impact:** Legitimate scoped queries that use jsonb-style scope extraction fall through to the "unscoped" error and force users to add `#[query(unscoped)]` (which kills the safety check entirely).

### [MEDIUM] sql_references_identity_scope returns scoped if ANY statement is scoped — too permissive

**File:** `crates/forge-macros/src/sql_extractor.rs:543-559`

**Issue:** The outer loop iterates all SQL strings and statements; the first scoped statement causes an early return of `Scoped`. So a query that runs two statements — `SELECT * FROM tasks WHERE user_id = $1` and `SELECT * FROM secrets` — passes the scope check, even though `secrets` is read unscoped.

**Expected:** Every statement must be scoped. `iter().all(...)` not `any(...)`.

**Impact:** A malicious or careless function can sneak in unscoped reads as long as it includes one scoped query. Defeats the whole point of the compile-time check.

### [LOW] FunctionRouter::execute logs at multiple levels using redundant macro

**File:** `crates/forge-runtime/src/function/router.rs:687-742`

**Issue:** The `log_fn!` macro generates 6 nearly identical match arms. The fallthrough `_ => log_fn!(trace)` is unreachable after a non-exhaustive match — except `LogLevel` is `#[non_exhaustive]` so this is technically defensive. Still, the macro indirection makes the code harder to grep.

**Expected:** Compute level once, use `tracing::event!(level, ...)`. Fewer lines, more idiomatic.

**Impact:** Style only.

### [LOW] `FunctionRouter::function_infos` clones every FunctionInfo

**File:** `crates/forge-runtime/src/function/router.rs:397-402`

**Issue:** Returns `Vec<FunctionInfo>` by clone. Callers that just want metadata pay the clone cost on every admin / introspection call. FunctionInfo holds `&'static str`s mostly, so clone is cheap, but still unnecessary.

**Expected:** Return `impl Iterator<Item = &FunctionInfo>` or `&[FunctionInfo]`.

**Impact:** Low. Would matter if function counts go past a few thousand.

### [LOW] `Sha256Hasher::finish_u64` silently truncates 256-bit digest to 64 bits

**File:** `crates/forge-runtime/src/function/cache.rs:19-27`

**Issue:** Only the first 8 bytes of SHA-256 are used. For cache keys this is fine — collision probability is acceptable at 2^-32. But the `// SHA-256 always yields 32 bytes; .get keeps clippy happy` comment is misleading: the truncation isn't the problem; it's the choice to use 64-bit hashes for cache keys at all. Birthday-bound at ~4B keys.

**Expected:** If the cache will never have 4B entries, this is fine but document why a `Sha256` was chosen over `xxhash`/`fxhash`. A cryptographic hash is overkill for cache keys and ~10x slower.

**Impact:** Performance overhead on every cache key, no security benefit (cache is local).

### [REGRESSION] tables = ["foo"] → tables("foo") breaking change is a breaking change without a migration aid

**File:** `crates/forge-macros/src/attrs.rs:58-81`, `crates/forge-macros/src/mutation.rs:46-47`, `crates/forge-macros/src/query.rs:58-59`

**Issue:** Per the progress tracker: "Breaking: `tables = [\"foo\", \"bar\"]` → `tables(\"foo\", \"bar\")`". darling's parse path will reject `tables = [...]` with an opaque `unknown field` error. Pre-1.0 tolerates breaking changes, fine — but the error message doesn't explain the migration path.

**Expected:** Add a custom error in `TablesList::from_value`/`from_string` that catches `tables = [...]` and emits "tables now uses parenthesized syntax: tables(\"foo\", \"bar\")".

**Impact:** Existing users see "unknown field 'tables'" or similar instead of a directed error. Annoying upgrade.

### [REGRESSION] `consistent` flag honored on mutation cache invalidation: never (mutation has no consistent field)

**File:** `crates/forge-runtime/src/function/router.rs:427-432`, `crates/forge-core/src/function/traits.rs:58-60`

**Issue:** The `consistent: bool` field is set on `FunctionInfo` but used only in the query path (router.rs:427-431) to choose primary vs replica pool. After a mutation runs, the cache invalidation runs on the local node only, but if a follower-replica query is cached, that cached value reflects pre-mutation state and stays cached until TTL. Setting `consistent: true` on the query forces it to read primary, which works around the cached-stale problem at query-execution time but does nothing about the cache itself: the cached value already exists with stale data.

**Expected:** When a query is `consistent: true`, either skip the cache entirely (defeats `consistent` for performance) or evict on every change — which is what `invalidate_cache_for_mutation` would do if it actually ran cluster-wide (HIGH finding above).

**Impact:** `consistent + cache_ttl` produces stale results on the consistent-supposedly query. Badly designed feature interaction.

### Summary: 29 findings (3 CRITICAL, 11 HIGH, 10 MEDIUM, 3 LOW, 2 REGRESSION)

<!-- AGENT_PHASE_3_END -->

---

## Phase 4: Reactivity

<!-- AGENT_PHASE_4_START -->
### Findings

### [CRITICAL] `flush_all` discards group IDs, dropping invalidations under buffer pressure
File: `crates/forge-runtime/src/realtime/invalidation.rs:118-121`
Issue: When `pending.len() >= max_buffer_size`, the code releases the lock then calls `self.flush_all().await`. `flush_all` returns `Vec<QueryGroupId>` (line 149-154), but the result is silently dropped. Those groups are erased from the pending map without ever being handed to the reactor for re-execution.
Expected: Either consume the returned IDs (push them to the reactor's re-execute path) or document that `flush_all` is fire-and-forget and remove the unused return type. As written, the buffer-overflow guard is the very thing that *causes* dropped invalidations.
Impact: Under burst load (the exact scenario the buffer was meant to defend against) subscribers see stale data forever until the next change on a relevant table or the periodic resync sweep (60s default) catches it. Silent correctness regression.

### [CRITICAL] Reactor bypasses `SharedRoleResolver`, relying on raw JWT claims for live re-execution
File: `crates/forge-runtime/src/realtime/reactor.rs:1124-1131` (`check_query_auth`)
Issue: The reactor's per-tick auth check uses `auth.has_role(role)` directly on the cached `AuthContext`. The rest of the runtime uses `SharedRoleResolver::resolve(...)` to look up roles from the auth provider, which can revoke roles dynamically. Subscriptions cache the AuthContext at subscribe-time and never re-resolve, so a user who has had a role revoked keeps receiving live data their JWT used to grant.
Expected: Inject the `SharedRoleResolver` into the reactor and call `.resolve()` on every re-execute, or at minimum on a cadence. Tie auth into the `forge_auth_revocations` channel that's reserved in `listener.rs:15` but unused.
Impact: Stale RBAC over SSE. A demoted/banned user keeps streaming privileged query results until they either disconnect or their JWT expires (potentially hours).

### [CRITICAL] `pg_notify` in trigger races BIGSERIAL allocation, allowing out-of-order seq emission
File: `crates/forge-runtime/migrations/system/v002_change_log.sql` (`forge_notify_change()` trigger)
Issue: The trigger does `INSERT ... RETURNING seq` then `pg_notify(... || '#' || seq)`. With concurrent transactions T1 (acquires seq=10) and T2 (acquires seq=11), nothing prevents T2 from emitting its NOTIFY before T1 commits. Listener receives `#11` first, advances `last_seq=11`, then sees `#10` and ignores or treats it as a regression. Worse, on a disconnect the replay query `seq > last_seq` will skip seq=10 entirely.
Expected: Either (a) emit notifications in commit order via a single-writer outbox + LISTEN advisory lock, or (b) treat NOTIFY purely as a wake-up signal and always re-read the change log from `last_seq+1` instead of trusting payload seq.
Impact: Silent data loss on the recovery path. Users see "I missed an INSERT but the next UPDATE on the same row eventually surfaces it." Hidden data divergence between subscribers and DB.

### [HIGH] `pg_notify` payload size guard missing — 8000-byte cap kills wide updates
File: `crates/forge-runtime/migrations/system/v002_change_log.sql` (trigger)
Issue: `pg_notify` has a hard 8000-byte payload limit. Triggers concatenate `v1:table:OP:row_id:col1,col2,...#seq`. A wide UPDATE on a table with many columns produces payloads that exceed the cap, raising `ERROR: payload string too long` and aborting the user's transaction. There's no truncation, no fallback to "columns elided," nothing.
Expected: Emit the column list only when below ~7900 bytes; otherwise emit just `v1:table:OP:row_id#seq` and rely on the change log row for column detail. Listener already falls back conservatively when columns are absent (`invalidates_columns` in readset.rs).
Impact: User-facing 500 errors on legitimate UPDATEs against wide tables. Easy to reproduce, hard to predict from app code.

### [HIGH] Job/workflow handlers use blocking `send_to_session` — slow client backpressures the reactor
File: `crates/forge-runtime/src/realtime/reactor.rs:820-823, 881-884`
Issue: `handle_job_change` and `handle_workflow_change` call `session_server.send_to_session(...).await` (blocking) per subscriber, sequentially. The query path correctly uses `try_send_to_session` (non-blocking, drops on full). One slow SSE consumer with a saturated channel will block the entire reactor task — every other subscriber on every other job/workflow waits behind it.
Expected: Use `try_send_to_session` like the query path. A single dropped progress update is preferable to head-of-line blocking the whole reactor. Or fan out per-subscriber on `tokio::spawn` with a per-session bounded queue.
Impact: One client with a stalled HTTP/2 stream stalls all real-time updates cluster-wide on this node.

### [HIGH] `JobSubscription`/`WorkflowSubscription` track no JWT exp; expired tokens stream forever
File: `crates/forge-runtime/src/realtime/reactor.rs:60-73`
Issue: Query subscriptions filter by `auth_context.token_is_expired()` (reactor.rs:485). Job and workflow subscriptions don't. They store an `auth_context` with no exp tracking. After the JWT exp passes, the runtime keeps fetching and pushing job/workflow updates until the SSE connection closes for an unrelated reason.
Expected: Either capture `token_exp` on the subscription struct and skip in `handle_*_change`, or move auth re-validation into `check_owner_access` itself (currently it doesn't look at exp at all).
Impact: Indefinite leak of authorization past JWT expiry on the live job/workflow channel.

### [HIGH] `auth_context` cached in `QueryGroup` never refreshed on role change
File: `crates/forge-core/src/realtime/subscription.rs` (`QueryGroup.auth_context`); `crates/forge-runtime/src/realtime/manager.rs:230-249`
Issue: When the first subscriber creates a group, their `AuthContext` (including roles, claims) is frozen into the group struct. Subsequent subscribers join via lookup-key match (which only hashes `principal_id + tenant_id`), but they don't update the cached auth. The reactor re-executes using the *first* subscriber's cached context. If that user gets a role revoked or a fresh JWT with new claims, the cached object is stale.
Expected: Re-execute per subscriber's auth context (back to per-subscription rather than per-group execution), or re-resolve the auth context against the auth provider on each re-execution.
Impact: Privilege snapshotting. The group's "auth identity" is whoever subscribed first. Even valid subscribers may get filtered/limited results matching the founding subscriber's permissions, not their own.

### [HIGH] AuthScope hashing ignores roles — different role sets share the same group
File: `crates/forge-core/src/realtime/subscription.rs` (`AuthScope::from_auth`)
Issue: The lookup key for dedup is `hash(query_name + args + auth_scope)`, where `AuthScope` only carries `principal_id + tenant_id`. Two users with the same principal/tenant but different roles (e.g., a regular user vs. an impersonating admin) will share the same group and the same auth context (see previous finding). Even if a single user has multiple JWTs with different roles, both subscriptions converge on whichever was first.
Expected: Include a stable hash of relevant claims/roles in `AuthScope`. Or use a per-subscriber execution model that doesn't rely on lookup-key collision for auth scoping.
Impact: Cross-role data leak via cache collision. Subtle and hard to spot in logs because both subscriptions look "valid."

### [HIGH] `Reactor::subscribe` always re-executes on existing group join, defeating dedup
File: `crates/forge-runtime/src/realtime/reactor.rs:265-276`
Issue: When a new subscriber joins an existing group (`is_new_group == false`), the code still calls `execute_query(...)` to "get fresh data for this subscriber (they might have joined mid-cycle)." This entirely defeats the point of group dedup: 100 simultaneous subscribers to the same query produce 100 query executions on subscribe, even though the group already has a cached result.
Expected: Return `group.last_result` (need to store the actual data, not just the hash) on join. Or accept eventual consistency and let the next debounce cycle deliver fresh data — the cost is N×50ms staleness for piggy-backed subscribers.
Impact: Subscribe storms (e.g., 1000 users opening a dashboard at once) burn N× the DB queries the system was designed to eliminate.

### [HIGH] `find_affected_groups` clones the entire HashSet for each change
File: `crates/forge-runtime/src/realtime/manager.rs:380-394`
Issue: `find_affected_groups` does `set.clone()` on the table's group HashSet to release the DashMap shard lock. For a hot table with thousands of subscribed groups, every NOTIFY event allocates a new HashSet sized to all groups. This is the per-change hot path.
Expected: Hold the read guard while iterating and collecting matching group IDs into a Vec (smaller, tighter), or use `RwLock<HashSet>` per shard so reads don't block writes. Better: store group IDs in a `Vec` per table (insertion-ordered, allocation-friendly) since lookups don't need set semantics.
Impact: O(subscriptions_for_table) allocation per change. Big-table workloads hit allocator pressure; observed in v1 as a tail-latency hot spot.

### [HIGH] `check_pending` holds a write lock for every 25ms tick even when empty
File: `crates/forge-runtime/src/realtime/invalidation.rs:125-146`; `reactor.rs:600-602, 644-663`
Issue: The reactor flush_interval ticks every 25ms unconditionally and grabs a `write` lock on `pending`. If the system is idle (overwhelmingly the common case), every tick blocks `process_change` writers from inserting until the empty-retain finishes.
Expected: Cheap pre-check via `pending.read()` followed by upgrade only when something is ready. Or move to a `tokio::sync::Notify`-driven model where `process_change` notifies after insertion.
Impact: Cross-shard write-lock contention scales with tick frequency, not with workload. Wasted CPU under steady-state low load.

### [HIGH] `coalesce_by_table=false` branch loses subsequent changes to the same group
File: `crates/forge-runtime/src/realtime/invalidation.rs:107-115`
Issue: When coalescing is disabled, the code uses `pending.entry(group_id).or_insert_with(...)`. If the group already has a pending entry, the new change is *discarded* rather than extending `last_change` or merging the table set. The branch comment claims "each change triggers its own invalidation entry," but in practice only the first change per debounce window is recorded.
Expected: Extend `last_change` and insert into `changed_tables`, mirroring the coalescing branch. The actual behavior under `coalesce_by_table=false` should be "no coalescing across tables" not "drop all but the first."
Impact: Footgun config option. Users who disable coalescing get worse correctness than the default, with no log indication.

### [HIGH] Listener swallows `replay_missed` errors via `.ok()?` — silent gap recovery failure
File: `crates/forge-runtime/src/realtime/listener.rs:112-119`
Issue: The replay query is wrapped with `.ok()?`, returning `None` on any DB error. The caller (line 209) ignores the return entirely. A transient network blip during reconnect produces zero log output, zero metrics, and zero retry — gap recovery just silently fails.
Expected: `match` on the result. Log warn-level on error. Set `needs_resync = true` so the next tick triggers full resync. Increment a `realtime.replay_failures` counter.
Impact: Subscribers stuck on stale data after transient DB hiccups, with no observability.

### [HIGH] `parse_notification` silently corrupts seq via `unwrap_or(0)`
File: `crates/forge-runtime/src/realtime/listener.rs:235-241`
Issue: If the `#seq` suffix exists but doesn't parse as i64, the code returns `seq=0` and treats the notification as v001-style (no seq). The change is still emitted, but `last_seq` is never advanced for this row. On reconnect, replay starts from the *previous* known seq, which works — but if a string of malformed payloads arrives, last_seq stays pinned to the last good seq forever. The actual gap goes unnoticed.
Expected: On parse failure, set `needs_resync = true` and log warn. Or skip the notification entirely so we don't fan out a change that we can't safely sequence.
Impact: Silent corruption tolerance. A malformed notification (driver bug, manual `pg_notify` from operations, partial truncation) hides gap-recovery from the rest of the system.

### [HIGH] Listener does not reconnect after `listener.recv()` error
File: `crates/forge-runtime/src/realtime/listener.rs:207-211`
Issue: On `Err(e)` from the underlying PgListener, the code calls `replay_missed`, sleeps 1 second, then loops back to `listener.recv()`. But `listener` is the same broken `PgListener` instance. Sqlx's `PgListener` does internally reconnect for some failures, but a hard transport break (server restart, network partition) returns repeatedly with errors and the loop never escalates to a full reconnect.
Expected: Tear down and rebuild the `PgListener` after N consecutive errors, or always rebuild it inside the error arm.
Impact: After a PG restart, the change channel can stay dead silently; the reactor's listener_handle restart logic only fires when `run()` returns, which it never does.

### [HIGH] `register_session` accepts `token_exp: None` for authenticated users without warning
File: `crates/forge-runtime/src/realtime/reactor.rs:165-174`; `crates/forge-runtime/src/gateway/sse.rs` (caller)
Issue: The reactor doc says "Pass `None` for unauthenticated sessions." Nothing enforces that authenticated sessions actually pass `Some(exp)`. If the JWT decoder forgets to capture exp (or the auth path is changed), authenticated sessions will silently never be evicted on token expiry. The cleanup loop relies on `Some` to do anything.
Expected: Make the API distinguish authenticated and unauthenticated paths. E.g., `register_authenticated_session(sid, sender, exp: i64)` vs `register_anonymous_session(sid, sender)`. Or panic-free debug_assert that authenticated AuthContexts always carry exp.
Impact: One-line refactor of the auth pipeline can re-introduce v1's "tokens never expire on SSE" bug with no compile-time safety net.

### [HIGH] SSE bridge emits `SESSION_EXPIRED` inline, bypassing `cleanup_expired_tokens`
File: `crates/forge-runtime/src/gateway/sse.rs:610-619`; `crates/forge-runtime/src/realtime/message.rs` (`cleanup_expired_tokens`)
Issue: There are two separate "evict on token expiry" code paths. One inline at message-send time (`try_send_to_session`), one periodic (`cleanup_expired_tokens`). Their behaviors diverge: one emits `RealtimeMessage::AuthFailed { reason: "Token expired" }`, the other emits a `SESSION_EXPIRED` event directly via the SSE bridge channel. Clients see different error shapes depending on whether expiry was caught at send time or sweep time.
Expected: Single source of truth. Either always go through `cleanup_expired_tokens` or always inline. Clients should not have to handle two incompatible "your token expired" messages.
Impact: Frontend has to handle both error shapes or it misses one. Subtle UX regression.

### [HIGH] `read_pool` snapshot at construction binds reactor to whatever was healthy at startup
File: `crates/forge-runtime/src/realtime/reactor.rs:108-117` (Reactor::new); `crates/forge-runtime/src/pg/pool.rs` (Database::read_pool)
Issue: The reactor receives a `read_pool: sqlx::PgPool` at construction. `Database::read_pool()` returns `&PgPool` based on health-check state at that moment, falling back to primary if no replicas are healthy. If primary was healthy but all replicas were unhealthy at boot, the reactor is now permanently bound to primary — even after replicas come back. The health monitor (15s interval) updates `Database`'s view but not the reactor's clone.
Expected: Hold an `Arc<Database>` and call `read_pool()` on each query, so health-check failover takes effect cluster-wide. Same fix needed for any other component that took a pool snapshot.
Impact: Reactor permanently routed to primary in deployments that boot during a replica outage. Defeats the read-replica strategy that was the whole point of `read_pool`.

### [HIGH] `update_group` adds runtime-discovered tables to the index *after* re-execution — race window
File: `crates/forge-runtime/src/realtime/manager.rs:437-450`
Issue: `update_group` is called after a re-execution returns. It extends the `table_index` with newly observed tables (those not in compile-time `table_deps`). Any DB change to these new tables that arrives *between* group creation and `update_group` will not match the table index and will not invalidate the group. With a 50ms+ debounce window plus query execution time, this race window is real.
Expected: Either pre-populate the index with all conservative table candidates at subscribe time, or run the first execution as a "warm-up" inside the subscribe path before announcing the subscription as live.
Impact: Initial-period invalidation misses for queries with runtime table discovery (the query body wasn't fully parsed by the macro). Manifests as a one-time stale-on-first-update bug per subscription, hard to repro.

### [HIGH] `SubscriberStore` behind a single `Mutex` — global lock among sharded structures
File: `crates/forge-runtime/src/realtime/manager.rs:124, 134-165`
Issue: `groups`, `group_lookup`, `table_index`, `session_subscribers` are all `DashMap` with 64 shards. `subscribers` is a single `Mutex<SubscriberStore>`. Every subscribe, unsubscribe, get_group_subscribers, and remove_session_subscriptions takes this lock. Under high concurrent subscribe/unsubscribe load (the very thing sharding was meant to fix), this becomes a serial bottleneck.
Expected: Replace with `DashMap<usize, Subscriber>` and an `AtomicUsize` for next_key.
Impact: Sharding optics without the sharding wins. Lock-contention hot spot that fights the rest of the design.

### [HIGH] `subscribers.lock().unwrap_or_else(...)` recovers from poisoned lock — but masks the bug
File: `crates/forge-runtime/src/realtime/manager.rs:264-267, 296-299, 346-349, 420-423`
Issue: Several call sites do `.unwrap_or_else(|e| { error!(...); e.into_inner() })` to recover from a poisoned mutex. This is a workaround pattern that turns a bug into a tracing error and silently hides whatever panic poisoned the lock. The lints in `Cargo.toml` even disallow `unwrap_used` precisely because of this risk; but the codebase has exceptions for "we know this can't actually fail" — a poisoned lock means it *did*.
Expected: Either guarantee no panics under the lock (clean up the panic-prone code paths) or upgrade to `tokio::sync::Mutex` if the lock is held across awaits, where poisoning isn't a thing. Recovering from a poisoned std::Mutex without understanding what was being mutated when the panic happened is data corruption waiting to happen.
Impact: Hidden state corruption. The first panic under a held lock leaves the SubscriberStore in an unknown state, then four call sites cheerfully proceed with "recovery."

### [HIGH] `emit_change` is `pub` outside test — public API leak
File: `crates/forge-runtime/src/realtime/listener.rs:266-268`
Issue: `pub fn emit_change(&self, change: Change)` is publicly exposed on `ChangeListener`. Comment says "for testing or manual triggering," but nothing gates it behind `#[cfg(test)]`. External crates can inject arbitrary changes into the broadcast channel and trigger the entire fan-out pipeline.
Expected: `#[cfg(any(test, feature = "test-utils"))] pub fn emit_change(...)` or rename to `pub(crate)`.
Impact: A malicious dependency or a misuse in user code can synthesize fake DB changes and DoS or confuse subscribers.

### [HIGH] Documentation says retention is 10 minutes; code default is 1 hour
File: `docs/docs/scale/reactivity.mdx`; `crates/forge-runtime/src/realtime/reactor.rs:420-434`
Issue: The user-facing docs state "the change log is retained for 10 minutes by default." `trim_change_log` runs `forge_trim_change_log('1 hour'::INTERVAL)` and `forge_trim_change_log` itself defaults to `INTERVAL '1 hour'`. Either the docs are wrong or the defaults regressed.
Expected: Pick one. Update the SQL default and reactor call to match the documented value, or update the docs to match the code. Also expose retention as config.
Impact: Operators tuning storage based on docs over-provision (or under-provision) by 6x. Either is bad.

### [HIGH] Docs reference `max_cached_result_bytes`; no enforcement found in realtime code
File: `docs/docs/scale/reactivity.mdx`; missing in `realtime_config.rs`/`manager.rs`/`reactor.rs`
Issue: Docs mention a `max_cached_result_bytes` knob to bound per-group result cache. The realtime code never references this field. There's no per-result size enforcement on `last_result_hash` or any cached payload. A massive query result is fine memory-wise (we only store the hash), but the docs imply something else exists.
Expected: Either implement size-bounded result caching (a real cache, with eviction) or remove the doc reference. Right now the doc is fiction.
Impact: Operators believe a defensive bound exists; it doesn't.

### [MEDIUM] No backpressure or queue-depth metric on re-execution semaphore
File: `crates/forge-runtime/src/realtime/reactor.rs:505-528`
Issue: `Semaphore::new(64)` bounds parallelism. When the queue exceeds 64, futures block on `acquire_owned().await` indefinitely. There's no metric for "groups awaiting permit," no histogram of acquire latency. Operators can't tell if their `max_concurrent_reexecutions` is too low.
Expected: Wrap `acquire_owned` with a timer and emit `realtime.reexec.acquire_latency_seconds`. Also expose `pending_in_flight` as a gauge.
Impact: Can't diagnose slow reactor without manual instrumentation.

### [MEDIUM] Re-execute permits acquired sequentially before spawning futures
File: `crates/forge-runtime/src/realtime/reactor.rs:508-528`
Issue: The loop acquires a permit, *then* pushes a future. With 64 permits and 200 ready groups, the first 64 acquire instantly; the next acquire blocks the loop, preventing any of the first 64 from being polled. The futures don't make progress until the loop yields. `FuturesUnordered::next().await` is only awaited after the entire loop exits.
Expected: Spawn each work item as a `tokio::spawn` inside the loop and join via `JoinSet`, or interleave `select!` between `acquire` and `next`.
Impact: Acquire-ahead-of-spawn pattern produces serialized startup. First-batch latency is hugely worse than steady-state under bursts.

### [MEDIUM] Listener adds no row count guard in `replay_missed` — unbounded fetch on gap
File: `crates/forge-runtime/src/realtime/listener.rs:112-119`
Issue: If a node was disconnected for a long time and the change log is large, `replay_missed` fetches *all* rows since `last_seq` in one go. For a busy app, this can be hundreds of thousands of rows held in memory.
Expected: Stream via `fetch()` (cursor) or batch with `LIMIT` and loop until caught up. Or fall back to `needs_resync` if the count exceeds a threshold (replay is amortized as full resync anyway).
Impact: Memory blowup on long disconnects. Reactor OOMs on reconnect rather than gracefully resyncing.

### [MEDIUM] `extract_table_name` naming-convention fallback strips arbitrary prefixes
File: `crates/forge-runtime/src/realtime/reactor.rs:1096-1108`
Issue: Falls back to `query_name` minus prefix `get_/list_/find_/fetch_` for "the table name." `get_recent_orders` becomes `recent_orders`, which probably isn't a real table. The read set then claims the query depends on a non-existent table; the table_index never matches; invalidation never fires.
Expected: Don't synthesize fake table dependencies. If the macro couldn't extract tables, route the query to "always invalidate on any change" or surface a compile-time warning that table_dependencies is empty.
Impact: Queries with unconventional naming silently never invalidate. Filed as a "subscription doesn't update" bug, traced over hours of debugging.

### [MEDIUM] Hash comparison via `serde_json::to_string` is non-canonical
File: `crates/forge-runtime/src/realtime/reactor.rs:383-386`
Issue: `compute_hash` does `serde_json::to_string(data).unwrap_or_default()`. Object key order in serde_json is preservation order from the source. If the underlying SQL or handler reorders keys between executions (unlikely but possible with complex JOIN projections or runtime composition), the hash flips and an unchanged result triggers a fan-out.
Expected: Canonicalize JSON before hashing (sort object keys, like `hash_json_canonical` already does in the subscription module). Re-use that helper.
Impact: Spurious updates that don't actually change observed data. Wastes bandwidth and re-renders. Also defeats the dedup intent of hash compare.

### [MEDIUM] `compute_hash` swallows `serde_json::to_string` failure as empty hash
File: `crates/forge-runtime/src/realtime/reactor.rs:383-386`
Issue: `unwrap_or_default()` returns `""` on serialization failure. `compute_hash` then hashes the empty string. Two failed serializations hash identically — looks like "no change" — and the update is silently dropped.
Expected: Propagate the error or use a sentinel hash that won't collide with any real result hash.
Impact: Serialization edge cases (rare but real with floats, unsupported types) cause silent dropped updates.

### [MEDIUM] `RealtimeMessage::Channel` and `GapDetected` reserved but unwired
File: `crates/forge-runtime/src/realtime/message.rs`
Issue: The message enum has `Channel { ... }` and `GapDetected { ... }` variants that are constructed nowhere. The README/docs hint at pub-sub channels and gap notifications to clients, but the runtime never emits them. Dead code that the compiler accepts because the enum is `#[non_exhaustive]`.
Expected: Implement, or remove. Half-implemented variants invite drift between intent and behavior.
Impact: Confusing surface area; future devs assume the variant is "live" and build against it.

### [MEDIUM] `forge_channels` and `forge_auth_revocations` reserved channels with no wiring
File: `crates/forge-runtime/src/realtime/listener.rs:13-15` (comments)
Issue: The comment block reserves `forge_channels` (ephemeral pub-sub) and `forge_auth_revocations` (cluster-wide auth/role teardown) channel names. Neither is implemented. With no implementation, role revocation is best-effort per-node JWT-expiry-based — see the SharedRoleResolver finding above.
Expected: Implement `forge_auth_revocations` LISTENer that drops cached AuthContexts on receipt. Or remove the reservation.
Impact: Documented capability that doesn't exist. Operators relying on the comment make false assumptions.

### [MEDIUM] No metric for invalidation pending depth or latency
File: `crates/forge-runtime/src/realtime/invalidation.rs:170-184` (InvalidationStats)
Issue: The stats expose `pending_groups` and `pending_tables`. Neither is wired to a `prometheus`/OTEL gauge. The cluster_metrics module has notification-related metrics (record_notification_processed) but nothing for invalidation backlog, debounce wait time, or flush duration.
Expected: Plumb through `record_invalidation_pending_depth`, `record_debounce_wait_seconds`, `record_flush_duration_seconds`. Tie into the existing observability stack.
Impact: Operators can't observe the "is the reactor keeping up?" question without ad-hoc logging.

### [MEDIUM] Resync sweep at 60s holds whole subscription manager lock implicitly via `all_group_ids`
File: `crates/forge-runtime/src/realtime/manager.rs:486-488`; `reactor.rs:447-462`
Issue: `all_group_ids()` iterates the entire `DashMap`. With 100k groups it allocates a 100k-entry Vec while holding shard locks (in iteration order). A subscribe/unsubscribe trying to grab the same shard waits behind iteration. Resync runs every 60s.
Expected: Iterate by shard, batch group IDs, yield between batches. Or use a lock-free snapshot via `Arc::clone` of an internal `Vec<QueryGroupId>` updated on subscribe.
Impact: 60s resync sweep stalls subscribe/unsubscribe across all shards momentarily. With many subs, observable latency spike.

### [MEDIUM] Resync sweep does no auth re-check — same staleness as live re-execute
File: `crates/forge-runtime/src/realtime/reactor.rs:440-463` (resync_all_groups)
Issue: Calls `reexecute_groups` which uses cached `auth_context` — same problem as the SharedRoleResolver bypass. The 60s sweep is supposed to *correct* missed invalidations but it inherits the cached-auth bug.
Expected: As above, route through SharedRoleResolver and skip groups whose principals have been revoked.
Impact: 60s sweep keeps pushing data to revoked principals.

### [MEDIUM] `reexecute_groups` permits drop *before* fan-out — no parallel limit on send
File: `crates/forge-runtime/src/realtime/reactor.rs:516-528`
Issue: Permit is dropped after `execute_query_static` returns but before `subscription_manager.update_group` and the per-subscriber send loop. Send fan-out happens inside the `while let Some(...) = futures.next().await` loop and is sequential. With 64 concurrent re-executions, all 64 results queue up to be sent serially after computation.
Expected: Either keep the permit until send completes (limits fan-out concurrency too) or move the send into its own bounded pool. Concurrent send is fine if non-blocking.
Impact: Serialized fan-out becomes the bottleneck once execution is parallelized; net throughput plateau.

### [MEDIUM] `cleanup_old_sessions` uses `chrono::TimeDelta::MAX` as fallback — silent retention bug
File: `crates/forge-runtime/src/realtime/manager.rs:89-96`
Issue: If `chrono::Duration::from_std(max_age)` overflows, the fallback is `TimeDelta::MAX`, making cutoff `Utc::now() - MAX` which is effectively "year 0." The `retain` predicate then keeps all sessions forever. Silent retention failure.
Expected: Saturate to a sensible upper bound (e.g., 1 year) and log a warn, or take a `Duration` already known to be in chrono's range.
Impact: Misconfigured `max_age` leads to disconnected sessions never being reaped, growing memory unbounded.

### [MEDIUM] Removing job/workflow subscriptions does extra retain pass over all entries
File: `crates/forge-runtime/src/realtime/reactor.rs:185-202` (remove_session)
Issue: Iterates every key in `job_subscriptions` and `workflow_subscriptions` to filter out the disconnecting session. With many jobs/workflows running, this is O(jobs * subscribers). Could index by session_id for O(1) cleanup.
Expected: Maintain a parallel `session -> set of (job_id|workflow_id)` map, mirror of session_subscribers.
Impact: Slow disconnect storms (e.g., load balancer drains 1000 SSE connections). Each disconnect O(N) job_subscriptions sweep.

### [MEDIUM] `handle_workflow_step_change` does an extra round-trip per step change
File: `crates/forge-runtime/src/realtime/reactor.rs:899-923`
Issue: For every step change, fetches `workflow_run_id` separately, then fetches the entire workflow including all its steps again in `fetch_workflow_data_static`. Two queries when one with a JOIN would do, and the steps query (lines 997-1008) is unbounded — a workflow with thousands of steps fetches them all on every step status change.
Expected: Either include `workflow_run_id` in the change payload directly, or batch step updates with a debounce window so a workflow with N step changes doesn't fan out N full re-fetches.
Impact: Workflow with many fast steps creates a lot of redundant DB reads. Hot-loop hazard.

### [MEDIUM] `from_seq` reset to 0 after replay — second disconnect loses prior position
File: `crates/forge-runtime/src/realtime/listener.rs:103-107`
Issue: `replay_missed` returns `None` when `since == 0`, meaning "first boot, nothing to replay." But `last_seq` starts at 0. If the listener has never seen a notification and disconnects, replay is skipped. On reconnect, the listener will emit only changes after the new connection's first message — anything that happened during the disconnect window of a brand-new node is lost.
Expected: On startup, set `last_seq` to `MAX(seq) FROM forge_change_log` so the first replay catches anything new.
Impact: New node startup misses changes for the brief window between connect and first NOTIFY.

### [LOW] `cleanup_old_sessions` retains based on `last_active_at` only for `Disconnected` status
File: `crates/forge-runtime/src/realtime/manager.rs:93-95`
Issue: Predicate is `status != Disconnected || last_active > cutoff`. Connecting/Reconnecting sessions are kept forever even if `last_active_at` is ancient. A stuck Connecting session (e.g., handshake hung) is never reaped.
Expected: Apply the cutoff regardless of status, except for `Connected`.
Impact: Slow leak of half-open sessions.

### [LOW] Tests use `unwrap()` despite project-wide `clippy::unwrap_used` deny
File: `crates/forge-runtime/src/realtime/listener.rs:272`, `manager.rs:502`
Issue: `#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]` on test modules. The CLAUDE.md says "no .unwrap() calls (use .expect() with reason or proper error handling)." Tests get a blanket allow.
Expected: Tests should still use `.expect("descriptive reason")`. Helps debug test failures.
Impact: Style only. Test failures from `.unwrap()` give panic-without-context messages that take longer to triage.

### [LOW] `handle_change` returns silently on missing `row_id` for forge_jobs/forge_workflow_runs
File: `crates/forge-runtime/src/realtime/reactor.rs:740-770`
Issue: If a job/workflow change arrives without `row_id`, the function silently returns without invalidating any subscriptions. Could be a malformed notification or a future schema change that doesn't include row_id.
Expected: Log debug or fall back to fanning out a "something changed, refetch" event to all job/workflow subscribers.
Impact: Hidden subscriptions that miss updates. Hard to detect.

### Summary: 38 findings (3 CRITICAL, 22 HIGH, 11 MEDIUM, 2 LOW)

<!-- AGENT_PHASE_4_END -->

---

## Phase 5: Jobs / Cron / Daemons / Workflows

<!-- AGENT_PHASE_5_START -->
### Findings

### [CRITICAL] WorkflowStatus has 12 variants instead of plan-mandated 6

**File:** `crates/forge-core/src/workflow/traits.rs:127-152`

**Issue:** Plan explicitly requires a "six-state status enum: Pending, Running, Sleeping, Waiting, Completed, Failed". The enum has 12 variants: `Created, Running, Waiting, Completed, Compensating, Compensated, Failed, BlockedMissingVersion, BlockedSignatureMismatch, BlockedMissingHandler, RetiredUnresumable, CancelledByOperator`. There is NO `Sleeping` state — durable sleep is conflated into `Waiting`. There is no `Pending` — `Created` is used instead. The plan says "lazy fail (no readiness gate)" for signature mismatch yet `BlockedSignatureMismatch` and the readiness module still exist (`workflow/readiness.rs`).

**Expected:** Six variants only — `Pending, Running, Sleeping, Waiting, Completed, Failed`. Compensation is an inline closure invoked during `Failed` transition, not a status. Operator cancellation collapses into `Failed` with a reason. Readiness gate must be removed.

**Impact:** Users see meaningless "blocked" states whose only resolution is operator action; surface area doubles (FromStr, as_str, is_terminal, is_blocked, db migrations) and locks future schema. `forge_workflow_runs` index `idx_forge_workflow_runs_name_version` lists 5 of these "terminal" statuses — every new state forces a migration.

### [CRITICAL] Workflow readiness gate still exists despite plan saying "lazy fail, no readiness gate"

**File:** `crates/forge-runtime/src/workflow/readiness.rs:1-79` (entire file), `crates/forge-runtime/src/workflow/mod.rs`

**Issue:** Plan: "Workflow advancement on shared worker pool via `$workflow_resume`. ... Strict signature compare; lazy fail on mismatch (no readiness gate)." The `readiness.rs` module still exists, presumably tied to `/_api/ready` and "blocked runs". Combined with `BlockedSignatureMismatch`/`BlockedMissingVersion`/`BlockedMissingHandler` enum variants and the `idx_forge_workflow_runs_name_version` partial index that filters them out, the readiness probe still fails when blocked runs exist.

**Expected:** Delete `readiness.rs`. Lazy fail = mark run `Failed` with reason on signature mismatch. `/_api/ready` stays green. No "blocked" sub-states.

**Impact:** A signature mismatch between a deployed binary and an in-flight workflow run flips `/_api/ready` red, and orchestrators pull the node from the load balancer — exactly the disaster the plan tried to avoid.

### [CRITICAL] Workflow signature uses FNV-1a 64-bit hash + type-name string, NOT blake3 + schemars

**File:** `crates/forge-macros/src/workflow.rs:308-346, 386-440`, `Cargo.toml:50`

**Issue:** Plan: "Workflow signatures use schemars hash; pin schemars exactly. blake3 hash of (name, version, sorted step names, schemars-of-input/output)." Reality:
1. `derive_signature` uses **FNV-1a 64-bit** in 16 hex chars (line 318, 345). 64 bits is collision-prone vs. blake3's 256.
2. Input/output captured as `quote!(#ty).to_string()` — i.e. the source-code identifier like `"MyInput"`. The actual struct shape (fields, types) is NOT hashed. Renaming `MyInput.amount: i32` → `MyInput.amount: f64` does NOT change the signature.
3. schemars is pinned to `=0.8.22`, plan demanded `=1.x.y`.

**Expected:** blake3-256 hex; hash schemars-derived JSON Schema of input/output not their type names; schemars 1.x.

**Impact:** Field-shape changes silently slip past the signature gate. Two incompatible workflow definitions can share a signature. Resume corrupts in-flight runs by deserializing into a different shape than they were started with. The signature isn't a safety gate — it's a placebo.

### [CRITICAL] Workflow execution still runs in `tokio::spawn` background task instead of via $workflow_resume jobs

**File:** `crates/forge-runtime/src/workflow/executor.rs:107-127`

**Issue:** Plan: "Workflow advancement on shared worker pool via `$workflow_resume`. No separate scheduler." But `WorkflowExecutor::start` immediately `tokio::spawn`s `execute_workflow` (line 107). Initial workflow execution runs in an ad-hoc background task on the node that received the start request — NOT through the job queue. The "shared worker pool" only handles resumes, not initial runs. Capacity reservations and queue-pause don't apply to initial runs.

**Expected:** `start` writes `forge_workflow_runs` row plus enqueues `$workflow_resume` (or a `$workflow_start`) job in the same TX. All execution flows through worker pool.

**Impact:** Two execution paths (spawned for start, queue for resume). Per-queue capacity reservations don't apply to start. Crash of the spawning node loses the workflow until /_api/ready notices missing runs. Goal of "shared worker pool" undermined.

### [CRITICAL] Atomicity gap: workflow suspend writes status and resume-job INSERT in two separate transactions

**File:** `crates/forge-runtime/src/workflow/scheduler.rs:271-323`, `executor.rs:798-862`

**Issue:** Plan: "Workflow suspend records status and the next wakeup in one transaction. No window where the run is 'Sleeping' with no resume scheduled." `try_claim_waiting` (line 271) executes one UPDATE that flips status `waiting → running`. `enqueue_resume` (line 294) executes a totally separate JobQueue::enqueue → second TX. Crash between these two leaves the run in `running` with no resume job in `forge_jobs`. The run is orphaned.

**Expected:** Single shared `&mut Transaction` plumbed through scheduler and queue. BEGIN; UPDATE forge_workflow_runs; INSERT INTO forge_jobs; COMMIT. NOTIFY post-commit.

**Impact:** Crash recovery scenarios leave workflow runs stuck in `running` forever with no scheduler entry to resume them. Operator must manually requeue or reset. Catastrophic data loss in a "durable" workflow engine.

### [CRITICAL] Cron `try_claim` and job enqueue are two separate transactions; orphaned `forge_cron_runs.running` rows on failure

**File:** `crates/forge-runtime/src/cron/scheduler.rs:307-359, 362-410`

**Issue:** `try_claim` inserts into `forge_cron_runs` with `status = 'running'`. Then `execute_cron` separately enqueues `$cron:{name}` into `forge_jobs`. If the job enqueue fails (line 397-410: just logs an error), the cron run row stays in `running` forever. No cleanup, no transition to `failed`. The next tick re-claims via stale-reclaim only after `run_stale_threshold` (default 15 min).

**Expected:** Single transaction inserts both rows. Or write only the job, derive cron_run from job state. Plan's "collapse cron into job mode" would eliminate this.

**Impact:** Each enqueue failure permanently leaks a `forge_cron_runs` row. Worse: while leaked, the tick loop can't re-execute that scheduled time (UNIQUE constraint), so the cron run is silently skipped for up to 15 min.

### [CRITICAL] Cron job dispatched with `max_attempts: 1`; stale-reclaim immediately exhausts retries

**File:** `crates/forge-runtime/src/cron/scheduler.rs:380-385`

**Issue:** `JobRecord::new(job_type, input, JobPriority::Normal, 1)` sets `max_attempts = 1`. The `claim()` query in `JobQueue` increments `attempts` on every claim. When `release_stale` resets a running job back to pending, `attempts` is NOT decremented. Next claim → attempts becomes 2, exceeding max_attempts → goes straight to dead_letter. Combined with the cron's own retry path being separate from the job's retry path, every transient stall kills the cron forever.

**Expected:** `max_attempts` configurable per-cron via `#[cron(max_attempts=N)]`, default ≥ 3. And/or `release_stale` decrements attempts when resetting.

**Impact:** Any flaky cron run dies on first stall. Operators see permanent dead_letter rows, no automatic recovery.

### [CRITICAL] OutboxBuffer pattern still in JobContext; plan rejected this entirely

**File:** `crates/forge-core/src/job/context.rs:9, 44, 80, 210-261`, `crates/forge-runtime/src/jobs/executor.rs:166-170, 290-353`

**Issue:** Plan REJECTS buffer-then-flush. JobContext still has `pending_dispatches: Arc<Mutex<OutboxBuffer>>`. `dispatch_job` and `start_workflow` push `PendingJob`/`PendingWorkflow` into the buffer. `JobExecutor` calls `take_pending_dispatches` AFTER the handler completes successfully, then INSERTs them in a separate post-commit pass. This is exactly the rejected outbox-flush pattern, and worse: the workflow flush at line 325-338 uses a runtime `sqlx::query()` (not the compile-time `query!`) and writes status='created' without enqueueing a $workflow_resume job — inert workflow rows.

**Expected:** Direct INSERT in handler's transaction. `dispatch_job(job_type, args, &mut tx)` style API.

**Impact:** Sub-jobs/workflows dispatched from a successful job run only get their INSERTs after the parent's status flip — if the executor crashes during `flush_pending_dispatches`, sub-jobs are lost while parent shows `completed`. Workflow rows inserted with `status='created'` and never woken — stuck forever. (See also the equivalent CRITICAL findings in Phase 3 above.)

### [HIGH] Three-method scheduler resume API instead of one

**File:** `crates/forge-runtime/src/workflow/scheduler.rs:243-266`

**Issue:** `resume_workflow`, `resume_with_timeout`, `resume_with_event` are near-identical bodies (each calls `try_claim_waiting` then `enqueue_resume`). They differ only in the `trigger` string and a `from_sleep` bool. Sloppy multi-method API.

**Expected:** Single method: `async fn resume(reason: WorkflowResumeReason)` taking an enum like `{ Timer, EventTimeout, Event }`. Single SQL, single test.

**Impact:** Triple the surface area, triple the bugs, triple the maintenance.

### [HIGH] Plan demands per-queue worker capacity (BTreeMap), code uses one shared semaphore

**File:** `crates/forge-runtime/src/jobs/worker.rs:14-43, 92, 173-178`

**Issue:** Plan: "Worker pool reserves capacity per queue. BTreeMap<String, QueueWorkerConfig>. Defaults: 8 default, 4 workflows, 2 cron." `WorkerConfig` has `max_concurrent: usize` (single number) and `capabilities: Vec<String>`. The semaphore is a single `Semaphore::new(self.config.max_concurrent)` over all jobs regardless of queue. There is no `queue` column in `forge_jobs` (only `worker_capability`). Mass `default` traffic can fully saturate the semaphore and starve `$workflow_resume` and `$cron:*` — unbounded latency on workflow advancement.

**Expected:** Per-queue semaphore map: `HashMap<String, Arc<Semaphore>>` indexed by `queue` column. Reserved counts per queue. `forge_jobs` needs a `queue VARCHAR` column populated from the macro.

**Impact:** Workflow advancement starvation under load. Any user job storm freezes cron, retries, and workflow steps. The plan goal of "make jobs the durable execution substrate" is undermined.

### [HIGH] `forge_jobs` schema lacks plan-mandated columns: queue, kind, retry policy, ownership, singleton, pause

**File:** `crates/forge-runtime/migrations/system/v001_initial.sql:41-71`

**Issue:** Plan: "Add `kind`, `queue`, retry policy, ownership, progress, singleton, pause fields. NO workflow-specific columns." Inspection of `forge_jobs`: columns exist for `priority`, `attempts`, `max_attempts`, `worker_capability`, `idempotency_key`, `owner_subject`, `progress_percent`, `progress_message`, `expires_at`, but NOT `queue`, NOT `kind`, NOT `singleton` flag, NOT a `forge_queue_state` table holding pause flags. `worker_capability` is reused as a queue-like signal but isn't.

**Expected:** Columns `queue VARCHAR NOT NULL DEFAULT 'default'`, `kind VARCHAR NOT NULL DEFAULT 'normal'`, plus a `forge_queue_state` table holding pause flags per queue.

**Impact:** Operationally lethal — cannot pause a single queue. Cannot route by queue. Cannot enforce per-queue capacity. Plan goal half-built.

### [HIGH] NOTIFY trigger fires on no-op UPDATE pending→pending

**File:** `crates/forge-runtime/migrations/system/v003_job_wakeup.sql:5-16`

**Issue:** Trigger fires `AFTER INSERT OR UPDATE OF status` and unconditionally checks `IF NEW.status = 'pending'`. Any UPDATE that touches `status` and ends up at `pending` — including a future no-op or a duplicate retry write — fires NOTIFY again. With `forge_enable_reactivity` also adding triggers on `forge_jobs` for `forge_changes`, every job state change fires multiple NOTIFYs. No `OLD.status IS DISTINCT FROM NEW.status` guard.

**Expected:** `IF (TG_OP = 'INSERT' OR OLD.status IS DISTINCT FROM NEW.status) AND NEW.status = 'pending'` to skip no-op transitions.

**Impact:** Notification storms during retry-heavy workloads. Listeners thrash. CPU waste on every worker.

### [HIGH] expires_at not set in release_stale or request_cancel non-running paths; old jobs accumulate forever

**File:** `crates/forge-runtime/src/jobs/queue.rs:483-564, 610-661`

**Issue:** `complete()`, `fail()` (dead_letter branch), `cancel()` set `expires_at = NOW() + retention`. But:
- `release_stale` (line 616-631) finalizes orphaned cancellations to `cancelled` WITHOUT setting `expires_at`.
- `request_cancel` (line 547-562) non-running branch sets `cancelled` but no `expires_at`.

These rows live forever. Index `idx_forge_jobs_expires` partial on `expires_at IS NOT NULL` skips them.

**Expected:** Every terminal status transition must set `expires_at`. Single `terminate(status, reason, ttl)` helper used everywhere.

**Impact:** Slow retention leak — every crash-during-cancel or cancel-while-pending adds a permanent row. Jobs table grows unboundedly under cluster instability.

### [HIGH] cleanup_expired and release_stale run per-worker instead of cluster-wide

**File:** `crates/forge-runtime/src/jobs/worker.rs:97-123`

**Issue:** Each `Worker::run()` spawns a cleanup task that periodically calls `release_stale()` and `cleanup_expired()`. With N workers across M nodes, you get N×M concurrent DELETEs racing on the same rows. Plan says "Default retention cron for forge_jobs. Daily cron." — meaning a leader-elected, scheduled cleanup. The current implementation is a thundering herd.

**Expected:** Single leader-elected `forge_jobs_retention` cron at daily cadence. Or a queue-pause-aware job inserted by leader.

**Impact:** N times the DELETE pressure. Lock contention on `forge_jobs`. Wasted PG CPU. Especially bad with >2 nodes.

### [HIGH] Daemon-as-job collapse rejected without justification in code

**File:** `crates/forge-runtime/src/daemon/runner.rs` (586 lines), `crates/forge-runtime/src/daemon/mod.rs`

**Issue:** Plan: "Collapse daemons into job mode. #[daemon] desugars to #[job(mode = daemon, keep_alive = true)]." Tracker says rejected but the `daemon/mod.rs` doesn't justify the deviation. Runner remains a 586-line separate file with its own loop, restart logic, heartbeat, leader election. Exactly the duplication the plan aimed to eliminate.

**Expected:** Either implement collapse — long-running self-re-enqueueing job; or document the deviation in `daemon/mod.rs` with the trade-offs so the next agent doesn't think it's an oversight.

**Impact:** Two execution substrates duplicate logic (shutdown, heartbeat, restart, leader election). Bug fixes have to land in both. Plan goal undermined.

### [HIGH] `claim` query lacks queue filter, queue pause, daemon leader role match

**File:** `crates/forge-runtime/src/jobs/queue.rs:204-242`

**Issue:** Plan: "Implement correct job claiming: Concrete query_as! SKIP LOCKED. Respect queue pause, schedule time, capability, stale reclaim fences, daemon leader role matching `daemon:<handler>`." Current claim WHERE clause:
- status = 'pending'
- scheduled_at <= NOW()
- worker_capability = ANY($2) OR worker_capability IS NULL

Missing: queue pause check, daemon-leader role match, queue filter, owner_subject role-based filter. The plan envisages roles like `daemon:my_handler` that only the leader can claim — but with no `queue` column and no leader-aware role filter, every node can claim every job.

**Expected:** Add `(worker_capability NOT LIKE 'daemon:%') OR EXISTS (...leader)` and queue-pause exclusion `NOT EXISTS (SELECT 1 FROM forge_queue_state WHERE queue = jobs.queue AND paused)`.

**Impact:** Daemons can run on every node simultaneously when they should be leader-pinned; no graceful queue drain; wrong roles claim wrong jobs.

### [HIGH] Stale reclaim doesn't decrement attempts; original worker can double-execute via slow-but-alive race

**File:** `crates/forge-runtime/src/jobs/queue.rs:633-658`, `executor.rs:64-79`

**Issue:** `release_stale` resets job to pending: clears `worker_id`, `claimed_at`, `started_at`, `last_heartbeat` but does NOT decrement `attempts`. The next `claim()` increments `attempts` again. Stale-reclaim consumes a retry. Worse: the (worker_id, attempts) fence in `start()` does protect the original claimant from running two `start()` calls, but ONLY at start time — if the original claimant is past `start()` (already running) and the row has been reclaimed and started by a new worker, the original may STILL execute its handler body to completion. The fence stops re-marking, but doesn't stop in-flight handler execution. Heartbeat is also fenceless.

**Expected:** Reset attempts to its prior pre-claim value (i.e., `attempts = attempts - 1` when resetting). Add cancellation watcher in handler executor that polls `worker_id` and aborts if changed. Best: ditch stale-reclaim entirely and use a leasing model.

**Impact:** Real retry budget halved. Hidden double-execution of side-effects. Surprising user behavior for jobs that hit transient stalls.

### [HIGH] `flush_pending_dispatches` uses raw `sqlx::query` for workflow inserts and skips dispatcher.start_by_name

**File:** `crates/forge-runtime/src/jobs/executor.rs:321-352`

**Issue:** When a job successfully completes, its buffered workflows are flushed via raw `sqlx::query("INSERT INTO forge_workflow_runs (...) VALUES (..., 'created', ...)")`. This bypasses the workflow dispatcher's signature pinning, version freeze, and any scheduling/notify side-effects. Workflow rows land in status='created' and never advance unless the workflow scheduler later picks them up by polling. There's no `$workflow_resume` enqueue, no NOTIFY, no signature lock against the active version.

**Expected:** Either route through `WorkflowDispatch::start_by_name` (which handles all of the above), or replicate ALL of its effects atomically. The current half-write produces orphan rows.

**Impact:** Job-spawned workflows are dead-on-arrival, lacking any scheduler hand-off. Same root cause as the Phase 3 finding above.

### [HIGH] update_workflow_status performs SELECT-then-UPDATE TOCTOU pattern despite the doc claim

**File:** `crates/forge-runtime/src/workflow/executor.rs:796-862`

**Issue:** The function fetches `status` (line 816), validates it's in `valid_from`, then does `UPDATE ... WHERE id=$2 AND status=$3` (line 843). Doc claims "atomic check-and-set: WHERE includes the expected current status" but you've already round-tripped to PG once for nothing. A simpler `UPDATE ... WHERE id=$1 AND status = ANY($2)` with the valid-from set baked in saves a round trip and removes any TOCTOU risk in dev. Worse: status `String` is stored as a clone; the check-and-set passes the OLD status string back as the WHERE predicate. If anything updated `status` to a still-valid but different string between SELECT and UPDATE, the UPDATE fails and you get `InvalidState` even though the transition would still be valid.

**Expected:** Single `UPDATE forge_workflow_runs SET status = $1 WHERE id = $2 AND status = ANY($3)` where `$3` is the valid-from array.

**Impact:** Spurious `InvalidState` errors under concurrent transitions, extra round-trip, more code.

### [HIGH] Compensation handlers can't survive process restart; admitted in code with `unwrap_or_else` warning

**File:** `crates/forge-runtime/src/workflow/executor.rs:430-457`

**Issue:** Plan: "Inline on_rollback closures." Closures captured into `Arc<dyn Fn>` cannot be persisted. After a restart, `compensation_state.handlers` is gone. Code at lines 437-450 explicitly says: "We have the step order from before the crash, but handlers are closures that can't survive a restart. Fail closed". So compensation is broken across restarts.

**Expected:** Either (a) re-execute the handler from scratch on resume so closures are reconstructed, or (b) use named compensation handlers (registered like jobs) instead of inline closures. Plan wanted (a).

**Impact:** A workflow that crashed mid-execution loses compensation entirely. Manual remediation required for every such run. This is exactly what "saga pattern" is supposed to prevent.

### [HIGH] Workflow scope still includes `parallel.rs` and `step_runner.rs` despite plan saying "NO parallel step builder"

**File:** `crates/forge-core/src/workflow/parallel.rs` (8.3KB), `crates/forge-core/src/workflow/step_runner.rs` (9.6KB)

**Issue:** Plan: "Make workflows restart-safe and minimal: ... NO parallel step builder. Workflow scope sequential". Both files exist with ~17KB of code. The mod.rs likely re-exports them. Sequential simplification benefit lost.

**Expected:** Delete `parallel.rs` and `step_runner.rs`. Sequential-only API: 4 capabilities (sequential steps, durable sleep, wait_for_event with timeout, retry-on-failure).

**Impact:** Behavior contract drift between docs and code. Future maintenance burden. Violates plan's "minimal" goal.

### [MEDIUM] `cron` macro doesn't desugar to `job(mode=cron)`

**File:** `crates/forge-macros/src/cron.rs:127-166`

**Issue:** Plan: "Collapse cron into job mode. #[cron] desugars to #[job(mode = cron)]." The macro emits a separate `ForgeCron` trait impl, separate `CronInfo`, separate executor. Not a desugar.

**Expected:** Macro emits `#[job]`-equivalent code that registers as a job kind=cron, with cron-specific scheduling fields stored in metadata.

**Impact:** Two trait families (ForgeCron and ForgeJob), two contexts, two registries. Plan goal "fold cron into jobs" not realized.

### [MEDIUM] `$cron:` and `$workflow_resume` job-name prefix collisions not validated

**File:** `crates/forge-runtime/src/jobs/registry.rs:75-109, 148-164`, `crates/forge-runtime/src/cron/bridge.rs:18`

**Issue:** Internal handlers register names like `$cron:my_cron` and `$workflow_resume`. `register_system` and `register` both insert into the same `HashMap<String, ...>`. If a user defines `#[job] fn workflow_resume(...)` with `name = "$workflow_resume"` (or by accident with manual naming), the registration silently wins/loses. No `validate_job_name(&name)` rejects `$`-prefixed names from user macros.

**Expected:** Compile-time validation in `forge-macros/src/job.rs`: reject names starting with `$`. Runtime `register()` should also reject (defense in depth).

**Impact:** Silent shadowing of system handlers. Confusing diagnostics. Trivial DoS if user's job collides with `$workflow_resume`.

### [MEDIUM] Cron bridge leaks memory: `Box::leak(job_name.clone().into_boxed_str())` for every cron

**File:** `crates/forge-runtime/src/cron/bridge.rs:67`

**Issue:** `let info = JobInfo { name: Box::leak(job_name.clone().into_boxed_str()), ... }`. The `name` field of `JobInfo` is `&'static str`, so the bridge fakes "static" by leaking memory. Each cron registration leaks one heap String permanently. Across config reloads or hot-restart-tests, this leaks unboundedly.

**Expected:** Change `JobInfo.name` to `Cow<'static, str>` or `Arc<str>` so dynamically-built names don't require leaking. Or make bridge handlers store names in their own struct and only leak constants.

**Impact:** Every cron registration permanently consumes a few bytes of heap, never freed. Live tests and reload paths leak. Code smell.

### [MEDIUM] JobContext leaks pool() and circuit_breaker_client() into the public API

**File:** `crates/forge-core/src/job/context.rs:139-148`

**Issue:** `pub fn pool(&self) -> &sqlx::PgPool` and `pub fn circuit_breaker_client(&self) -> &CircuitBreakerClient` exist on the public `JobContext`. The doc comment says "for bridge handlers that need to construct other context types". This means every user job handler also sees these accessors, encouraging users to bypass `ctx.db()`/`ctx.http()` abstractions and do raw SQL or raw HTTP.

**Expected:** Separate `BridgeContext` extending `JobContext` for bridge-only use. Or have bridges directly take `(JobQueue, Pool, ClientArc)` and not go through `JobContext` at all.

**Impact:** Encourages users to call `ctx.pool()` directly, bypassing every safety net (auth, observability, transaction boundaries). The hard-won abstraction is leaked.

### [MEDIUM] `register_system` is pub(crate) but bridges call it from non-builder code

**File:** `crates/forge-runtime/src/jobs/registry.rs:148-164`, `crates/forge-runtime/src/cron/bridge.rs`, `crates/forge-runtime/src/workflow/bridge.rs`

**Issue:** `mod.rs` exposes `pub(crate) mod registry` so bridges can call `register_system`. Future maintainer can't tell where system handlers are wired without grep. A trait or a private builder would be tighter.

**Expected:** Move bridge registration into the registry constructor or a `JobsBuilder::with_cron_bridge().with_workflow_bridge().build()`. Bridges shouldn't reach into the registry at runtime.

**Impact:** Hidden coupling. Difficult to reason about registration ordering.

### [MEDIUM] Workflow signature's schemars version is 0.8.22 not 1.x

**File:** `Cargo.toml:50`, `crates/forge-core/Cargo.toml:15`

**Issue:** Plan: "schemars = '=1.x.y'". Workspace pins `schemars = "=0.8.22"`. Schemars 1.x has a different output format. Future upgrade silently changes signatures for every workflow → all in-flight runs become BlockedSignatureMismatch. Or if intent is 0.8 lock-in, no migration path to 1.x without coordinated cluster rebuild.

**Expected:** Pin to `=1.0.x`. Document the schemars-derived JSON Schema as part of the signature input.

**Impact:** Signature stability across crate upgrades is fragile. Signature feature is stuck on a 0.8.x branch that will eventually be unmaintained.

### [MEDIUM] Step idempotency relies on JSONB `step_results` AND `forge_workflow_steps` table — two stores

**File:** `crates/forge-runtime/migrations/system/v001_initial.sql:124-195`

**Issue:** Schema has both `forge_workflow_runs.step_results JSONB` and `forge_workflow_steps` table. Two sources of truth for "which step completed". Plan says "completed steps skip on resume" — drift potential between the two stores leads to either skipped steps (incorrect) or re-executed steps (duplicate side-effects).

**Expected:** One source of truth — `forge_workflow_steps` only, with `(workflow_run_id, step_name)` index.

**Impact:** Drift between two stores → either skipped steps (correctness bug) or re-executed steps (duplicate side-effects).

### [MEDIUM] `$cron:{name}` uses `:` which complicates handler-name plumbing

**File:** `crates/forge-runtime/src/cron/bridge.rs:18`, `crates/forge-runtime/src/cron/scheduler.rs:370`

**Issue:** The bridge job name has a `:`. Stored verbatim in `forge_jobs.job_type VARCHAR(255)`. But: tracing span fields, OTEL attrs, admin UIs need to handle `:` consistently. Anywhere does `name.replace(':', '_')` and the lookups break silently.

**Expected:** Use a different separator (`__cron__name`) or non-textual mapping.

**Impact:** Probable bug in observability/admin paths. Reserved-character risk.

### [MEDIUM] Workflow `start_by_name` does NOT enforce signature compatibility for callers passing JSON values

**File:** `crates/forge-runtime/src/workflow/executor.rs:1003-1031, 70-128`

**Issue:** `start_by_name` takes a `serde_json::Value` and forwards to `start`. There's no input-shape validation against the workflow's `WorkflowInfo` input schema before the row is persisted. If the input doesn't match what the workflow handler expects, the failure surfaces only when the spawned task tries to deserialize — by then the row is in `created` state and will linger.

**Expected:** Validate input against schemars schema before INSERT. Reject with `Validation` if shape mismatches.

**Impact:** Garbage-in-database. Workflow tests pass at compile time but malformed runtime payloads still create rows.

### [MEDIUM] cleanup_consumed_events runs hourly per scheduler-instance; thundering herd

**File:** `crates/forge-runtime/src/workflow/scheduler.rs:121-133`

**Issue:** Every `WorkflowScheduler::run` instance ticks `cleanup_interval = 3600s` and calls `event_store.cleanup_consumed_events`. With N nodes, N concurrent DELETE on `forge_workflow_events`. Same root cause as the job cleanup thundering herd.

**Expected:** Leader-elected cleanup, daily cron.

**Impact:** N× DELETE pressure on `forge_workflow_events` once per hour. Lock contention.

### [LOW] WorkerConfig has no per-queue capacity even in docstring

**File:** `crates/forge-runtime/src/jobs/worker.rs:13-43`

**Issue:** `WorkerConfig` has only one capacity slot. Plan envisaged BTreeMap. Docstring doesn't mention queues. Future operator confusion.

### [LOW] WorkerConfig::default has 10 max_concurrent; plan defaults are 8 default + 4 workflows + 2 cron = 14

**File:** `crates/forge-runtime/src/jobs/worker.rs:31-43`

**Issue:** Default `max_concurrent: 10` doesn't match plan's per-queue total of 14 (8+4+2).

### [LOW] cron `catch_up_limit` defaults to 10 but isn't documented as a hard cap on missed-runs

**File:** `crates/forge-macros/src/cron.rs:111`

**Issue:** Plan: "catch-up limits". Default 10 missed-runs caught-up after a long downtime is hardcoded with no operator override path beyond per-cron. No global `[cron] max_catchup_runs` config exists.

### [LOW] WorkflowScheduler.run calls `process_ready_workflows` from BOTH the interval AND the NOTIFY arm without dedup

**File:** `crates/forge-runtime/src/workflow/scheduler.rs:97-138`

**Issue:** A burst of NOTIFY events plus a coincident interval tick both call `process_ready_workflows` concurrently. The function does a SELECT then per-row processing without any leader gate or row-level lock; concurrent calls fight over the same rows. `try_claim_waiting` saves us at the row level but you do duplicate SELECTs and per-row UPDATE attempts.

**Expected:** A single-flight or `Mutex<()>` around `process_ready_workflows` so only one runs at a time.

**Impact:** Wasted DB cycles under burst. Minor.

### [REGRESSION] WorkflowStatus FromStr fails (rather than fallback) on unknown legacy values

**File:** `crates/forge-core/src/workflow/traits.rs:207-227`, `crates/forge-runtime/src/workflow/executor.rs:737-742`

**Issue:** `from_str` returns `Err(ParseWorkflowStatusError(s.to_string()))` for unknown strings. Then `executor.rs:737-742` converts that to `ForgeError::Internal`. If a legacy v1 row has a status not in the 12-variant set, the executor returns an error and the run cannot be loaded. Hot upgrades from v1 break.

**Expected:** Either accept unknown statuses as `Failed` with the original value preserved in metadata, or implement a forward/backward compatible status field.

**Impact:** Upgrading from a snapshot with extra statuses crashes loaders. v1→v2 migration breaks.

### [REGRESSION] CronStatus enum has 4 variants, no terminal `cancelled` or `dead_letter`

**File:** `crates/forge-runtime/src/cron/scheduler.rs:13-36`

**Issue:** `CronStatus` is `Pending, Running, Completed, Failed`. No way to mark a cron run as cancelled by operator, no dead_letter equivalent. v1 had richer state. Combined with the cron-as-job dispatch now being separate (forge_jobs has more states than forge_cron_runs), they fall out of sync — a job that goes to dead_letter leaves its cron_run row stuck in `running` until stale-reclaim catches it.

**Expected:** Sync states between cron_runs and the job substrate. Or collapse cron_runs entirely (per plan).

**Impact:** Operator views of `forge_cron_runs` miss states that `forge_jobs` exposes. Diagnostic mismatch.

### Summary: 33 findings (8 CRITICAL, 12 HIGH, 9 MEDIUM, 2 LOW, 2 REGRESSION)

<!-- AGENT_PHASE_5_END -->

---

## Phase 6: Gateway / Auth / MCP / Signals

<!-- AGENT_PHASE_6_START -->
### Findings

#### CRITICAL: Signal partition cron is never wired up — signals will silently break each new month

**File:** `crates/forge-runtime/src/signals/partition.rs:10-46` and `crates/forge/src/runtime.rs:838-870`

`ensure_partitions()` (creates current/next month partitions) and `drop_old_partitions()` (retention) are defined and unit-tested, but never invoked outside tests. The runtime's signals init at `runtime.rs:838-870` wires `SignalsCollector` and the session reaper but never calls `ensure_partitions`. Grep confirms only test-module call sites.

**Expected:** Call `ensure_partitions(&pool, retention_days).await?` during signals init, and run a daily cron (or daemon tick) that calls both `ensure_partitions` and `drop_old_partitions`.

**Impact:** First insert into `forge_signals_events` after the configured next-month partition rolls past will fail with `no partition of relation "forge_signals_events" found for row`. All signal inserts are dropped from then on. Retention is also unenforced, so the partitioned table grows unbounded.

#### CRITICAL: MCP `tools/list` returns every registered RPC, not just public/MCP-exposed handlers

**File:** `crates/forge-runtime/src/gateway/mcp.rs:405-571`

`handle_tools_list` does not consult auth and does not filter by `is_public`. It iterates `function_router.function_infos()` and emits every `Query`, `Mutation`, and `Webhook` (the `_ => "function"` arm at lines 505-509 catches webhooks because `FunctionKind` only has `Query`/`Mutation`/`Webhook`). Private/internal queries and mutations (e.g. anything not `#[query(public)]`/`#[mutation(public)]`) and even webhook endpoints get advertised verbatim, including parameter schemas and table dependencies, to any caller that can reach `/_api/mcp`.

**Expected:** Filter to handlers explicitly opted into MCP (an `mcp_exposed` flag on `FunctionInfo`, or restrict to `#[mcp_tool]` registrations only) AND/OR enforce that `tools/list` requires authentication and only returns handlers the caller is permitted to invoke.

**Impact:** Information disclosure. An attacker hitting MCP discovery learns the entire private RPC surface — names, args, table dependencies — without authenticating. Even worse, webhooks are listed as callable tools, which violates the contract that webhooks are HMAC-gated.

#### CRITICAL: `X-Forwarded-For` is trusted unconditionally in OAuth and signals — IP-based limits and visitor IDs are spoofable

**File:** `crates/forge-runtime/src/gateway/oauth.rs:1024-1044` and `crates/forge-runtime/src/signals/visitor.rs` (visitor-id derivation)

`oauth.rs:1031-1044` reads `X-Forwarded-For` and `X-Real-IP` and returns the first hop as the client IP, with no `trusted_proxies` configuration gate. The result feeds the OAuth login rate limiter at lines 243 and 545. The same unconditional-trust pattern appears in the signals visitor-id derivation (`SHA256(ip+ua+daily_salt)`).

**Expected:** Only honor forwarded headers when the immediate peer matches a configured `trusted_proxies` list (CIDRs). Walk the header right-to-left, dropping hops until a non-trusted address is found. Default off when no trusted proxies are configured — fall back to `connect_info` peer.

**Impact:** A client setting `X-Forwarded-For: 10.0.0.<random>` can rotate IPs per request, bypassing OAuth login throttle and minting unique visitor IDs (defeating GDPR-compliant deduplication). On an unprotected deployment the attacker controls both signals analytics and brute-force budget.

### HIGH

#### HIGH: OAuth CSRF token compared with `==`, not constant-time

**File:** `crates/forge-runtime/src/gateway/oauth.rs:529`

`if cookie_csrf == form.csrf_token { ... }` is a plain string compare. Likewise `sse.rs:74` and `sse.rs:987` compare `session.session_secret != session_secret` directly.

**Expected:** Use `subtle::ConstantTimeEq` or `hmac::Hmac::verify_slice` for any token equality check that bears on auth/session validity.

**Impact:** Timing oracle on the CSRF token and SSE session secret. Lower priority because both values are 16+ random bytes, but the codebase already has constant-time helpers and uses them elsewhere — this is inconsistent and exploitable in principle.

#### HIGH: `JwtConfig::dev_mode()` falls back to defaults instead of failing closed in production

**File:** `crates/forge-runtime/src/gateway/auth.rs:102-125`

When `FORGE_ENV=production` is set but no JWT keys are configured, `dev_mode()` logs a `tracing::error!` and returns `Self::default()`. The server boots with a randomly-generated dev secret and silently issues "valid" tokens.

**Expected:** Panic or return `Err(ForgeError::Config(_))` so the binary refuses to start in production without explicit JWT configuration.

**Impact:** A misconfigured production deployment looks healthy (logs an error nobody reads, then serves traffic) but anybody with a `forge` binary can mint tokens that the gateway accepts. Fail-closed is mandatory here.

#### HIGH: OAuth login rate limit is per-process, not cluster-wide

**File:** `crates/forge-runtime/src/gateway/oauth.rs:243,545` (limiter usage); limiter type is in-memory.

`forge-runtime`'s OAuth login limiter is constructed per node and scoped by `(client_ip, username)`. Behind a load balancer, each node sees only its share of attempts.

**Expected:** Either centralize the limiter (PG-backed counter using existing rate-limit infra) or document the per-node multiplier in `auth.rs`/`oauth.rs` and lower the per-node budget accordingly.

**Impact:** Brute-force budget scales linearly with node count. A 10-node cluster gives an attacker 10× the per-IP attempts. Not an immediate breach but undermines the lockout claim from the plan.

#### HIGH: `validate_signature` ignores `_headers` for non-Stripe variants and silently rejects unknown variants

**File:** `crates/forge-runtime/src/webhook/handler.rs:362-478`

The function takes `_headers: &HeaderMap` but only inspects them on the Stripe arm. The fall-through `_ => false` (around line 390) means any new `WebhookSignatureScheme` variant added later silently fails verification with no warning, and HMAC-SHA256 / Ed25519 verifications cannot read alternate header names (e.g. `X-Hub-Signature-256` for GitHub) because the headers parameter is dropped.

**Expected:** Match exhaustively on the variant; let each variant pull its own header(s) from `_headers` (or accept signature lookup as a closure). Compile-time error on unhandled variants.

**Impact:** Webhooks that should support GitHub-style header conventions break silently. Adding a new scheme without updating this match drops every webhook of that scheme on the floor.

#### HIGH: WebhookContext-to-MutationContext migration deferred, contradicting Phase 6 plan

**File:** `crates/forge-runtime/src/webhook/handler.rs:258`

The handler still constructs `WebhookContext`, which the plan explicitly called out as redundant with `MutationContext`. The deferral is acknowledged in `.agents/rewrite-progress.md` but the plan target is "fold webhooks into the function registry and reuse `MutationContext` end-to-end."

**Expected:** Webhooks accept `MutationContext` (so they can dispatch jobs / start workflows transactionally via the outbox). Drop `WebhookContext` from `forge-core`.

**Impact:** Webhook handlers cannot use `dispatch_job` with the same transactional outbox semantics as mutations, so any webhook that must atomically persist + queue is inconsistent with the rest of the framework. Pre-1.0 policy says no "old way / new way" coexistence.

#### HIGH: MCP OAuth uses `Uuid::new_v4()` for client_id but argon2id for client_secret with weak parameters

**File:** `crates/forge-runtime/src/gateway/oauth.rs:657` (`DUMMY_HASH`) and surrounding argon2 config.

The argon2id parameters are `m=19456 KiB, t=2, p=1`, which is the OWASP minimum. For password-equivalents protecting OAuth client_secrets in a server context, this is on the floor.

**Expected:** Either bump to `m=65536, t=3, p=1` (or higher) per current OWASP password-storage guidance, or document the chosen parameters as a deliberate latency tradeoff in `oauth.rs`.

**Impact:** A leaked OAuth client_secret hash table is brute-forceable on commodity GPUs faster than necessary. Cheap to harden.

#### HIGH: Webhooks have no replay protection (no nonce store, no timestamp window enforcement outside Stripe)

**File:** `crates/forge-runtime/src/webhook/handler.rs:362-478`

Stripe's signature scheme includes a timestamp; the implementation does check tolerance there. HMAC-SHA256 and Ed25519 schemes do not. There is no per-webhook nonce table to detect replays across schemes.

**Expected:** Either require a `Date`/timestamp header per scheme with a configurable freshness window, or store recent signature digests in a small per-webhook `forge_webhook_replay_seen` table with TTL.

**Impact:** Captured webhook bodies can be replayed indefinitely by anyone who sees them in transit (e.g. via a proxy log) for non-Stripe schemes.

#### HIGH: Subscription per-user limit defaults drift from plan (`max_sessions_per_user=8` vs plan `10`; `max_cached_result_bytes=1MiB` vs plan `10MiB`)

**File:** `crates/forge-core/src/config/realtime_config.rs:107-117`

```
fn default_max_sessions_per_user() -> usize { 8 }
fn default_max_cached_result_bytes() -> usize { 1_048_576 }
```

The plan's target was `max_sessions_per_user=10` and `max_cached_result_bytes=10 * 1024 * 1024`. Both numbers ship lower than promised; nothing documents the change.

**Expected:** Either bump defaults to plan values, or update `.agents/rewrite-progress.md` and the docs to reflect the deliberate-tightening rationale (and adjust example apps that may rely on >8 sessions per user).

**Impact:** Real apps with multiple browser tabs / devices per user will hit the cap unexpectedly. Larger queries will get evicted from cache too aggressively, causing extra re-execute work after invalidation.

#### HIGH: `max_subscriptions_per_user=500` enforced globally but `subscription_max_per_session=100` is the plan's per-session cap — relationship is undocumented

**File:** `crates/forge-core/src/config/realtime_config.rs`

Both limits exist independently. With `sse_max_sessions=10_000` and `max_sessions_per_user=8`, a user can hit 500 subs across at most 5 sessions of 100 each — but nothing enforces that the per-user count is the floor of `sessions_per_user * subs_per_session`.

**Expected:** Document the relationship in the config doc (`docs/docs/ship/configuration.mdx` and `references/api.md`) and add a debug-mode warning if `max_subscriptions_per_user > max_sessions_per_user * subscription_max_per_session`.

**Impact:** Operators tuning one knob will surprise themselves — either the user-level cap is dead code (under-tuned per-session cap) or the per-session cap is unreachable (over-tuned per-user cap).

### MEDIUM

#### MEDIUM: `handle_proxied_function_call` accepts an unused `_state: &Arc<McpState>` parameter

**File:** `crates/forge-runtime/src/gateway/mcp.rs:792`

`_state` is unused. Either it should be used (e.g. to consult a tool allow-list / per-tool rate limit) or dropped from the signature.

**Expected:** Remove the parameter, or wire it through to the gating logic suggested under the CRITICAL `tools/list` finding.

**Impact:** Code smell + misleading signature for callers.

#### MEDIUM: `oauth.rs:1024` trusts `X-Forwarded-Proto` unconditionally for redirect URI scheme decisions

**File:** `crates/forge-runtime/src/gateway/oauth.rs:1024`

Same trusted-proxy hygiene applies as for `X-Forwarded-For`. If only TLS-terminating proxies should be able to set this, the gate must say so.

**Expected:** Only honor `X-Forwarded-Proto` when peer is in `trusted_proxies`.

**Impact:** Attacker on plain HTTP can flip the resolved scheme to `https` and break protocol invariants used elsewhere (audience checks, secure-cookie bindings).

#### MEDIUM: Hand-rolled URL-encoding in `oauth.rs:1066`

**File:** `crates/forge-runtime/src/gateway/oauth.rs:1066`

The crate already pulls in `url`/`urlencoding`-like deps elsewhere in the workspace. The bespoke encoder is one more place to get reserved-char handling wrong.

**Expected:** Use `urlencoding::encode` (or `url::Url::query_pairs_mut`) for all redirect/state URL building.

**Impact:** Subtle escape bugs (e.g. `+` vs `%20` in form posts vs query strings) and one more attack surface to audit.

#### MEDIUM: Bot detector is an O(N) lowercased scan per request

**File:** `crates/forge-runtime/src/signals/bot.rs:64-77`

Each `is_bot(ua)` call lowercases the UA and linearly scans 50 patterns. That's per-request allocation + iteration on a hot signals path.

**Expected:** Build an `aho-corasick`/`AhoCorasick` automaton once at startup, store in the collector. Fall back to a small `OnceCell` cached lowercased buffer if simpler.

**Impact:** Roughly a few microseconds per signal event under load. Not catastrophic but noticeable in flame graphs and easy to fix.

#### MEDIUM: `tracing::error!` instead of `tracing::warn!` on dev-mode JWT init obscures real production errors

**File:** `crates/forge-runtime/src/gateway/auth.rs:102-125`

The `error!` is emitted on every dev-mode startup. Combined with the fail-open behavior (see HIGH), it normalizes errors in logs.

**Expected:** Either fail closed (preferred — see HIGH) or downgrade to `warn!` and prefix the message with a clear marker (`"DEV-ONLY JWT KEY: insecure, do not use in production"`).

**Impact:** Alerting on `level=ERROR` from the gateway becomes noisy; real prod errors get lost.

### LOW

#### LOW: `tools/list` does not paginate — large registries will produce huge responses

**File:** `crates/forge-runtime/src/gateway/mcp.rs:405-571`

The MCP `tools/list` JSON-RPC method returns every tool in one payload. There's no `cursor`/`page_size` handling.

**Expected:** Implement the spec's optional `nextCursor` field. Even a fixed page size of 200 is fine.

**Impact:** Single-shot responses on large apps will be tens of KB to MB. Most clients tolerate it but spec-correct cursoring future-proofs.

#### LOW: SSE session-secret generated via `Uuid::new_v4()` — fine cryptographically, but undocumented

**File:** `crates/forge-runtime/src/gateway/sse.rs:516`

`Uuid::new_v4()` uses `getrandom()` and provides 122 bits of entropy, which is sufficient. The intent (high-entropy secret, not a UUID identifier) is undocumented.

**Expected:** Add a one-line comment explaining "treated as a 122-bit secret; constant-time compared in `sse.rs:74`/`987`".

**Impact:** Future maintainers may swap `new_v4()` for a v5/v7 (deterministic) without realizing the secrecy property is load-bearing.

### REGRESSION

#### REGRESSION: 0.6 stored signed-cookie session, 0.7 ships server-side session_secret + `Uuid::new_v4()` UUID without documenting the migration

**File:** `crates/forge-runtime/src/gateway/sse.rs` (whole module) vs prior 0.6 SSE auth path.

Pre-0.7 had stateless signed cookies for SSE auth. 0.7 introduces stateful session_secret tracking. Pre-1.0 policy allows breaking changes, but the changelog/skill references don't call this out.

**Expected:** `docs/docs/connect/realtime.mdx` and `references/api.md` should describe the new SSE handshake and the implications for horizontal scaling (session_secret needs to be visible to whichever node services the SSE connection — confirm whether this is per-node memory or backed by PG).

**Impact:** Operators upgrading from 0.6 will be surprised by the new session table / state and may not know which migration sequence to follow.

### Summary: 19 findings (3 CRITICAL, 8 HIGH, 5 MEDIUM, 2 LOW, 1 REGRESSION)

Top three to address before tagging 0.7:

1. **Signal partitions never created at runtime** (`signals/partition.rs` + `forge/runtime.rs`). Helpers exist and are tested but no caller wires them into startup or a daily cron, so signal inserts will start failing the day the next-month partition runs out — silently dropping all analytics events.
2. **MCP `tools/list` advertises every RPC** (`gateway/mcp.rs:405-571`). No auth, no `is_public` filter, no `mcp_exposed` gate. Webhooks fall through the `_` arm and get listed too. An unauthenticated discovery call leaks the entire private API surface.
3. **`X-Forwarded-For` trusted unconditionally** in OAuth login throttle and signals visitor-ID derivation. No `trusted_proxies` gate. Attackers rotate spoofed IPs to bypass brute-force limits and break GDPR-compliant deduplication.
<!-- AGENT_PHASE_6_END -->

---

## Phase 7: Config / KV / Cache / Rate Limits

<!-- AGENT_PHASE_7_START -->

### CRITICAL

**C1. Broken `set_if_absent` and `increment` CTEs — silent loss of atomicity guarantees**
- Files: `crates/forge-runtime/src/kv/store.rs:106-129`, `crates/forge-runtime/src/kv/store.rs:145-171`
- Both methods use `WITH cleared AS (DELETE FROM forge_kv WHERE ...) INSERT INTO forge_kv ... ON CONFLICT DO ...`. In PostgreSQL, the data-modifying CTE and the main statement run on the **same snapshot**: the `INSERT` does not see the `DELETE`'s effects. So when an expired row exists, the CTE deletes it, but the main `INSERT` still sees the pre-delete row and trips `ON CONFLICT DO NOTHING` (`set_if_absent`) or adds to the stale value (`increment`).
- Impact for `set_if_absent`: returns `false` for an *expired* lock — caller thinks the lock is held by someone else when it's actually expired. This breaks the only documented use case (mutex/leader/idempotency keys).
- Impact for `increment`: an expired counter is treated as live, so its old value is never zeroed. Counter resets on TTL boundaries silently fail, breaking rate-limit / quota use cases the doc explicitly advertises.
- Tests do not exercise the expired-row path so the bug ships green.
- Fix: remove the CTE, use `DELETE … WHERE expires_at <= NOW()` then `INSERT … ON CONFLICT` as separate statements inside an explicit transaction (or push expiry-aware logic into the `ON CONFLICT` `WHERE` predicate).

**C2. `KvStore::delete_prefix` ships despite plan explicitly forbidding it**
- File: `crates/forge-runtime/src/kv/store.rs:226-248`
- Plan (`.agents/rewrite-progress.md` Phase 7 KV goals) says the API surface is `get / set / delete / set_if_absent / increment` — no scans, no prefix ops. The plan justification is "PG is not a tree store; prefix scans push the index sideways and invite footguns".
- Implementation adds `delete_prefix` doing `DELETE … WHERE key LIKE $1` against both tables. With ~1M rows this is a sequential scan + write-lock storm. Worse, the user-facing `prefix` is dropped straight into `format!("{escaped}%")` with no length cap — a malicious caller can pass an empty prefix and wipe the entire KV.
- This is a smoking gun for plan deviation and a destructive footgun in one method.
- Fix: delete the method.

**C3. JWT secret has no production startup refusal**
- File: `crates/forge-core/src/config/auth.rs` (whole module)
- `AuthConfig::jwt_secret: Option<String>` defaults to `None`. There is no validator that refuses to boot in `release`/`production` if the secret is missing, dev-default, or shorter than 32 bytes. The plan called this out as the entire reason for keeping `reject_secret_defaults`, but no equivalent check exists for the case where the field is simply unset.
- A misconfigured prod deploy boots happily with no auth secret, then produces invalid signatures on first request — or worse, accepts forged tokens if any code path falls back to a hard-coded dev key.
- Fix: add a `validate()` step run from `ForgeConfig::load`; refuse `None`/`< 32` bytes when `RUST_ENV=production` (or equivalent), surface as `ForgeError::Config`.

### HIGH

**H1. `reject_secret_defaults` kept despite plan saying remove it**
- File: `crates/forge-core/src/config/loader.rs` (function still present and wired in)
- Plan: "drop reject_secret_defaults — replaced by typed validators on the AuthConfig struct". Implementation kept the old function and added nothing on the typed side. So we have the worst of both: a brittle textual scan + no real production check (see C3).
- Fix: remove `reject_secret_defaults`, replace with proper validators on `AuthConfig`, `DatabaseConfig`, etc.

**H2. Rate-limit and cache state not on KV — direct plan violation**
- Files: `crates/forge-runtime/src/rate_limit/limiter.rs`, `crates/forge-runtime/src/function/cache.rs`
- Plan Phase 7: "fold rate-limit counters and query cache invalidation into the KV layer so we have one durable state primitive". Implementation kept both as process-local `DashMap` / `RwLock<HashMap>`, and the KV module is unaware of either.
- Consequences: per-node rate limits drift across cluster (a 1000 rpm limit becomes `1000 × N` nodes); cache invalidation does not cross node boundaries (LISTEN/NOTIFY drives one node's cache, the rest serve stale data on next request); restarts blow away all rate-limit state.
- Fix: move counters to `forge_kv_counters` (already exists), move invalidation to a NOTIFY broadcast keyed by table name.

**H3. Four hand-rolled duration parsers, none agree on accepted suffixes**
- Files: `crates/forge-core/src/util/mod.rs:parse_duration`, `crates/forge-core/src/config/types.rs:DurationStr` (delegates to util), `crates/forge-core/src/rate_limit/mod.rs` (private parser), plus an inline parser in `gateway` config helpers.
- The plan said adopt `humantime` and stop. We now have 4 parsers, all slightly different — the `rate_limit` one is missing `ms` support entirely (so `100ms` is rejected for rate-limit windows but accepted for gateway timeouts).
- `parse_duration` and `parse_size` use raw `value * multiplier` with no `checked_mul` — a config of `9999999999w` overflows silently to a small number.
- Fix: depend on `humantime`, delete all four custom parsers.

**H4. UTF-8 corruption in `substitute_env_vars`**
- File: `crates/forge-core/src/config/loader.rs:substitute_env_vars`
- The function iterates `bytes[i] as char` to scan for `${`. For any non-ASCII byte (UTF-8 continuation), `as char` produces a Latin-1 codepoint, and the resulting string is **re-encoded as UTF-8** when handed back to TOML. So a config with a non-ASCII string literal (Unicode project name, tenant ID, password) is silently mangled into mojibake.
- Repro: set `project.name = "café"` in `forge.toml`, watch it become `cafÃ©`.
- Fix: operate on `&str` and `char_indices`, or use a real shell-like expander (`shellexpand` crate).

**H5. `HybridRateLimiter::new` ignores `config.max_local_buckets`**
- File: `crates/forge-runtime/src/rate_limit/limiter.rs` (constructor) and `crates/forge/src/runtime.rs:582-602`
- Config field exists, doc says it caps in-memory buckets. Constructor does not read it. The internal `DashMap` is unbounded.
- Operationally: a DDoS scanning random API keys grows the map without limit; OOM in minutes.
- Fix: thread `max_local_buckets` into the limiter, evict by LRU when over cap.

**H6. KV TTL cleanup runs on every Worker node, not leader-only**
- File: `crates/forge/src/runtime.rs:582-602` (the `tokio::spawn` calling `cleanup_expired`)
- Cleanup is started unconditionally per Worker. With 5 nodes, you have 5 deletes-of-expired racing every cleanup interval, hammering `forge_kv` with redundant write locks. Plan says "leader-only background tasks" for exactly this reason.
- Fix: gate behind `LeaderElection::is_leader()` like cron and other periodic work.

**H7. `RateLimiter::cleanup()` is dead code**
- File: `crates/forge-runtime/src/rate_limit/limiter.rs` (the public `cleanup` fn)
- Method exists, has no callers. `Worker` never schedules it. So `HybridRateLimiter` keeps every bucket forever — even cleanup-by-eviction relies on it. Combined with H5 this is the OOM path.
- Fix: spawn a periodic cleanup task in `runtime.rs` or call from the worker tick.

**H8. `QueryCache::invalidate_by_tables` walks the entire HashMap under a write lock**
- File: `crates/forge-runtime/src/function/cache.rs:152-159`
- `entries.retain(|k, _| !query_names.iter().any(|name| k.function_name == *name))` is O(N×M) under a global write lock. With a hot mutation that touches 3 tables and a cache of 10k entries, every mutation blocks every read for the full scan. The whole reason `FunctionRouter` builds a `table_to_queries` reverse index is to avoid this scan — but the cache itself doesn't use it.
- Fix: keep a `HashMap<table, HashSet<CacheKey>>` inside the cache, drop the keys directly. Or push the index into `QueryCache::set` and use it in `invalidate_by_tables`.

**H9. Cache invalidation runs *after* mutation handler returns, not after commit**
- File: `crates/forge-runtime/src/function/router.rs` (the `invalidate_cache_for_mutation` call site)
- The mutation handler can `?` out of a SQL transaction with the rows updated but the COMMIT not yet flushed. If invalidation fires before `Drop` of the transaction guard, a concurrent reader can re-populate the cache with the *old* row and the cache stays stale until the next mutation.
- Fix: invalidate inside the `MutationContext::commit` path, or use the existing `ChangeListener` LISTEN/NOTIFY signal which fires on commit.

### MEDIUM

**M1. KV API bloat — 8 helpers vs. plan's 5**
- File: `crates/forge-runtime/src/kv/store.rs`
- Plan API: `get / set / delete / set_if_absent / increment`. Shipped: `get / get_string / get_json / set / set_string / set_json / set_if_absent / delete / increment / get_counter / reset_counter / cleanup_expired / delete_prefix` — 13 methods.
- Each typed wrapper duplicates a serde call you can do in two lines at the call site. They also fragment error semantics (e.g., `get_string` returns `Deserialization` for non-UTF-8, but `get_json` returns the same variant for unrelated JSON parse failures).
- Fix: keep the 5 the plan called for, delete the rest.

**M2. No namespacing in KV keys**
- File: `crates/forge-runtime/src/kv/store.rs` everywhere
- All callers share one flat keyspace. Two unrelated subsystems can collide on a key like `"leader"` or `"locked"`. Plan called for a namespace prefix per subsystem.
- Fix: take a `namespace: &'static str` in the constructor, prepend `{namespace}:` to every key.

**M3. `default_true` helpers repeated across config modules**
- Files: `auth.rs`, `cluster.rs`, `database.rs`, `gateway.rs`, `mcp_config.rs`, `observability.rs`, `realtime_config.rs`, `signals.rs`, `worker.rs`
- Each module redefines `fn default_true() -> bool { true }` and `fn default_false() -> bool { false }`. ~9 copies.
- Fix: one `pub(crate) fn default_true` in `config/types.rs` (already a shared module).

**M4. `SizeStr` derives `Default` → 0 bytes**
- File: `crates/forge-core/src/config/types.rs`
- A field like `max_request_size: SizeStr` defaulting to 0 bytes silently rejects every request. The plan called for `Option<SizeStr>` or explicit `default_*` fns instead of a meaningless zero default.
- Fix: remove the `Default` derive; force every site to provide an explicit default.

**M5. `DurationStr::as_millis` truncates `u128` to `u64`**
- File: `crates/forge-core/src/config/types.rs:DurationStr::as_millis`
- `Duration::as_millis()` returns `u128`; the wrapper does `as u64`. A duration of `> 584M years` truncates silently. Realistic case: a config typo of `"100w"` accidentally written as `"100weeks"` triggers a fallback path elsewhere that hands `u128::MAX` in.
- Fix: return `u128`, or use `try_into()` and propagate.

**M6. `auth_cache_scope` falls back to `unwrap_or_default` on serde failure**
- File: `crates/forge-runtime/src/function/router.rs` (`auth_cache_scope` helper)
- If serializing the auth claims fails, the cache key falls back to `String::default()` (empty string). Two different users with un-serializable claim shapes would now share a cache slot.
- Fix: surface as `ForgeError::Internal`, refuse to cache.

**M7. `cleanup_local` uses `Instant::now() - max_idle` (panic risk)**
- File: `crates/forge-runtime/src/rate_limit/limiter.rs` (`cleanup_local` body)
- Subtraction on `Instant` panics on underflow. If `max_idle` is configured larger than the process uptime (common during dev / CI), the first cleanup tick panics and crashes the worker.
- Fix: use `Instant::now().checked_sub(max_idle).unwrap_or(epoch)`.

### LOW

**L1. `delete_prefix` LIKE escaping is incomplete**
- File: `crates/forge-runtime/src/kv/store.rs:227`
- Escapes `\`, `%`, `_` but not `[` (some PG configs treat it as a class start in `LIKE`). Either way moot if we delete the method per C2.

**L2. `parse_var_with_default` uses `find('-')` without bounds check on `:-` form**
- File: `crates/forge-core/src/config/loader.rs`
- Splitting `${VAR:-default}` on `find('-')` finds the wrong dash if the variable name contains one (`MY-VAR:-x` → split is wrong). Spec for shell uses `:-` as a single separator.
- Fix: split on `":-"` first, then `"-"`.

**L3. The 4th private duration parser in `rate_limit/mod.rs` has no `ms` support**
- File: `crates/forge-runtime/src/rate_limit/mod.rs`
- Accepts `s/m/h/d` but not `ms`. Subsumed by H3; flagged separately because the failure mode is silent — `100ms` is parsed as `0`.

**L4. `config/mod.rs` is still 818 lines, of which 558 are inline tests**
- File: `crates/forge-core/src/config/mod.rs`
- Plan goal was to slim mod.rs to ~150 lines after splitting per-section. Tests stayed inline.
- Fix: move tests under `tests/config_*.rs` or per-module `#[cfg(test)]`.

**L5. `is_valid_env_var_name` validator exists but the substitution scanner doesn't call it**
- File: `crates/forge-core/src/config/loader.rs` (helper defined, not invoked)
- Means malformed names like `${1FOO}` substitute as if literal then later panic on env read. Dead code path until you trigger it.

<!-- AGENT_PHASE_7_END -->

---

## Cross-Cutting Concerns

<!-- AGENT_CROSS_START -->

### Findings

#### CRITICAL — workspace dependency budget plan unmet
**File**: `Cargo.toml:1-110`
**Plan**: `.agents/rewrite/03-DEPENDENCY-BUDGET.md` requires `resolver = "3"`, trimmed `tokio` features (`rt-multi-thread, macros, net, time, sync, signal, process`), `syn` without `extra-traits`, drop `tracing-subscriber` json feature, drop `anyhow`/`jsonschema`/`bcrypt`/`tracing-opentelemetry`/`opentelemetry-*`, add workspace deps `subtle`, `humantime`, `figment`. Target ~200 transitive crates and ~90s cold build.
**Actual**:
- Line 2: `resolver = "2"` (still v1's default).
- Line 41: `tokio = { version = "1.48", features = ["full"] }` pulls every Tokio feature; `full` was specifically called out to remove.
- Line 80: `syn = { version = "2.0", features = ["full", "extra-traits", "visit"] }` — `extra-traits` was the example bad case in the budget doc.
- Line 69: `tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }` still has `json`.
- Line 67: `anyhow = "1.0"` still in workspace deps; budget says drop everywhere except CLI seam.
- Line 51: `jsonschema = "0.28"` still present (also pulled in directly by `crates/forge-runtime/src/gateway/mcp.rs`).
- Lines 73-78: full `tracing-opentelemetry`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-semantic-conventions`, `opentelemetry-appender-tracing` block intact; budget said drop the entire OTel SDK and emit OTLP via plain `reqwest`.
- No `subtle`, `humantime`, `figment` workspace deps were added.
- `parking_lot` and `once_cell` still in the transitive tree (pulled by sqlx/opentelemetry).
**Impact**: Cold build budget will not be met; ~40 extra transitive crates for OTel SDK alone. `subtle` is a hard requirement for security item 33 (constant-time PKCE compare); without the workspace pin, every consumer either rolls its own or copy-pastes — exactly the fragmentation the budget was meant to prevent.

#### CRITICAL — `WorkflowStatus` enum has 12 variants, plan caps at 6
**File**: `crates/forge-core/src/workflow/traits.rs:127-152`
**Plan**: `07-DELETION-LIST.md` collapses to `Created, Running, Waiting, Completed, Failed, Compensated` (6 variants). Compensation phase folded into `Compensated`; `Blocked*`, `RetiredUnresumable`, `CancelledByOperator` removed because operators should patch the active version, not maintain split brains.
**Actual** (12 variants, all referenced by `crates/forge-runtime/src/workflow/executor.rs` lines 138, 231, 363, 423, 460, 770-776): `Created, Running, Waiting, Completed, Compensating, Compensated, Failed, BlockedMissingVersion, BlockedSignatureMismatch, BlockedMissingHandler, RetiredUnresumable, CancelledByOperator`.
**Impact**: Doubles state-machine complexity. Three "blocked" terminal variants and the `RetiredUnresumable`/`CancelledByOperator` operator-only variants all add CLI surface, RPC variants, gateway readiness checks, migrations, and frontend mappings. Forbidden under pre-1.0 zero-tech-debt policy.

#### CRITICAL — `OutboxBuffer` pattern still present, plan rejected it
**File**: `crates/forge-core/src/function/context.rs` (`OutboxBuffer` field on `MutationContext`); flush in `crates/forge-runtime/src/function/router.rs`.
**Plan**: `02-ARCHITECTURE.md` and `05-PG-DOCTRINE.md` say "no buffered side effects, dispatches happen inside the request transaction; if the handler returns Err, the tx rolls back and nothing was dispatched." OutboxBuffer was a v1 wart that arose because dispatch happened on the wrong connection.
**Actual**: Buffer fills during the handler, flushes post-handler in router. If the gateway crashes between handler return and flush, jobs/workflows are lost; if the flush partially succeeds, duplicates fire.
**Impact**: Atomicity guarantee promised in `docs/docs/build/jobs.mdx` is false. Replace with `dispatch_job()` taking the request `&mut PgConnection` and inserting inline.

#### HIGH — `jsonwebtoken` version drift between workspace and `forge-runtime`
**File**: `Cargo.toml:105` pins `jsonwebtoken = "9"`. `crates/forge-runtime/Cargo.toml:44` overrides with `jsonwebtoken = { version = "10", default-features = false, features = ["rust_crypto", "use_pem"], optional = true }`.
**Impact**: Two majors compiled into the dep tree, two crypto stacks, larger binary, larger attack surface. `cargo tree` confirms both `jsonwebtoken v9.3.1` and `jsonwebtoken v10.3.0` are in the lockfile. Pick 10 in the workspace and remove the override.

#### HIGH — 9 test contexts in `crates/forge-core/src/testing/context/`, plan says 4
**Files**: `context/{query,mutation,job,cron,workflow,daemon,webhook,mcp_tool,mod}.rs` (~2,743 LOC total).
**Plan**: `07-DELETION-LIST.md` collapses to `TestQueryContext, TestMutationContext, TestJobContext, TestWorkflowContext` and uses generics for the rest.
**Impact**: Every new handler type adds a new context file; framework grows linearly with handler count when it should be O(1).

#### HIGH — `gen_random_uuid()` instead of `uuidv7()` everywhere
**File**: `crates/forge-runtime/migrations/system/v001_initial.sql` (every `DEFAULT gen_random_uuid()`).
**Plan**: `05-PG-DOCTRINE.md` mandates UUIDv7 (`uuidv7()` in PG18) for natural index ordering and B-tree locality on append-heavy tables (`forge_jobs`, `forge_signals_events`, etc.).
**Impact**: Random UUIDs cause B-tree page splits and index thrash on every insert. UUIDv7 is *the reason* PG18 was made a hard requirement; not using it discards a major perf win.

#### HIGH — `signals_stub.rs` still present despite 07-DELETION-LIST entry
**File**: `crates/forge-runtime/src/signals_stub.rs`
**Plan**: Delete; signals is a feature-gated subsystem so the stub is dead weight.
**Impact**: Workspace lint denies `dead_code`; only compiles because the gate puts it behind `#[cfg(not(feature = "signals"))]`. Either delete the gate or delete the stub.

#### HIGH — old `cron/scheduler.rs` and `daemon/runner.rs` retained alongside the new bridge
**Files**: `crates/forge-runtime/src/cron/scheduler.rs` (528 LOC), `crates/forge-runtime/src/cron/registry.rs` (97 LOC), `crates/forge-runtime/src/daemon/runner.rs` (586 LOC), `crates/forge-runtime/src/daemon/registry.rs` (81 LOC).
**Plan**: `07-DELETION-LIST.md` says delete the standalone scheduler/runner; cron and daemons run through the shared job worker pool.
**Actual**: Bridge added (commit `12c0c3a`) but the original modules were never removed; both code paths exist. `.agents/rewrite-progress.md` notes "daemons kept separate" as a deviation but doesn't mark it as a debt to repay.
**Impact**: Two leader-election mechanisms, two registries to keep in sync, and the deletion plan was the entire reason cron-on-jobs was introduced.

#### HIGH — docs still ship the v1 `tables = ["foo"]` syntax
**Files**: `docs/docs/start/anatomy.mdx:63`, `docs/docs/tutorials/realtime-todo.mdx:94,106,444`, `docs/docs/build/read-data.mdx:153`, `docs/docs/build/subscribe-to-changes.mdx:87`, `docs/skills/forge-idiomatic-engineer/references/api.md:25`.
**Plan**: Phase 3 macro change requires `#[query(tables("foo", "bar"))]` (function-call syntax). The CLAUDE.md docs policy says any framework change must update `docs/docs/` AND `docs/skills/forge-idiomatic-engineer/references/`.
**Impact**: Every example in the docs fails to compile. Skill references that the AI agent consumes will produce broken code.

#### HIGH — workspace fmt check fails
**Issue**: `cargo fmt --all -- --check` reports violations in `crates/forge-macros/src/{enum_type,mcp_tool,model,mutation,query,sql_extractor}.rs`, `crates/forge-runtime/src/cron/bridge.rs`, `crates/forge-runtime/src/function/registry.rs`, `crates/forge-runtime/src/gateway/oauth.rs`.
**Impact**: CI `cargo fmt --all --check` step (`.github/workflows/ci.yml`) fails. Either CI is currently red or the check was disabled.

#### HIGH — 15 `todo!()` in `forge-codegen/src/parser.rs` despite `clippy::unimplemented = "deny"`
**File**: `crates/forge-codegen/src/parser.rs:654, 674, 703, 721, 739, 755, 844, 849, 854, 859, 864, 869, 874, 987, 1006`
**Issue**: `todo!()` is deliberately not covered by `clippy::unimplemented`, but the workspace's "no panics" policy (`clippy::panic = "deny"`, `clippy::unwrap_used = "deny"`) plainly intends to forbid this pattern. Fifteen `todo!()` macros in a single file is "we'll come back to it" code that violates pre-1.0 zero-tech-debt policy.
**Impact**: Codegen will panic with "not yet implemented" for whichever Rust types the parser doesn't recognise (probably generics or rare type constructors). Expand `clippy::todo = "deny"` workspace lint and fix the holes properly.

#### MEDIUM — three files past the 1,200 LOC red flag
**Files**: `crates/forge-runtime/src/gateway/mcp.rs` (1,838 LOC), `crates/forge/src/runtime.rs` (1,660 LOC), `crates/forge/src/cli/check.rs` (1,644 LOC).
Additionally, eight more files are between 1,000 and 1,300 LOC (`function/context.rs:1454`, `gateway/sse.rs:1289`, `realtime/reactor.rs:1273`, `sql_extractor.rs:1208`, `jobs/queue.rs:1156`, `gateway/oauth.rs:1080`, `gateway/auth.rs:1077`, `workflow/executor.rs:1056`, `codegen/parser.rs:1022`, `workflow/context.rs:1015`, `gateway/server.rs:1002`).
**Plan**: `01-PRINCIPLES.md` "400 LOC target, 1,200 LOC red flag." Three files past the red line, none flagged for split in `rewrite-progress.md`.
**Impact**: Single-responsibility violation; `mcp.rs` alone duplicates registry+executor+router responsibilities from elsewhere in the runtime.

#### MEDIUM — monolithic `v001_initial.sql` (828 LOC)
**File**: `crates/forge-runtime/migrations/system/v001_initial.sql`
**Plan**: `07-DELETION-LIST.md` splits per subsystem (`v001_cluster.sql`, `v002_jobs.sql`, `v003_workflows.sql`, …) so each subsystem owns its schema and feature gates control which migrations run.
**Impact**: A user opting out of `signals` still gets all signals tables created; subsystem ownership is gone.

#### MEDIUM — no `MERGE INTO` or `RETURNING OLD/NEW` usage despite PG18 requirement
**Issue**: zero hits for `MERGE INTO`, `RETURNING OLD`, `RETURNING NEW` across `crates/forge-runtime/`.
**Plan**: `05-PG-DOCTRINE.md` lists these as the reason PG18 was made minimum; cluster heartbeat, leader leases, and KV upserts should all use `MERGE`.
**Impact**: Existing code still uses `INSERT ... ON CONFLICT DO UPDATE` in places `MERGE` would be cleaner and atomic. PG18-only guarantee is leaked but not used.

#### MEDIUM — 131 `#[allow(...)]` suppressions across 107 source files
**Plan**: `01-PRINCIPLES.md` requires zero suppressed lints in main code; allow only with a `// reason:` comment and a tracking ticket.
**Impact**: Lint hygiene degraded; the workspace lint policy is partially circumvented file-by-file. Audit each one and remove or document.

#### MEDIUM — `resolver = "2"` instead of `"3"` despite edition 2024
**File**: `Cargo.toml:2` (resolver), `Cargo.toml:23` (edition = "2024").
**Plan**: `03-DEPENDENCY-BUDGET.md` calls out resolver 3 as a prerequisite for the budget plan to take effect.
**Impact**: edition 2024 with resolver 2 is a known feature-unification footgun; the budget cuts won't bite without resolver 3.

#### MEDIUM — `ParallelBuilder` still present, plan removed it
**File**: `crates/forge-core/src/workflow/parallel.rs:24` (`pub struct ParallelBuilder`), `crates/forge-core/src/workflow/context.rs:885` (`pub fn parallel`).
**Plan**: `07-DELETION-LIST.md` "drop `ParallelBuilder`; `tokio::join!` is the idiomatic replacement and the builder added no value over the language primitive."
**Impact**: One more API to test, document, and version; users who reach for it write code that doesn't compose with `?`/`async` like vanilla `join!` does.

#### LOW — `delete_prefix` still in KV store
**File**: `crates/forge-runtime/src/kv/store.rs:226`
**Plan**: `07-DELETION-LIST.md` strips KV down to `get/put/delete`. `delete_prefix`, `cas`, `scan_prefix`, `decrement`, `expire`, `ttl` were all removed.
**Actual**: `delete_prefix` survives. Drop it; otherwise it grows callers and users start complaining when it goes.

### Summary: 17 findings (3 CRITICAL, 9 HIGH, 4 MEDIUM, 1 LOW)

<!-- AGENT_CROSS_END -->

---

## Regressions vs v1

<!-- AGENT_REGRESSIONS_START -->

### Findings

#### CRITICAL — v2-NEW security additions from `06-SECURITY-CARRY-FORWARD.md` not implemented
- **Item 27** `max_jobs_per_request` (DoS guard): no field in `crates/forge-core/src/config/gateway.rs`, no enforcement in the request path. A single mutation can buffer unbounded `dispatch_job()` calls.
- **Item 28** `max_result_size_bytes`: no enforcement in `FunctionExecutor`; large query results stream through SSE without a cap.
- **Item 41** `max_json_depth`: no parser depth guard; arbitrarily-nested JSON bodies parse fine and burn CPU.
- **Item 40** per-route `csp_overrides`: no config field, no middleware. The static CSP set in `gateway/server.rs` cannot be loosened for routes that legitimately need inline scripts.
- **Item 42** `Access-Control-Max-Age`: not set on the CORS layer; preflight repeats on every request.
- **Item 43** JWT `kid` header missing — `crates/forge-runtime/src/gateway/auth.rs:211` builds `jsonwebtoken::Header::new(self.algorithm)` and never sets `header.kid`. The validation path *expects* `kid` for JWKS lookup (line 337), so issued tokens fail self-validation in any RS256 setup. Tokens cannot be rotated without an outage.
- **Item 6** `legacy_secrets` per-secret TTL — `crates/forge-core/src/config/auth.rs:90` declares `pub legacy_secrets: Vec<String>` (bare strings, no TTL or `valid_until` field). Item 6 says "each legacy secret has a `not_after` timestamp; expired secrets are auto-pruned." Without TTL there is no rotation discipline; secrets pile up forever.
- **Item 33** `subtle::ConstantTimeEq` for PKCE: zero hits for `subtle` or `ConstantTimeEq` across `crates/forge-runtime/src` and `crates/forge-core/src`. PKCE verifier compare is a `==` string compare, timing-leaks the verifier byte-by-byte.
**Impact**: All seven items are TEST REQUIRED in the plan. Each is independently shippable; together they constitute the bulk of the v2 security uplift the rewrite promised.

#### CRITICAL — `pg_notification_queue_usage` monitoring missing on `/_api/ready`
**File**: `crates/forge-runtime/src/gateway/server.rs` `readiness_handler`
**Plan**: `05-PG-DOCTRINE.md` "/_api/ready returns 503 when `pg_notification_queue_usage() >= 0.50` so load balancers route around backpressured nodes." Without it the LISTEN/NOTIFY pipeline silently saturates and reactivity collapses.
**Actual**: Readiness only checks `db_ok && reactor_ok && workflows_ok`. Backpressure is invisible until queue full and PG starts dropping notifications.

#### HIGH — PgBouncer detection missing
**File**: `crates/forge-runtime/src/db/` (no startup-time detection).
**Plan**: `05-PG-DOCTRINE.md` says detect PgBouncer at startup and refuse to boot — Forge requires session-level features (LISTEN, advisory locks held by session) that PgBouncer transaction pooling silently breaks.
**Impact**: Production deploys behind PgBouncer transaction mode will deadlock advisory locks and lose NOTIFY messages with no error message.

#### HIGH — JWT algorithm set narrowed without migration message
**File**: `crates/forge-core/src/config/auth.rs:13-22`
**v1 supported**: HS256, HS384, HS512, RS256, RS384, RS512, ES256, EdDSA.
**v2 supports**: HS256, RS256 only (per `06-SECURITY-CARRY-FORWARD.md` "narrow to two; everything else is footgun-by-default").
**Impact**: Existing v1 users with HS512/RS384/etc. tokens get a config parse error on first v2 boot with no migration hint. Pre-1.0 policy says breaking changes are encouraged, but the parse error must explicitly say "this algorithm was removed; mint new tokens with HS256 or RS256."

#### HIGH — KV API surface narrowed without runtime error message
**File**: `crates/forge-runtime/src/kv/store.rs`
**v1 had**: `get/put/delete/get_versioned/cas/scan_prefix/decrement/expire/ttl/delete_prefix`.
**v2 has**: `get/put/delete` (+ leftover `delete_prefix`).
**Impact**: Apps calling `kv.cas()` or `kv.expire()` get a compile error after upgrading; the migration path is "redesign your code." `get_versioned` was the only way to do optimistic concurrency in v1 and has no replacement. The CHANGELOG must call this out per-method with a v2 idiomatic alternative (TTL → cache TTL on the row, CAS → transactional update, etc.).

#### HIGH — webhook signature variants narrowed
**File**: `crates/forge-runtime/src/webhook/handler.rs`
**v1 supported**: `HmacSha1`, `HmacSha256`, `HmacSha512`, `StandardWebhooks`.
**v2 supports**: `HmacSha256` only.
**Impact**: GitHub (Sha1) and Stripe-compatible (StandardWebhooks-style) integrations break. Either keep the variants or document the migration to a userland verifier.

#### HIGH — signal types narrowed: `vital`/`user` removed
**File**: `crates/forge-runtime/src/signals/endpoints.rs`
**v1 had**: `event`, `view`, `vital`, `user`, `report` (5 endpoints).
**v2 has**: `event`, `view`, `report` (3 endpoints).
**Impact**: Frontend `ForgeSignals.trackVital()` and `ForgeSignals.identifyUser()` calls compile to runtime 404s. `packages/forge-svelte` and `packages/forge-dioxus` need matching client trims, but the changelog must call this out as a frontend breaking change.

#### MEDIUM — Phase 3 `tables = [...]` → `tables(...)` macro syntax breaking change
**File**: `crates/forge-macros/src/query.rs`
**Issue**: When users upgrade and hit `unexpected token: =`, the compiler error is unhelpful. Add a parser arm that detects the old form and emits `compile_error!("the `tables = [...]` syntax was removed in v2; use `tables(\"foo\", \"bar\")`")` so the migration hint is front and center. The doc rewrite called out in cross-cutting findings is required regardless.

#### MEDIUM — daemons left as a separate subsystem despite plan
**File**: `crates/forge-runtime/src/daemon/`
**Plan**: `07-DELETION-LIST.md` "daemons fold into long-running jobs" (a perpetual job claimed by leader, restarts on failure).
**Actual**: Separate `DaemonRunner`, `DaemonRegistry`, leader lock. `.agents/rewrite-progress.md` notes this as a "deliberate" deviation but doesn't justify why the plan was wrong; pre-1.0 policy is no compat shims, so either delete daemons or rewrite the plan doc.

#### MEDIUM — `WorkflowStatus` operator-only variants regress on plan terminal states
**File**: `crates/forge-core/src/workflow/traits.rs:140-152` (`CancelledByOperator`, `RetiredUnresumable`, `BlockedMissingVersion`, `BlockedSignatureMismatch`, `BlockedMissingHandler`).
**Plan stance**: split brains are a smell; users should redeploy with the active version restored, not maintain operator-cancellation paths.
**Impact**: 5 extra status values bleed into RPC types, frontend type unions, migrations, gateway readiness checks, and admin tooling. Each is a regression vs the simpler 6-state v2 design and produces frontend type drift the moment the enum gains another variant.

### Summary: 10 findings (2 CRITICAL, 5 HIGH, 3 MEDIUM, 0 LOW)

<!-- AGENT_REGRESSIONS_END -->

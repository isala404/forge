# Agent-first API audit — what to fix before 1.0

Scope: public surface of `forgex`, `forge-core`, the 10 attribute macros, contexts, `forge.toml`, and the CLI. Lens: a coding agent writing handlers from intuition, with no awareness of the underlying Postgres-only infrastructure.

The North Star is "the agent writes `#[forge::query/mutation/job/workflow/...]` and the framework hides infra." Most of the framework gets this right. The findings below are the places it doesn't, ordered by how badly an agent will trip over them.

---

## Findings

### F1. `dispatch_job` / `start_workflow` are string-keyed, not type-safe — Critical-for-GA
- **Location:** `crates/forge-core/src/function/context.rs:1221` (`MutationContext::dispatch_job`), `:1279` (`start_workflow`); duplicated on `JobContext`, `WebhookContext`, `DaemonContext`, `McpToolContext`. Trait at `function/dispatch.rs`.
- **Agent failure mode:** Agent writes `#[forge::job] pub async fn send_welcome_email(...)`, then calls `ctx.dispatch_job("welcome_email", input)` (wrong name) or `ctx.dispatch_job("send_welcome_email", WrongShape { ... })`. Both compile, both fail at runtime — the *one* thing strongly-typed Rust ought to catch. Whole categories of agent-generated bugs survive `cargo check`.
- **Fix:** macros already generate `SendWelcomeEmailJob` types — wire dispatch through them. `ctx.dispatch::<SendWelcomeEmailJob>(input)` (or `SendWelcomeEmailJob::dispatch(&ctx, input)`). Keep `dispatch_by_name` as an escape hatch behind a separate method for dynamic cases. Same for `start_workflow`. This is the highest-leverage change in this list.

### F2. KV store is implemented but invisible to handlers — Critical-for-GA
- **Location:** `crates/forge-runtime/src/kv/store.rs` and `migrations/system/v004_kv.sql` ship a `forge_kv` table with TTL plus a counters table. No `ctx.kv()` exists on any handler context; not in the prelude.
- **Agent failure mode:** Agent needs "cache this for 5 minutes" or "rate-limit per user" or "remember last-seen". Without a discoverable KV, it invents an ad-hoc table, or reaches for an external dep (Redis), defeating the North Star. The runtime *already does this work*; the API just doesn't surface it.
- **Fix:** add `ctx.kv()` to `HandlerContext` returning `KvHandle` with `get/set/set_with_ttl/delete/incr`. Document in `api.md`. Same call site from every handler kind.

### F3. No `dispatch_job_at(...) / dispatch_job_after(...)` on the context — High
- **Location:** `JobQueue::dispatch_with_delay` exists in `crates/forge-runtime/src/jobs/dispatcher.rs:36`, but `MutationContext::dispatch_job` and friends have no delay/`scheduled_at` parameter.
- **Agent failure mode:** "Send a reminder in 24 hours" is one of the most common agent prompts. Agent either writes a cron + DB row + flag scan (bad), or a workflow with `ctx.sleep` (over-engineered for a one-shot reminder).
- **Fix:** `ctx.dispatch_job_after(JobType, input, Duration)` and `ctx.dispatch_job_at(JobType, input, DateTime<Utc>)` directly on every context that already has `dispatch_job`. Fold into F1's type-safe dispatch.

### F4. Context methods drift across handler kinds — High
- **Location:**
  - `QueryContext::db() -> ForgeDb` vs `MutationContext::db() -> DbConn<'_>` vs `JobContext::db() -> ForgeDb`. Same method name, three return types.
  - `MutationContext::conn() -> ForgeConn<'_>` exists; `JobContext::conn()` returns `ForgeConn<'static>`; `QueryContext` has none. `bypass_pool()` only on `MutationContext`.
  - `dispatch_job` is `async` on `MutationContext`/`JobContext`/`WebhookContext`/`DaemonContext`/`McpToolContext` (good), but `QueryContext` deliberately has no dispatch (correct — but undocumented at the trait level).
  - Logging helpers: `CronContext::info/warn/error/debug` exist (`cron/context.rs:159+`). No equivalent on `JobContext`, `DaemonContext`, `WorkflowContext`, `MutationContext`. Agent guesses `ctx.info(...)`, gets a compile error, falls back to `tracing::info!`.
- **Agent failure mode:** Agent reads "use `ctx.db()`" in one place, applies it elsewhere, gets a confusing type error or — worse — uses `MutationContext::db()` thinking it's pool-backed (it's the transaction). Code review missed.
- **Fix:**
  1. Pick one name for the pool view and one for the connection view; apply uniformly. The `HandlerContext` trait now exists in `forge-core/src/context.rs` but the inherent methods still diverge. Remove the inherent methods (or make them return the same type). The `MutationContext::db()` returning the active tx while `HandlerContext::db()` returns the pool is a *trap*.
  2. Add `ctx.log_info/warn/error` on every context (cron has it; everyone else should). Or remove them from cron for consistency and tell the agent to use `tracing::*` everywhere.

### F5. `transactional` defaults to `true` but `transactional = false` is silently a footgun — High
- **Location:** `crates/forge-macros/src/mutation.rs:106`, `:199`. The macro errors at compile time if you call `dispatch_job` with `transactional = false` — good. But `transactional = false` still allows arbitrary side effects, and there's no compile-time guarantee that a `transactional = true` mutation isn't doing non-rollback-safe work (raw HTTP via `ctx.http()`, file writes).
- **Agent failure mode:** Agent sets `transactional = false` to "make this faster," then later adds a `dispatch_job` — compile error fires. Good. But the opposite case: `transactional = true` + `ctx.http().post(...)` happily inside the transaction. On rollback, HTTP side effect already happened.
- **Fix:** Make `ctx.http()` on `MutationContext` either (a) refuse to fire until commit (outbox-style), (b) hard-warn at compile time when `transactional = true` and `ctx.http()` is called, or (c) only available on a `MutationContext::after_commit()` callback. Decide once. Right now it's the agent's responsibility, which violates the North Star.

### F6. Scoping enforcement is regex-fragile and only on `#[query]` — High
- **Location:** `crates/forge-macros/src/query.rs:282-310`. Compile-time check requires SQL containing `user_id` or `owner_id`. No equivalent check on `#[mutation]` — agent can write a mutation that updates *anyone's* row and the framework says nothing.
- **Agent failure mode:** `#[forge::mutation] update_profile(ctx, input: { user_id: Uuid, name: String })` → updates whoever the input says. Massive IDOR. The query macro would catch the same shape; the mutation macro doesn't.
- **Fix:** Apply the same scope check to mutations. Better: a typed `ctx.owner_id()` that gets injected into queries automatically, with an explicit `#[query/mutation(unscoped)]` to opt out. The string match (`user_id`/`owner_id`) is also too narrow — projects with `account_id`, `org_id`, `tenant_id` get false positives requiring `unscoped`.

### F7. `MutationContext::db()` returns the active transaction; `HandlerContext::db()` returns the pool — Critical-for-GA
- **Location:** `crates/forge-core/src/context.rs:84` — comment says "intentionally bypasses the active transaction." Inherent `MutationContext::db()` at `function/context.rs:1042` returns `DbConn<'_>` which *is* the transaction.
- **Agent failure mode:** Generic helper `fn count<C: HandlerContext>(ctx: &C)` silently reads uncommitted-by-this-tx state. Agent writes a helper, uses it in a mutation, sees stale data, can't explain it. The exact "data structures first" trap — two semantically different things share a name.
- **Fix:** Rename one. Suggest: `ctx.db()` → always the pool; `ctx.tx()` → the active tx (only on `MutationContext`); the trait method is `HandlerContext::db()`. Then `MutationContext` doesn't even have an inherent `db()` shadowing the trait. Right now it does and that's the bug-magnet.

### F8. Workflow versioning has hidden compile-time invariants the agent will miss — High
- **Location:** `forge-macros/src/workflow.rs`. Signature is FNV-1a over step keys, wait keys, timeout, input/output. Changing a step name silently produces a new signature; registering under the same `(name, version)` fails at startup, not compile.
- **Agent failure mode:** Agent renames a step (`"create_user"` → `"create-user"`) "for consistency." Compiles. App fails to boot with a signature-conflict error the agent has to reverse-engineer from server logs. Worse, agent doesn't know "old runs get blocked" until production.
- **Fix:** Either (a) make step keys idents not strings so renames are git-detectable, (b) emit a compile-time warning when the workflow has *any* existing runs in offline metadata, or (c) introduce a `forge check` lint that warns on workflow contract drift. At minimum, the macro should reject a workflow without explicit `version =` rather than defaulting silently.

### F9. `#[forge::cron]` schedule is a string; expression typos found only at compile via `cron` crate — Medium
- **Location:** `forge-macros/src/cron.rs`. Cron expressions are validated at compile time (good). But "every 5 minutes" requires `"*/5 * * * *"` knowledge.
- **Agent failure mode:** Agent guesses wrong field order (cron has 5- and 6-field variants). Compile error is `cron`-crate native — usable but not delightful.
- **Fix:** Accept a duration sugar — `#[forge::cron(every = "5m")]` or `#[forge::cron(daily_at = "03:00", timezone = "UTC")]` alongside the raw expression. Map sugar → cron internally. Most agent prompts ("once a day", "every hour") never need raw cron.

### F10. `#[forge::daemon]` requires manual `tokio::select! { ctx.shutdown_signal() }` — Medium
- **Location:** macro docs at `forge-macros/src/lib.rs:296`. Every daemon must hand-roll the shutdown loop or it never shuts down cleanly.
- **Agent failure mode:** Agent writes `loop { do_work(); tokio::time::sleep(60s).await; }`. Daemon hangs on shutdown. CI catches it as a test timeout, agent retries until it remembers the pattern.
- **Fix:** Provide `ctx.tick(Duration)` that internally selects on shutdown and returns `Ok(())` to continue or breaks the loop. Or generate the loop scaffold from a `interval = "60s"` attribute and pass the loop body as a closure. Daemons-as-singletons-that-poll is 90% of the use case.

### F11. `JobContext::saved` / `save` / `set_saved` are three overlapping APIs — Medium
- **Location:** `job/context.rs:177` (`saved` returns `Value`), `:185` (`set_saved` replaces all), `:207` (`save(key, value)` merges).
- **Agent failure mode:** Agent reads docs, picks `set_saved`, wipes earlier `save` calls. Or uses `save` then `saved()` and doesn't know if it's the merged value.
- **Fix:** Keep `save(key, value)` and `load(key)`. Drop `saved()`/`set_saved()` from the public API; if there's a use case for "replace whole bag," name it `clear_then_save_all`.

### F12. Auth scoping on `#[forge::job]` is dispatch-time only and easily inert — Medium
- **Location:** macro docs claim `require_role("admin")` "requires admin role to dispatch." But jobs are usually dispatched from inside a mutation; the role of the *mutation caller* is what gets checked. There's no compile-time link.
- **Agent failure mode:** Agent puts `require_role("admin")` on a job, then dispatches it from a `#[forge::mutation(public)]` thinking "the job is admin-only." Wrong — the role check fires only if the job is dispatched by an unauthenticated client (which only happens for inbound webhooks etc.).
- **Fix:** Either rename to make the semantic explicit (`dispatch_requires_role = "admin"`), or drop role on jobs entirely and require the caller (mutation) to enforce. The current name suggests something it doesn't deliver.

### F13. `start_workflow` takes a string name; workflow versioning makes the right name nontrivial — High
- **Location:** `function/context.rs:1279`. Just `workflow_name: &str`. Versioning happens server-side ("active version pin").
- **Agent failure mode:** Agent writes `ctx.start_workflow("user_onboarding_v2", ...)` after seeing the v2 file. Wrong: the *logical* name is `user_onboarding`. The framework infers `name` from the function ident unless overridden, so `user_onboarding_v2` becomes the logical name unintentionally. Agent debugs in production.
- **Fix:** Tied to F1. `ctx.start::<UserOnboardingWorkflow>(input)` — type system enforces the logical name. The macro can refuse to derive the logical name from a function whose ident ends in `_v\d+`, forcing an explicit `name =`.

### F14. `MutationContext::http()` exists but is a footgun mid-transaction — High
- **Location:** `function/context.rs:1059`. HTTP client returns a working `HttpClient`. No warning about transaction-scope leakage.
- **Agent failure mode:** Agent calls Stripe inside a mutation, transaction rolls back later, payment took. Classic.
- **Fix:** see F5. Either remove `ctx.http()` from `MutationContext`, or rename it `ctx.http_unsafe_in_tx()` (ugly but loud), or buffer-and-flush-after-commit. The "no external calls in mutations" rule is in the trait doc-comment; that's not enforcement.

### F15. `forge.toml` defaults aren't safe-by-default — Medium
- **Location:** generated `forge.toml` in `examples/with-*/*/forge.toml`. Observations:
  - `cors_origins = ["http://localhost:9080", ...]` — fine for dev, but the template doesn't ship a production variant. Agent runs `forge new`, pushes to prod, CORS is wide open to localhost.
  - `mcp.oauth = true` in the demo template but the minimal omits auth entirely. No middle ground.
  - `database.pool_size = 50` (default) is comment-documented as "size as worker.max_concurrent + reactor cap + ... + ~6". Agent will not do this math.
  - No default `signals.enabled` shown in the templates even though docs say "enabled by default."
- **Agent failure mode:** Agent ships the template config to production unchanged. Either security (CORS) or capacity (pool_size) bites.
- **Fix:** `forge.toml` should support a `[deploy]` section that switches on `FORGE_ENV=production` and forces sane requirements (CORS allowlist non-localhost, JWT secret present, observability enabled). `forge check --production` validates. Also auto-size `pool_size` from `worker.max_concurrent` if unset.

### F16. Discoverability: 10 macros, no `forge::*` index in the prelude — Medium
- **Location:** prelude at `crates/forge/src/runtime.rs:104`. Re-exports types but not the proc macros (they live at `forge::query`, `forge::mutation`, etc. at crate root). Agents trained on "import the prelude" still have to qualify each macro.
- **Agent failure mode:** Agent writes `use forge::prelude::*; #[query] ...` — fails (the macro isn't in the prelude). Writes `#[forge::query]` — works. Inconsistent muscle memory.
- **Fix:** Either re-export the macros via the prelude (`pub use crate::{query, mutation, job, ...}`) or stop re-exporting context types via the prelude so the style is uniform ("everything is `forge::X`"). Pick one.

### F17. `Result` is the Forge `Result` but `ForgeError::Function`, `ForgeError::Job` etc. are duplicative — Medium
- **Location:** `error.rs`. Variants: `Function`, `Job`, `JobCancelled`, `Cluster`, `Database`, `Sql`, `Internal`, `InvalidState`, `Io`, `Config` — many overlap. `Sql` vs `Database`? `Function` vs `Internal`?
- **Agent failure mode:** Agent picks `ForgeError::Function` for an arbitrary internal issue when it should be `Internal`. Or wraps a sqlx error in `Internal` instead of letting the `From<sqlx::Error>` impl produce `Database`. Inconsistent error responses to the frontend.
- **Fix:** Collapse to ~8 variants with clear ownership. Drop `Function`, `Sql`, `Job`, `Cluster`, `Io` — they're all "Internal with a tag." Keep the HTTP-mapped ones (`NotFound`, `Unauthorized`, `Forbidden`, `Validation`, `Timeout`, `RateLimitExceeded`, `InvalidArgument`) since those have user-facing status code semantics, plus `Database`, `Internal`, `Config`. Add a structured cause chain if richer telemetry is wanted.

### F18. Testing contexts don't share a builder shape with production contexts — Medium
- **Location:** `crates/forge-core/src/testing/context/*.rs`. `TestMutationContext::builder().as_user(uuid).with_role("admin")` is great. But the *non-test* `MutationContext::new(db_pool, auth, request)` takes positional `AuthContext` — no builder.
- **Agent failure mode:** Agent writes a test, then tries to construct a context in main code for some glue (e.g., a `tower::Layer`), can't, copies the test builder, ends up coupling production code to `forge-core::testing`.
- **Fix:** Mirror the builder pattern on production contexts. Construction is private anyway (framework calls it); the builder is what users (including tests) should see.

### F19. No first-class email / notification primitive — Medium
- **Location:** none. Agent prompts to send email → must write an integration job calling Resend/Postmark via `ctx.http()`.
- **Agent failure mode:** Every project reinvents an `email_send` job. The North Star says "everything backed by Postgres" but doesn't say "skip ubiquitous tasks." Email is one.
- **Fix:** Ship a `forge-email` crate (or `[email]` config section) with pluggable provider (SMTP, SES, Resend) and `ctx.email().send(...)` available on mutation/job/workflow contexts. Implement the queue as a forge job under the hood. Mock in test contexts. Same logic for `forge-storage` (S3/R2 uploads) and `forge-search` (pg full-text). Make these *opt-in features* but discoverable from the prelude.

### F20. `webhook` handler can't subscribe to its own RPC for replay — Low/Medium
- **Location:** `webhook/context.rs`. Idempotency exists but there's no `ctx.replay()` or "dead-letter for me" affordance.
- **Agent failure mode:** Stripe webhook fails to deserialize one event in a thousand. Agent has no built-in way to inspect and re-fire it; ends up adding a side-table for raw bodies.
- **Fix:** Auto-store raw webhook body keyed by idempotency key with TTL. Expose `forge webhook replay <id>` in CLI.

### F21. `forge generate` is implicit; codegen output is not part of the build graph — Medium
- **Location:** `cli/generate.rs`. Frontend bindings are generated when the agent remembers to run `forge generate`. Otherwise type drift goes undetected.
- **Agent failure mode:** Agent adds a `#[forge::mutation]`, runs `cargo build`, ships. Frontend doesn't see it. Agent doesn't realize until they `bun dev` and the frontend errors on stale types.
- **Fix:** `forge check` already runs codegen verification — make `cargo build` do it via a build script in templates, *and* make the CI template smoke-test fail on drift. Or commit the generated bindings and have `forge check` diff them.

### F22. Workflow `ctx.step("name", closure)` re-uses string keys — High
- **Location:** `workflow/context.rs:348` `record_step_start`, `:390` `record_step_complete`. Step names are runtime strings.
- **Agent failure mode:** Same as F8 — agent renames a step in source, signature changes silently, old runs blocked. Compounded by the fact that `ctx.step("foo", ...)` looks like a label, not a contract.
- **Fix:** Either a typed step API (`#[step] fn foo(...)` extracted by the workflow macro), or hash step keys *with the file path* so a deliberate move still hashes the same as long as the name matches. At minimum, document this loudly in the workflow doc-comment.

### F23. `#[forge::query(public)]` and `#[forge::query(unscoped)]` look similar; one is auth, one is scope — Medium
- **Location:** `forge-macros/src/query.rs:44-48`. Both keywords appear in macro attribute lists, both look like "open it up." Their semantics are unrelated (`public` = no auth, `unscoped` = no row filter).
- **Agent failure mode:** Agent writes `#[query(public)]` thinking it also disables scoping → compile error about missing `user_id`. Adds `unscoped` to "fix it" → query is now both unauthenticated *and* unscoped. Reasonable code review misses this.
- **Fix:** Rename. `public` → `auth = "none"`; `unscoped` → `scope = "global"`. Same attribute axis ("the visibility/access policy"). And gate `scope = "global"` behind a louder name like `scope = "no_owner_filter"` or require it together with a `// SAFETY: ...` doc-comment.

### F24. `ctx.user_id()` returns `Result<Uuid>` everywhere except `AuthContext::user_id()` which returns `Option<Uuid>` — Low
- **Location:** `function/context.rs:511` (Option) vs `:792` and trait `AuthenticatedContext::user_id` (Result).
- **Agent failure mode:** Agent calls `.unwrap()` on the wrong one.
- **Fix:** Pick one shape across the codebase. `AuthContext::user_id() -> Option<Uuid>` is fine if it's clearly "raw auth state"; the context-level `user_id()` should always return `Result` because the contract is "require auth."

### F25. `forge new` debug-build patches `Cargo.toml` with local `[patch.crates-io]` — Low
- **Location:** `cli/new.rs`. Convenient for framework dev, dangerous for users running a local debug forge.
- **Agent failure mode:** Agent builds forge from source, runs `forge new`, generated project depends on absolute paths on the host. Agent zips it up and the project doesn't build anywhere else.
- **Fix:** Guard behind an explicit `FORGE_DEV=1` env var, or emit a warning, or only do it when the working directory is *inside* the forge repo.

---

## Pre-GA breaking changes to make NOW

Ranked by leverage (how many agent failures it removes) × cost-to-change-later (post-1.0).

1. **Type-safe dispatch** (F1, F3, F13). `ctx.dispatch::<JobType>(input)`, `ctx.start::<WorkflowType>(input)`, `ctx.dispatch_after::<JobType>(input, Duration)`. Removes the single largest class of agent runtime bugs. Macros already emit the types — just route through them.
2. **Resolve `db()` ambiguity** (F7, F4). Inherent `MutationContext::db()` returning a tx while the trait returns a pool is the worst footgun in the codebase. Rename one: pool stays `db()`, transaction becomes `tx()`. Or vice versa — but pick one and apply uniformly to every handler kind.
3. **Surface KV on the context** (F2). One-line addition to `HandlerContext`. Unlocks "cache this," "rate-limit per user," "remember last-seen," all without external infra.
4. **Mutation transaction safety** (F5, F14). Decide: either ban `ctx.http()` in transactional mutations at compile time, or buffer-and-flush-after-commit like the outbox already does for jobs. The "agent should remember" version is not agent-first.
5. **Scope check on mutations** (F6). Apply the query-style `user_id`/`owner_id` macro check to mutations. Broaden the column-name heuristic (`tenant_id`, `account_id`, `org_id`). Or replace with an injected `ctx.owner_id()` parameter that mutations *must* use in their WHERE.
6. **Rename `public`/`unscoped`** (F23). `auth = "none"` and `scope = "global"`. Two unrelated axes, two clearly distinct attribute names.
7. **Collapse error variants** (F17). After 1.0 each variant is a breaking change. Cut to ~10.
8. **Typed step/wait keys in workflows** (F8, F22). Either compile-time step idents or attribute-derived keys. String-keyed durable contracts are an after-1.0 nightmare.
9. **Logging methods uniformly on every context** (F4). `ctx.log_info/warn/error/debug` everywhere, or remove from `CronContext` and standardize on `tracing::*`.
10. **First-class affordances: email, scheduled-one-off, replay** (F19, F3, F20). Add as features, advertise in the prelude. Removes the most common "agent invents a side table" failure.
11. **`forge.toml` production mode** (F15). `FORGE_ENV=production` + `forge check --production`. Pool sizing auto-derived.
12. **Daemon loop ergonomics** (F10). `ctx.tick(Duration) -> bool` (or similar) so daemons can't forget the shutdown channel.
13. **Builder for production contexts** (F18). Mirror test builders; avoid `forge-core::testing` leaking into prod glue.
14. **Re-export macros via prelude OR force `forge::query` style** (F16). Consistency.
15. **Auto-run codegen on `cargo build`** (F21). Make drift impossible.

After 1.0 these become migration efforts. Now they're commits.

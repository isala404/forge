# Documentation Audit — Forge Framework

Scope: `docs/docs/` (Docusaurus, user-facing) + `docs/skills/forge-idiomatic-engineer/references/` (AI skill, `api.md`, `frontend.md`, `patterns.md`, `pitfalls.md`, plus undocumented sidekicks `recipes.md`, `resilience.md`, `testing.md`, `frontend/`). Cross-referenced against `crates/forge-macros`, `crates/forge-core`, `crates/forge-runtime`, `crates/forge`, `packages/forge-svelte`, `packages/forge-dioxus`.

The two doc surfaces are required by `CLAUDE.md` to stay in sync. Today they diverge meaningfully, and several user-facing features have **no user documentation at all** despite being in the skill ref or vice versa.

---

## Findings

### F1. `start/first-app.mdx` teaches a registration model the framework no longer uses — Critical

`docs/docs/start/first-app.mdx:100-160` walks the user through `Forge::builder().register_query::<functions::ListTodosQuery>().register_mutation::<functions::CreateTodoMutation>()`. Every shipped template (`examples/with-svelte/minimal/src/main.rs:15`, `examples/with-svelte/demo/src/main.rs:15`) uses `Forge::builder().auto_register()`, which the macros wire up via `inventory::submit!`. A first-time user copying the docs will end up with empty registries or duplicate registrations. This is the very first page a new user reads.

Fix: rewrite the section around `.auto_register()`, point at the inventory mechanic in `start/anatomy.mdx`, mention manual `register_*` only as an escape hatch.

### F2. `ctx.http_with_circuit_breaker()` is documented in user docs but does not exist — Critical

`docs/docs/build/write-data.mdx:216,258` references `ctx.http_with_circuit_breaker()` and says `ctx.http()` returns a raw `reqwest::Client`. Source: `crates/forge-core/src/function/context.rs:1059` — `ctx.http()` already returns `HttpClient` (circuit-breaker-backed). The skill `api.md:148` documents it correctly. Anyone following the user doc gets a compile error.

Fix: replace the section in `build/write-data.mdx` with the `api.md` description; delete the `http_with_circuit_breaker` rows from any context tables.

### F3. `reference/errors.mdx` is stale against `ForgeError::http_status()` — High

The canonical mapping lives at `crates/forge-core/src/error.rs:180`. The user errors page misses three variants entirely and gets one status code wrong:

| Variant | In source | In `reference/errors.mdx` |
|---|---|---|
| `Conflict(String)` → 409 | yes (`error.rs:99`) | missing |
| `UnprocessableEntity(String)` → 422 | yes (`error.rs:105`) | missing |
| `ServiceUnavailable(String)` → 503 | yes (`error.rs:109`) | missing |
| `Deserialization(String)` → 400 | yes (`error.rs:187`) | listed as 500 (line 34) |

The skill `api.md` table at line 405–422 is correct for `Deserialization=400` but also omits `Conflict`, `UnprocessableEntity`, `ServiceUnavailable`. Both surfaces need updates.

Fix: regenerate the error table from `ForgeError::http_status` doc comment; add a CI check that diffs the comment table against `errors.mdx`.

### F4. No documentation page for admin/operator endpoints — Critical (operations)

`/_api/admin/*` (jobs cancel/retry/force-abort, workflows cancel/retry/force-abort, queues pause/resume, nodes, leaders) is fully implemented (`crates/forge-runtime/src/gateway/admin.rs`) and documented in the **skill ref only** (`api.md:284-307`). User docs have zero coverage. Operators run a production app without knowing the endpoints exist.

Fix: new page `reference/admin-api.mdx` (and sidebar entry). Cover audit log (`forge_admin_audit`), reason field convention, `admin` role requirement, examples for the common incident-response calls.

### F5. No documentation for `/_api/ready` semantics — High

The probe is referenced obliquely in `scale/multiple-nodes.mdx` and a couple of deploy snippets but the response shape, the five flags (`database`, `reactor`, `notify_queue_ok`, `migrations_ok`, `cluster_registered`), and the PG 18 minimum (`MIN_POSTGRES_MAJOR`) are documented only in skill `api.md:308-330`. Critical for k8s/load-balancer configuration.

Fix: section in `ship/deploy.mdx` "Health probes" with the flag table and remediation hints; mirror in skill `patterns.md` operations section.

### F6. OAuth 2.1 / MCP authorization endpoints lack a user-facing reference — High

`/oauth/authorize`, `/oauth/token`, `/oauth/register` (PKCE S256, DCR, 60s codes, client-bound refresh tokens, X-Frame-Options, etc.) are implemented in `crates/forge-runtime/src/gateway/oauth.rs`. `ship/mcp-security.mdx` describes the security profile but never lists the endpoints, request/response shapes, or registration flow. `reference/wire-protocol.mdx` doesn't mention them at all. Anyone wiring an MCP client has nothing to copy from.

Fix: new page `reference/oauth.mdx` covering endpoints, payloads, status codes, CSRF cookie, the `forge:mcp` audience scoping, and the sticky-session caveat from `scale/multiple-nodes.mdx:340`.

### F7. `custom_routes` is documented in skill ref but not in user docs — High

`ForgeBuilder::custom_routes(|pool| Router)` is a commonly-needed escape hatch (CSV exports, special-case endpoints, etc.). Covered well in skill `api.md:332-348` including the conflict path list. The user-facing `build/custom-handlers.mdx` doesn't show this API at all (it talks about MCP and OAuth instead).

Fix: rename / expand `build/custom-handlers.mdx` (or new page `build/custom-routes.mdx`) with the factory, reserved path list, and "middleware applies automatically" note.

### F8. `RoleResolver` trait undocumented in user docs — Medium

Custom RBAC via `RoleResolver` is in skill `api.md:354-379` with a worked hierarchy example. User docs have zero mention. `build/protect-routes.mdx` only explains the static `roles` JWT claim. Any team needing dynamic RBAC, tenant-scoped permissions, or DB-backed roles has to read source.

Fix: new section in `build/protect-routes.mdx` "Custom role resolution" with the registered-via-builder example.

### F9. Worker queue model (`default`, `workflows`, `cron`, custom) under-documented — High

The reserved queue names with default sizes (default=8, workflows=4, cron=2) and the `worker_capability` tag-to-queue routing are documented in skill `api.md:198-208` and (sparsely) in `scale/worker-pools.mdx`. There's no page that explains: how `worker_capability` on `#[forge::job]` maps to `[worker.queues.<name>]`, how `$workflow_resume` and `$cron:<name>` queue names are reserved, what "heavy traffic on one queue cannot starve another" means in practice. Pre-1.0 this is the single most operationally important config block.

Fix: rewrite `scale/worker-pools.mdx` from first principles — diagram the claim SQL, the reserved queues, the capability matching, and pause/resume from admin API.

### F10. Reactivity has an internals page but no mental-model page — High

`scale/reactivity.mdx` (92 lines) describes the pipeline (NOTIFY → InvalidationEngine → Reactor → SSE). What's missing for users: when *should* you call `forge_enable_reactivity()`, what's the cost (trigger overhead per row, `forge_change_log` retention, hash recompute), what shouldn't be reactive (large result sets, time-windowed queries), and how subscriptions interact with auth scope dedup (`AuthScope` in the hash). `build/subscribe-to-changes.mdx` shows usage but doesn't establish the mental model. Skill `patterns.md` similarly thin.

Fix: new page `start/reactivity-model.mdx` or a "How it works" section in `build/subscribe-to-changes.mdx` with the row-level vs table-level adaptive tracking rule (<100 subs → row, >100 → table) which is currently buried in `CLAUDE.md` and not in any doc.

### F11. No first-class "Security model" page — Critical (GA blocker)

There are scattered security notes (`mcp-security.mdx` only covers MCP; bits about SSRF / private host blocking in `crates/forge-core/src/http`; admin audit logging; query scoping; outbound deny-list). There's no single "Forge security model" page covering:

- AuthN: JWT validation, HS256/RS256/JWKS, kid rotation, legacy_secrets table, dev_mode refusal in production
- AuthZ: roles, RoleResolver, query scope enforcement, `unscoped` escape hatch
- Tenant isolation (`TenantIsolationMode` in `crates/forge-core/src/tenant/mod.rs` — totally undocumented)
- Outbound SSRF guard (private host blocked → 403 Forbidden, from `crates/forge-core/src/http`)
- Rate limit modes (`hybrid` vs `strict`)
- Admin audit trail
- TLS posture (internal-only, no HSTS/ACME)
- DNT/Sec-GPC handling on signals

Fix: new page `ship/security.mdx` as the canonical entry, link from each handler page.

### F12. `TenantIsolationMode` is implemented but undocumented — High

`crates/forge-core/src/tenant/mod.rs` defines `None`, `Strict`, `ReadShared`. No mention in `docs/docs/` or in any skill ref. `auth.tenant_id()` is documented in `build/protect-routes.mdx:175`, but the isolation modes that consume it, and the row-level enforcement story, aren't. For B2B/multi-tenant deployments this is essential.

Fix: section in the new `ship/security.mdx` or a dedicated `build/multi-tenancy.mdx`.

### F13. Signals: server endpoint discriminator missing from user docs — Medium

Skill `api.md:257-279` documents the single `POST /_api/signal` with `type: "event" | "view" | "report"` and the DNT/Sec-GPC short-circuit. `ship/signals.mdx` describes the client API (`track`, `identify`, `captureError`) and the schema, but doesn't document the wire endpoint or how to call it directly from a non-Forge client. Auto-captured event types (`rpc_call`, `server_execution`, `web_vital`, `breadcrumb`) are also missing from the user page.

Fix: add a "Wire format" section to `ship/signals.mdx` with the type discriminator table from `api.md`.

### F14. `forge env` and `forge doctor` exist in CLI but only in CLI reference — Medium

`reference/cli.mdx:347-385` covers both commands. They're not mentioned in `start/first-app.mdx` (which is where a new user would benefit most from `forge env` to wire shell completions and `SQLX_OFFLINE`) — actually it *is* mentioned at first-app.mdx:24-29, but not `forge doctor`. New users hitting "DATABASE_URL not set" or "cargo-sqlx missing" don't know `forge doctor` will tell them.

Fix: add `forge doctor` to the troubleshooting section of `start/first-app.mdx` and `tutorials/shipping-to-production.mdx`.

### F15. No error catalog with code → cause → remediation — High

`reference/errors.mdx` lists variants and HTTP codes but doesn't catalogue actual production-observable errors: "signature mismatch" workflow states, `BlockedMissingVersion`, `notify_queue_ok=false`, `circuit open`, `rate limit exceeded` retry-after handling on the wire. Operators triaging an incident need a table keyed by symptom.

Fix: extend `reference/errors.mdx` with a "Runtime conditions" section covering each `WorkflowStatus` variant (`BlockedMissingVersion`, `BlockedSignatureMismatch`, `BlockedMissingHandler`, `RetiredUnresumable`, `CancelledByOperator`) and the readiness flag failure modes.

### F16. `WorkflowStatus` variants under-documented — High

`crates/forge-core/src/workflow` defines 10 statuses including `BlockedMissingVersion`, `BlockedSignatureMismatch`, `BlockedMissingHandler`, `RetiredUnresumable`, `CancelledByOperator`, `Compensating`, `Compensated`. `build/long-processes.mdx:380-390` mentions the simple states but not the blocked/retired states or how to recover (admin `retry` to re-pin, `force-abort` to retire). Skill `api.md` mentions `Created, Running, Waiting, Completed, Compensating, Compensated, Failed, BlockedMissingVersion, BlockedSignatureMismatch, BlockedMissingHandler, RetiredUnresumable, CancelledByOperator` in passing but doesn't link to remediation.

Fix: state-transition table in `build/long-processes.mdx` plus a "Recover a blocked workflow" runbook in the new admin-api page.

### F17. Tutorials don't show the full breadth of macro attributes — Medium

The four tutorials are good for the happy path but never demonstrate: `cache="30s"`, `consistent`, `rate_limit(...)`, `idempotent(key=...)`, `compensate="fn"`, `worker_capability`, `staging` workflow status, `replay_window_secs` webhook attr, `audience_required` auth. They live in `reference/attributes.mdx` but aren't taught.

Fix: add an "Advanced macro attributes" page under `build/` cross-linking reference, with copy-paste idiomatic snippets for each.

### F18. No "5-minute" getting-started path — High

`start/first-app.mdx` is 274 lines and walks model → query → migration → mutation → reactivity → frontend. It is the only entry point. A first-impression "see something running" path is missing: `forge new ... && cd ... && docker compose up && open localhost:9080`, period. Demo templates already do this, but the docs land on a heavy tutorial.

Fix: trim `start/first-app.mdx` to a sub-5-minute "run the template" step, move the build-it-yourself walkthrough to a new `tutorials/your-first-feature.mdx` or merge into `tutorials/realtime-todo.mdx`.

### F19. No "deployment topology" / "production architecture" page — Critical (operations)

`ship/deploy.mdx` (496 lines) covers building a binary, env vars, TLS, single-node deploy. `scale/multiple-nodes.mdx` covers HA. Neither is the **first thing** an operator looking to deploy reads. We need a single "How to deploy Forge" reference page with: single-binary vs split worker/api, load balancer in front, sticky sessions for SSE+OAuth, DATABASE_URL pool sizing, Postgres 18 requirement, blue/green caveat (workflow signatures!), migration order in rolling deploys.

Fix: new page `ship/production-architecture.mdx` or restructure existing pages with a clearer "I have written code, how do I run it in prod" answer.

### F20. Migration documentation missing operational details — High

`reference/cli.mdx:287-340` covers `forge migrate`. What's missing: forward-only rationale, idempotency expectations (the doc says "don't use IF NOT EXISTS" but doesn't explain why), the `forge_system_migrations` ledger, advisory lock semantics during a rolling deploy, `forge_enable_reactivity('table')` lifecycle on schema changes, what to do when migrations are out of order across branches, how to roll forward a bad migration. None of this is anywhere.

Fix: new page `ship/migrations.mdx` (or expand existing reference) with the operations playbook.

### F21. Cluster setup / node roles is fragmentary — High

`scale/multiple-nodes.mdx` exists but doesn't tie back to: advisory-lock leader election (`crates/forge-core/src/cluster/`), `forge_nodes` table heartbeats, the `cluster_registered` readiness flag, `[node] roles = [...]` array (referenced in `ship/configuration.mdx:25` with no full enumeration), `worker_capabilities`. The skill `patterns.md` is similarly thin. Result: nobody knows what a "scheduler" role does vs "gateway" + "worker".

Fix: dedicated `scale/cluster-architecture.mdx` page; document `roles = ["gateway", "worker", "scheduler"]` and how leader election keys them.

### F22. `Cargo.toml` features (`gateway`, `worker`, `api`, `minimal`, `geoip`, `otel`) — Medium

Documented in skill `api.md:439-459`. **Zero mention** in user docs. Splitting a deployment into API-only and worker-only nodes is a primary scaling lever and users don't know it's a feature-flag away.

Fix: add a "Build presets" section to `ship/deploy.mdx` or `scale/multiple-nodes.mdx`. Cross-link from the feature-gate error message itself.

### F23. Frontend client API reference is non-existent — High

`packages/forge-svelte/src/` (`client.ts`, `auth.ts`, `signals.ts`, `hooks.ts`, `upload.ts`) and `packages/forge-dioxus/src/` (`client.rs`, `auth.rs`, `signals.rs`, `hooks.rs`, `upload.rs`) define the public client surface. User docs have `connect/generated-client.mdx` (generated bindings) and `connect/track-progress.mdx` (progress hooks) — nothing reference-style. `getForgeClient()`, `ForgeProvider`, `useForgeAuth`, `setForgeAccessToken`, retry/reconnect behaviour, error mapping, the live store contract (`{ data, loading, error, refresh }`) — none of these have a reference page. Skill `frontend.md` (155 lines) covers it for the AI but not for humans.

Fix: new `reference/client-svelte.mdx` and `reference/client-dioxus.mdx`. Treat them like API docs, generated from JSDoc/rustdoc.

### F24. `forge_enable_reactivity()` and PG helper functions undocumented as an API — Medium

Used everywhere (`migrations/0002_todos_reactivity.sql`, etc.) but never given a stable reference. Same for `forge_trim_change_log`, `forge_notify_change` trigger function. Reserved table prefix `forge_*` is in skill `api.md:487` but not user docs.

Fix: add a "PostgreSQL helpers" reference page or a section in `reference/wire-protocol.mdx`-adjacent reference covering the SQL surface that's part of the framework contract.

### F25. Testing framework: skill ref has it, user docs partial — Medium

`ship/testing.mdx` (866 lines) is comprehensive for *backend* testing — that part is fine. What's missing from user docs but present in skill `testing.md`: `IsolatedTestDb::setup()` signature requirement (`forge::get_internal_sql()`), the assertion macros catalogue (`assert_job_dispatched!`, `assert_workflow_started!`, `assert_http_called!`), webhook job-dispatch assertions, and the multi-claim/role builder methods. Frontend testing (Playwright fixtures from `tests/fixtures.ts` — `rpc()`, `gotoReady()`, `uniqueId()`, `ACTION_TIMEOUT`) gets exactly zero documentation despite being shipped in every example template.

Fix: expand `ship/testing.mdx` "Frontend tests" section using the fixtures from `examples/with-svelte/demo/frontend/tests/fixtures.ts`.

### F26. Inconsistent terminology across docs — Medium

Same concept, different names:

- "Function" vs "handler" vs "RPC function" — used interchangeably (`reference/contexts.mdx` says "handler", `connect/generated-client.mdx` says "function", skill `api.md` mixes both)
- "Job queue" vs "worker pool" vs "queue" — `scale/worker-pools.mdx` calls them pools, admin endpoints call them queues, config calls them `[worker.queues.<name>]`
- "Subscription" vs "reactive query" vs "live store" — `connect/generated-client.mdx` uses all three on the same page
- "Outbox buffer" (skill) vs "transactional outbox" (`build/long-processes.mdx`) vs "buffered jobs" (`build/write-data.mdx`)

Fix: a short Glossary page at `reference/glossary.mdx`; settle on one canonical term per concept and fix the others.

### F27. No doctests / no executable examples — Medium

`grep -r '```rust no_run\|```rust ignore' docs/docs/` is empty. Every Rust snippet is a non-compiled markdown block. Snippets in `start/first-app.mdx` already drift from source (F1). For pre-1.0 with breaking changes, snippets need either doctests (compiled in `crates/forge-core` lib docs as `///` examples — which exist but aren't surfaced) or a CI grep that diffs snippets against the example apps.

Fix: at minimum, add a CI job that compiles every fenced ```rust block in `docs/docs/` against the workspace (cargo doc-test pattern). Long-term: source the canonical snippets from `examples/with-svelte/demo` via Docusaurus partials.

### F28. Reference page completeness: not every handler has a parity reference — High

`reference/attributes.mdx` (619 lines) covers all 10 macros. `reference/contexts.mdx` (551 lines) covers most contexts, but: `McpToolContext` is light, `WebhookContext` is light on `WebhookResult` variants, `DaemonContext` shutdown signal isn't fully laid out. The skill `api.md` Context Capability Matrix at line 384-403 is the clearest summary of the whole framework — and it's only in the AI ref.

Fix: copy the Context Capability Matrix into the top of `reference/contexts.mdx`. Audit each context section against the source methods.

### F29. Skill references have content not surfaced in `docs/docs/` — Medium

Inverse of most findings above: `docs/skills/forge-idiomatic-engineer/references/resilience.md` (58 lines on circuit breaker SSRF guard, half-open state machine), `recipes.md` (300 lines of patterns), and parts of `patterns.md` and `pitfalls.md` contain genuinely useful knowledge that humans would want. They are not linked from `docs/docs/` and not findable by Docusaurus search.

Fix: either promote `pitfalls.md` content into a user-facing "Common pitfalls" page, or surface skill refs via a Docusaurus plugin / sidebar link. Today they're invisible to non-AI readers.

### F30. `Pre-1.0 Policy` and breaking-change posture not in user docs — Medium

`CLAUDE.md` says "Breaking changes are encouraged without a migration path if they produce a cleaner API or simpler internals" — this is a non-trivial expectation that should appear in `docs/docs/` (probably under `index.mdx` or a `start/stability.mdx` page) so users adopting pre-1.0 know what they're signing up for. Today it lives only in the contributor-facing `CLAUDE.md`.

Fix: add a "Stability and versioning" section to `docs/docs/index.mdx` or a dedicated page.

---

## Pre-GA documentation must-haves (priority order)

1. **Fix F1** — `start/first-app.mdx` must teach `.auto_register()`. This is the first thing every user sees and it currently doesn't work.
2. **Fix F2** — remove the `ctx.http_with_circuit_breaker()` lie in `build/write-data.mdx`.
3. **Fix F3** — sync `reference/errors.mdx` and skill `api.md` with `ForgeError::http_status()`; add the three missing variants. Wire a CI check.
4. **F11 Security model page** — `ship/security.mdx`. Pre-1.0 GA without a single security doc is a blocker.
5. **F19 Production architecture page** — `ship/production-architecture.mdx`. People will deploy this; they need a clear topology reference.
6. **F4 Admin/operator API page** — `reference/admin-api.mdx`. You cannot operate Forge in production without these endpoints; they must be discoverable.
7. **F5 Readiness probe documentation** — folded into deploy doc and security model. K8s/LB users need it on day one.
8. **F18 Five-minute path** — trim `start/first-app.mdx`. The current onboarding is 274 lines deep before the user sees the app run.
9. **F16 Workflow status state machine** — `build/long-processes.mdx`. The blocked states are unavoidable in production rolling deploys; document the recovery path.
10. **F20 Migration playbook** — `ship/migrations.mdx`. Forward-only without a runbook is a foot-gun.
11. **F23 Frontend client API references** — `reference/client-svelte.mdx`, `reference/client-dioxus.mdx`. Generated bindings are documented; the *runtime* surrounding them is not.
12. **F9 Worker queue model rewrite** — `scale/worker-pools.mdx`. Single most-tuned config block, currently scattered.
13. **F22 Cargo features in user docs** — `ship/deploy.mdx`. Worker/api split is invisible.
14. **F6 OAuth endpoint reference** — `reference/oauth.mdx`. Required by anyone wiring MCP clients.
15. **F12 Multi-tenancy doc** — pre-GA features that nobody knows exist will not get used.
16. **F27 CI snippet compilation** — stop F1/F2-class drift from happening again.
17. **F26 Glossary + terminology cleanup** — small effort, high readability win.
18. **F10 Reactivity mental model** — folded into existing pages or new `start/reactivity-model.mdx`.

Everything else (F7, F8, F13–15, F17, F21, F24, F25, F28–F30) is post-GA polish.

---

## Recommended docs IA changes

Current sidebar (from `docs/sidebars.ts`): Start, Tutorials, Build, Connect, Ship, Scale, Agents, Reference. Proposed adjustments:

**Start** — keep the section, but split:
- `start/first-app` (trimmed to 5-min path, F18)
- `start/anatomy` (keep)
- `start/reactivity-model` *(new, F10)*
- `start/stability` *(new, F30)*

**Build** — keep all 12 pages. Reframe `build/custom-handlers.mdx` to be about `custom_routes` (F7); pull MCP into its own page only.

**Connect** — keep `generated-client`, `track-progress`. Add the realtime store contract (link to F23 client reference).

**Ship** — promote to the heaviest section:
- `ship/configuration` (keep)
- `ship/security` *(new, F11)* — entry point for all security concerns
- `ship/production-architecture` *(new, F19)* — single-binary vs split, LB, sticky sessions
- `ship/migrations` *(new, F20)* — extracted from `reference/cli.mdx`
- `ship/deploy` (keep, leaner, link out to the new pages)
- `ship/testing` (keep, add Playwright fixtures section per F25)
- `ship/signals` (keep, add wire format section per F13)
- `ship/mcp-security` (keep)

**Scale** — rename `scale/multiple-nodes.mdx` to `scale/cluster-architecture.mdx` (F21); rewrite `scale/worker-pools.mdx` (F9); keep `scale/reactivity.mdx` as the internals page (separate from the new `start/reactivity-model`); fold `scale/overnight-success.mdx` content into the cluster page or production-architecture page (it's currently a mixed bag).

**Reference** — this is the biggest gap. Proposed pages:
- `reference/cli` (keep)
- `reference/attributes` (keep)
- `reference/contexts` (keep, add the Capability Matrix per F28)
- `reference/errors` (rewritten per F3 + F15)
- `reference/wire-protocol` (keep)
- `reference/observability-catalog` (keep)
- `reference/admin-api` *(new, F4)*
- `reference/oauth` *(new, F6)*
- `reference/client-svelte` *(new, F23)*
- `reference/client-dioxus` *(new, F23)*
- `reference/postgres-helpers` *(new, F24)* — `forge_enable_reactivity`, `forge_change_log`, `forge_*` reserved prefix
- `reference/glossary` *(new, F26)*

**Skill ↔ user docs sync**: introduce a short header on each skill-ref file pointing to its user-doc twin, and a CI script that warns when one is touched without the other (the policy in `CLAUDE.md` is already there — it needs enforcement). Promote `resilience.md`, the operational parts of `recipes.md`, and the operations section of `patterns.md` into user-facing pages (F29) so humans can find them.

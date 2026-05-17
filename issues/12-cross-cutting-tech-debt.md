# Cross-cutting Tech Debt, Consistency & Dependency Hygiene

Scope: workspace-wide build/dep/CI/example/package surface. Per-subsystem perf and security issues are out of scope (covered by 01–02 and a separate security audit).

Findings are ordered by severity within each cluster. Severity is the engineering cost paid by every future contributor or downstream user, not the runtime cost.

---

## A. Dependency hygiene

### A1. Workspace deps duplicated as ad-hoc direct versions — Medium
- `crates/forge-runtime/Cargo.toml:30` declares `futures-util = "0.3"`, `crates/forge-runtime/Cargo.toml:31` declares `sha2 = "0.10"`, `crates/forge-runtime/Cargo.toml:32` and `crates/forge-core/Cargo.toml:28` both pin `base64 = "0.22"` inline.
- `sha2`, `base64`, `futures-util` already need a single source of truth (some are partially in `[workspace.dependencies]`, e.g. `sha2`; others are not). The point of `[workspace.dependencies]` is to bump in one place — partial use defeats that.
- Fix: move `base64`, `futures-util`, `tokio-util`, `ring`, `hmac`, `sha1`, `aho-corasick`, `percent-encoding`, `serde_urlencoded`, `rustls-pemfile`, `tokio-rustls`, `rustls`, `tls-listener`, `db_ip`, `maxminddb`, `tempfile`, `rcgen` into `[workspace.dependencies]` and reference with `{ workspace = true }` from each crate. Today only the load-bearing crates are listed.

### A2. Examples bypass the workspace deps entirely — Medium
- `examples/with-svelte/minimal/Cargo.toml:14-22`, `examples/with-dioxus/demo/Cargo.toml:14-26`, `benchmarks/app/Cargo.toml:10-17` all redeclare `tokio = { version = "1", features = ["full"] }`, `serde = "1"`, `uuid = "1"`, `chrono = "0.4"`, `sqlx = "0.8"`, `reqwest = "0.12"`, etc. inline.
- `benchmarks/app/Cargo.toml:14` pins `jsonwebtoken = "9"` while the workspace pins `"10"` (Cargo.toml:108). Two major versions of `jsonwebtoken` are now compiled in the same lockfile every time benchmarks are touched.
- Fix: switch every example and benchmark to `{ workspace = true }`. Examples are the canonical user-facing template; they should model the dep-graph discipline we ask of users.

### A3. `opentelemetry` stack pinned to 0.27 with a TODO in `Cargo.toml` — Medium
- `Cargo.toml:74` says `# TODO(pre-1.0): Replace with plain OTLP via reqwest`. The whole 5-crate OTel block is now ~3 minor versions behind upstream (0.30+ is current) and the SDK has had breaking API changes that affect tracing-opentelemetry compatibility. Each `cargo update` will keep skipping these.
- Either commit to the "rip out otel-rust SDK, hand-roll OTLP/HTTP" plan now (pre-1.0 is the only window) or bump to current upstream and delete the TODO. Right now it is in limbo and the pinned versions are silently aging.

### A4. `schemars` pinned to `=0.8.22` — Low
- `Cargo.toml:50`: exact version pin. Comment-less. Likely papers over an incompatibility with `jsonschema 0.28` or `darling 0.20`. Either add an explanatory comment or bump to 0.9 (schemars released a major in 2025 with cleaner JSON Schema 2020-12 support, which is what MCP wants).

### A5. Duplicate `cargo install` of `cargo-deny` and `cargo-audit` per CI run — Low
- `.github/workflows/ci.yml:71-80`: every PR build does a fresh `cargo install cargo-deny --locked` (~90s) and `cargo install cargo-audit --locked` (~60s). `Swatinem/rust-cache@v2` with `save-if: false` caches almost nothing here because the install target is `~/.cargo/bin/` outside the standard cache scope.
- Fix: use `taiki-e/install-action@v2` with `cargo-deny,cargo-audit` — single-step prebuilt binaries, ~3s.

### A6. `RUSTSEC-2025-0134` (rustls-pemfile unmaintained) ignored with no removal target — Low
- `deny.toml:14-21` ignores it indefinitely. Acceptable today, but pre-1.0 policy says no permanent workarounds. Fix: swap to `rustls-pki-types::PemObject` now (it is already in the dep graph transitively via `rustls 0.23`) and delete the ignore.

---

## B. Workspace structure & code consistency

### B1. `packages/forge-dioxus` is excluded from the workspace — High
- `Cargo.toml:18-20`: `exclude = ["packages/forge-dioxus"]`. This means workspace `cargo build`, `cargo clippy`, `cargo test`, and `cargo fmt --all` skip this crate. Its lints, MSRV, and dep alignment drift silently.
- It also re-declares `serde = "1.0"`, `serde_json = "1.0"`, `reqwest = "0.12"` independently. The exclusion is presumably to keep wasm32 deps out of the host build, but the correct fix is one of:
  - Use `[target.'cfg(target_arch = "wasm32")']` gating (already done inside its `Cargo.toml`) and put it in the workspace.
  - Or keep it standalone and add a `clippy + fmt + cargo build --target wasm32-unknown-unknown` job in CI dedicated to it.
- Today neither is happening: it builds only when the dioxus example smoke test happens to patch `forge-dioxus = { path = ... }`.

### B2. Util fn duplication across crates — Medium
- `to_snake_case` exists at:
  - `crates/forge-core/src/util/mod.rs:77`
  - `crates/forge/src/cli/handler_scaffold.rs:203`
  - `crates/forge-macros/src/model.rs:146`
  - `crates/forge-macros/src/enum_type.rs:160`
- `to_camel_case` at `forge-core/src/util/mod.rs:93` and `forge-core/src/schema/function.rs:128`.
- `to_pascal_case` at `forge-core/src/util/mod.rs:64` and `forge-macros/src/utils.rs:9`.
- `parse_duration` at `forge-core/src/util/mod.rs:15`, `forge-core/src/rate_limit/mod.rs:131`, `forge-macros/src/utils.rs:23`.
- `forge-macros` cannot depend on `forge-core` (proc-macro crate, separate compile unit), so case helpers must be duplicated there. But the three copies inside `forge-macros` itself (`utils.rs`, `model.rs`, `enum_type.rs`) should all reach through `utils.rs`. And the `forge-core/src/rate_limit/mod.rs:131` copy of `parse_duration` should call `crate::util::parse_duration`.

### B3. Several modules tagged for splitting but never split — Medium
- `crates/forge/src/runtime.rs` is 1685 lines with a `// TODO(pre-1.0): Split into smaller modules` at line 1.
- `crates/forge/src/cli/check.rs` is 1645 lines, same TODO at line 1.
- `crates/forge-runtime/src/gateway/mcp.rs` is 1843 lines, same TODO at line 1.
- These are the hardest files to review and most likely to merge-conflict. The TODOs are not tracked anywhere — pre-1.0 policy says no permanent debt markers. Either split or delete the markers.

### B4. `BAD_CODE_0_7.md` deleted but still in `git status` — Trivial
- Status shows `D BAD_CODE_0_7.md`. Commit the deletion or restore it.

### B5. `crates/forge/generated/template-bundle.tar` is dead — Low
- `template-bundle.tar` (1.9 MB) is not referenced anywhere (`grep -rn template-bundle crates/ scripts/` returns nothing). Only `examples.tar` is consumed by `build.rs:26`. Delete the file and its publish-time generation in `scripts/build-template-archive.sh` if applicable.

### B6. `99` `#[allow(clippy::unwrap_used, clippy::indexing_slicing)]` escapes across `forge-core` — Medium
- `grep -rn "#\[allow(clippy::unwrap_used" crates/ | wc -l` returns 99 (most in test modules, which is fine). But several are on non-test code: e.g. `forge-core/src/types/local_time.rs:48` uses `.expect("midnight is always valid")` and `forge-core/src/types/instant.rs:110`, `local_date.rs:100`, `upload.rs:129` carry blanket allows at module scope.
- Audit: each non-test allow should justify itself in a comment. Today most are bare attributes with no rationale, which violates the "every escape hatch needs a why" coding rule.

### B7. `.expect("workflow lock poisoned")` repeated 12+ times in `workflow/context.rs` — Low
- `forge-core/src/workflow/context.rs` calls `.expect("workflow lock poisoned")` on every read/write of `self.saved_state`, `self.step_states`, `self.completed_steps`. Each `RwLock` panic message is the same string. Wrap the locks in a helper (`fn states(&self) -> RwLockReadGuard<'_, _> { self.step_states.read().expect(LOCK_MSG) }`) and pull the message into one `const`. Same pattern in `forge-core/src/schema/registry.rs` (12 occurrences of `.expect("schema registry lock poisoned")`).

### B8. `realtime/listener.rs:279` and `cluster/metrics.rs` carry `#[allow(dead_code)]` — Medium
- Workspace lint denies `dead_code` (Cargo.toml:124). 6 `#[allow(dead_code)]` escapes in `cluster/metrics.rs` plus 1 in `realtime/listener.rs` mean those structs/fields are never read on the active code path. Either delete them or expose them through the API.

---

## C. Feature flags

### C1. `testcontainers` feature on `forgex` exists but no consumer enables it on the CLI — Low
- `crates/forge/Cargo.toml:141`: `testcontainers = ["forge-core/testcontainers", "forge-runtime/testcontainers"]`. Nothing actually compiles `forgex` with `testcontainers` (CI runs `cargo test -p forge-svelte-demo-template --features testcontainers` and `-p todo`). The CLI doesn't need to forward this; it's a path of feature unification that adds confusion without benefit.
- Fix: drop the `testcontainers` feature from `forgex`. Examples enable `forge-core/testcontainers` directly.

### C2. `forgex` default = `full` means the slim presets (`worker`, `api`, `minimal`) require `default-features = false` — Medium
- `crates/forge/Cargo.toml:96`: `default = ["full"]`. Anyone wanting a worker-only binary writes `forge = { version = "0.9", default-features = false, features = ["worker"] }`. This is fine but every example today is `forge = { workspace = true }` which picks up `full` — the slim presets are unreachable from templates and untested in CI.
- Fix: add at least one example or smoke test exercising `worker` or `api` presets so they don't bit-rot.

### C3. `geoip` adds a build-time network fetch with no offline fallback — Medium
- `crates/forge-runtime/Cargo.toml:67`: `db_ip = "0.3"` with `include-country-code-lite` triggers a download from db-ip.com at build time. `full` includes `geoip` by default — meaning `cargo build -p forgex` without network access fails. There's no `geoip-offline` or similar.
- Fix: make `geoip` opt-out of `full` (move to a separate `full-online`/`full-offline` split), or vendor the lite DB into the repo.

### C4. The `gateway` cfg gate on `signals` creates a parallel no-op module — Low
- `crates/forge-runtime/src/lib.rs:51-100`: real `signals` mod vs. inline stub `pub mod signals { ... emit_raw(_) {} }` mod. Today's stubs implement only `emit_raw`/`emit_view`/`bot::is_bot`/`visitor::compute_visitor_id`. Easy for the real and stub APIs to drift; only the type system catches it.
- Fix: extract the trait surface (`SignalsSink`) into `forge-core`, have both impls implement it, single source of truth for the API shape.

---

## D. CI / Release pipeline

### D1. No MSRV check — High
- Workspace declares `rust-version = "1.92"` (Cargo.toml:8). CI uses `dtolnay/rust-toolchain@stable` everywhere. If a `*` dep pulls in 1.93-only syntax, CI is green but downstream users on the declared MSRV break.
- Fix: add a job using `rust-toolchain@1.92` running `cargo check --workspace --all-features`. Cheap (<2 min cached).

### D2. `cargo test -p todo-dioxus --features testcontainers` is missing from `workspace-integration` — Medium
- `.github/workflows/ci.yml:111` runs `cargo test -p todo --features testcontainers` (the Svelte realtime template). The Dioxus realtime template (`todo-dioxus`) has its own integration tests but they never run.
- Fix: add `cargo test -p todo-dioxus --features testcontainers` next to the existing line.

### D3. PR CI skips 4 of 6 examples — Medium
- `pr-smoke` matrix (`.github/workflows/ci.yml:113-126`): only `with-svelte/demo` and `with-dioxus/demo`. The `minimal` and `realtime-todo-list` variants in both stacks are only exercised on `main` pushes (`integration` job). Means a PR can land that breaks `forge new with-svelte/minimal` and nobody notices until after merge.
- Fix: either run all 6 templates on PR (current `minimal` takes ~5min each so the matrix doubles wall time but still completes in <20min) or run a representative pair from each frontend stack.

### D4. Benchmarks (`benchmarks/app`) never run in CI — Medium
- The crate compiles via `cargo build --workspace` but no perf regression check, criterion baseline, or even a smoke `cargo run -p forge-bench --release` test exists. As a result the benchmark code rots and depends on an outdated `jsonwebtoken = "9"` (item A2).
- Fix: nightly benchmark workflow that runs against a fixed commit baseline and posts perf deltas, or at minimum a `cargo check -p forge-bench --release` on PR.

### D5. Release pipeline publishes `forge-dioxus` from outside the workspace — Low
- `.github/workflows/release.yml:217`: `cd packages/forge-dioxus && publish_crate --allow-dirty`. Since `forge-dioxus` is excluded from the workspace (B1), it has its own `Cargo.lock` (`packages/forge-dioxus/Cargo.lock`) that the validate/test jobs never touch. We can ship a published `forge-dioxus` that fails to build for users.
- Fix: include in workspace (B1) and use `cargo publish -p forge-dioxus` from workspace root.

### D6. NPM publish ships raw `.ts` — Medium
- `packages/forge-svelte/package.json` has `"main": "./index.ts"`, `"types": "./index.ts"`, `"files": ["*.ts", "*.svelte"]`. No build step. No `dist/`, no `.d.ts` emission, no `.js` output.
- Works for SvelteKit/Vite consumers that compile TS via Vite. Breaks for:
  - Plain node consumers (`require('@forge-rs/svelte')` cannot resolve `.ts`).
  - TypeScript projects with `moduleResolution: node` (looks for `.d.ts`).
  - Bun's loader handles it, but `bun publish` is not what we use — `npm publish` ships exactly what's there.
- Fix: add a minimal `tsup` or `svelte-package` build step. Emit `dist/index.js` + `dist/index.d.ts`, update `exports` map to point at `dist`, keep `.ts` as `"source"` for IDEs. The `publish-npm` job (`release.yml:235-248`) currently has no `npm run build` step.

### D7. `test-template.sh` swallows formatter errors — Low
- `scripts/ci/test-template.sh:51-53`: `cargo fmt 2>/dev/null || true` and `bunx prettier --write . 2>/dev/null || true`. If the scaffolded template emits ill-formed Rust or TS that rustfmt/prettier reject, we silently move on and only fail at clippy/test stage — making the root cause harder to find.
- Fix: drop the `|| true`. If formatting fails, the test should fail loud.

### D8. `test-template.sh` uses `sed -i.bak` (GNU sed syntax) — Low
- `scripts/ci/test-template.sh:35`: works on macOS by accident (BSD sed treats `-i.bak` differently). On Linux CI it works as intended; on a contributor's mac it leaves `.bak` files behind. Not the same `sed -i` semantics everywhere — use `perl -pi -e` or a Python one-liner for portability.

### D9. CI cache key `shared-key: ci` reused across jobs with different feature sets — Low
- `.github/workflows/ci.yml:45,68,108`: `validate` (all-features clippy), `guardrails` (no features), `workspace-integration` (testcontainers feature) all share the same cache key. The cache will be whichever job finished last writing it. Cache hit rate suffers because each job effectively invalidates the others.
- Fix: separate `shared-key` per feature profile, or accept it and use `save-if: github.ref == 'refs/heads/main'` consistently (currently only set on the read-side).

### D10. Release publishes crates in fixed order with hardcoded 30s sleeps — Low
- `.github/workflows/release.yml:222`: `for pkg in forge-macros forge-core forge-runtime forge-codegen; do sleep 30 && publish_crate -p $pkg --allow-dirty; done`. The 30s sleep is a workaround for crates.io's index propagation lag. Brittle — sometimes 30s isn't enough.
- Fix: poll the crates.io API for the published version (`curl -sf https://crates.io/api/v1/crates/$pkg/$VERSION`) instead of sleeping. Or use `cargo-release` which handles this natively.

---

## E. Docker / local dev

### E1. `docker-compose.yml` Postgres matches CI (good) — None
- Both `docker-compose.yml:3` and CI services use `postgres:18`. Aligned. No action needed.

### E2. `docker-compose.yml` has no Grafana/Loki/Tempo despite signals docs referencing a Grafana dashboard — Low
- CLAUDE.md mentions "Grafana dashboard over PostgreSQL datasource" for signals, but local dev offers no preconfigured Grafana. There is a `docker-otel-lgtm.yml` workflow (likely image builder), but no `docker-compose` profile to start it locally alongside the DB.
- Fix: add an `observability` profile to `docker-compose.yml` that starts otel-lgtm + Grafana with the signals dashboard pre-provisioned.

---

## F. Profile & build

### F1. `release-fast` profile defined but not used in any CI script — Low
- `Cargo.toml:163-167`: `[profile.release-fast]` with `lto = false, codegen-units = 16`. Smoke tests still use the default `release` profile (full LTO). Local benchmarks could use it.
- Fix: either use it in `template-smoke.yml` (cuts ~3min off the build by skipping LTO) or delete the profile to keep the file slim.

### F2. `strip = true` on release means panics in production lack symbols — Low
- Release profile strips everything (Cargo.toml:160). Combined with `lto = true, codegen-units = 1`, production panic traces are useless. The framework auto-installs `tracing-subscriber` but panic locations still need line tables.
- Fix: `strip = "debuginfo"` (keeps symbol names, drops DWARF), or pair with `split-debuginfo = "packed"` and ship the debuginfo separately for releases.

---

## Top 5 cleanups before GA

1. **Move every example and benchmark to `{ workspace = true }` deps (A1, A2)**. Today examples teach users the wrong pattern and the `jsonwebtoken 9 vs 10` mismatch is a tangible bug. One PR, mechanical, big consistency win.
2. **Decide the OTel story now (A3)**. Either rip the `opentelemetry-*` crate stack out for hand-rolled OTLP/HTTP, or bump to the current upstream. The 0.27 pin with a TODO is technical debt that compounds with every transitive update.
3. **Add NPM build step for `@forge-rs/svelte` (D6)**. Shipping raw `.ts` makes the package effectively SvelteKit-only. Pre-1.0 is when the published artifact shape is forgivable to change.
4. **Bring `forge-dioxus` into the workspace (B1) and add MSRV + worker/api preset smoke jobs (D1, C2)**. The crate is published from outside the workspace today, with no clippy/fmt/MSRV coverage. Fixing this also unlocks D5.
5. **Split the three 1500+ line files marked `// TODO(pre-1.0): Split`**: `crates/forge/src/runtime.rs`, `crates/forge/src/cli/check.rs`, `crates/forge-runtime/src/gateway/mcp.rs`. These are the highest-friction files in the repo for both review and merge conflicts. The split markers themselves violate the no-permanent-debt rule.

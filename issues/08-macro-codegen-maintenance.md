# Macro & Codegen Maintenance Audit

Scope: `crates/forge-macros/` (10 attribute macros, sql extractor, attrs, utils) and `crates/forge-codegen/` (syn parser, SchemaRegistry, BindingSet, TS + Dioxus emitters, `emit.rs` as single source of truth for type mapping).

The framework is pre-1.0 with a "zero tech debt" policy. The issues below are the ones most likely to ossify and become expensive after GA, ordered loosely by severity.

---

## 1. Two `FunctionKind` enums drift silently

**Severity:** High
**Files:** `crates/forge-core/src/function/traits.rs:74` (Query/Mutation/Webhook only) and `crates/forge-core/src/schema/function.rs:10` (Query/Mutation/Job/Cron/Workflow).

There are two distinct `FunctionKind` enums with overlapping but non-identical variant sets. Codegen uses the schema one; runtime auth/rate-limit paths use the traits one. Adding a new handler kind (e.g. `Daemon`, `McpTool`) requires remembering to update both, and the compiler will not flag the omission because each enum is only matched in its own crate.

**Long-term hurt:** every new handler kind silently picks up partial integration (registry but no codegen, or codegen but no auth scoping). The first symptom will be a missing route/binding discovered at runtime.

**Fix:** consolidate into one `FunctionKind` in `forge-core` shared by both modules, with `#[non_exhaustive]` and exhaustive `match` at every consumer so adding a variant is a compile-time obligation.

---

## 2. `type_to_rust_type` parses syn::Type by stringifying then string-matching

**Severity:** High
**File:** `crates/forge-codegen/src/parser.rs:437`

The codegen parser converts `syn::Type` to a string with `quote!{}.to_string()` and then matches substrings ("Option <", "Vec <", "HashMap <") to recover structure. This is whitespace-sensitive (note the embedded spaces from `quote`), breaks on path prefixes (`std::vec::Vec<T>`, `alloc::collections::HashMap<K,V>`), and silently misclassifies anything with a comment or unusual formatting as `Custom`.

**Long-term hurt:** users writing perfectly valid Rust types will get binding generation that silently labels them `Custom` and emits an opaque TS/Dioxus type. Failures are silent — there is no diagnostic when a structurally-valid `Vec` falls through.

**Fix:** walk the `syn::Type` AST structurally (`Type::Path` -> last segment -> `PathArguments::AngleBracketed`) the way the macros already do. Reject path prefixes explicitly with a real error.

---

## 3. TS and Dioxus emitter parity is unenforced

**Severity:** High
**File:** `crates/forge-codegen/src/emit.rs:411` (test only checks non-empty)

`emit.rs` is documented as "the single source of truth for type mapping" but there is no test that, for every `RustType` variant and every documented `Custom(...)` alias, both `ts_type()` and `dioxus_type()` return a non-fallback string. The existing test asserts non-empty output, which any `"unknown"` fallback satisfies.

**Long-term hurt:** when someone adds a new `RustType` variant (e.g. `Decimal`, `BigInt`) for the TS emitter, the Dioxus side stays on the `Custom` fallback and ships broken Rust client code until a user reports it.

**Fix:** add a property-style test that iterates every `RustType` variant (use `strum::EnumIter` on the enum) plus a curated list of `Custom` aliases (`HashMap<...>`, `Vec<...>`, `Page<...>`, `BTreeMap<...>`) and asserts neither emitter returns its fallback sentinel.

---

## 4. Mutation macro silently ignores SQL parse failure; query macro errors

**Severity:** High
**Files:** `crates/forge-macros/src/mutation.rs:413` vs `crates/forge-macros/src/query.rs:259`

`query.rs` errors at compile time if the SQL extractor fails on a string literal (so private queries cannot smuggle past scope checking). `mutation.rs` falls back to empty table dependencies and continues compiling.

**Long-term hurt:** a mutation whose SQL fails to parse generates a handler with no recorded table dependencies, which means reactivity will not invalidate any subscriber when it runs. The handler "works" but breaks live queries. This will be a recurring bug report.

**Fix:** make the mutation macro emit the same compile error path as `query.rs`. Mutations have a stronger reactivity contract than queries do, not weaker.

---

## 5. Cron macro emits `.expect()` in generated code

**Severity:** Medium
**File:** `crates/forge-macros/src/cron.rs:145`

The cron macro validates the schedule string at compile time, but the generated handler still emits `Schedule::from_str(...).expect("Invalid cron schedule")`. This is dead defensive code: if compile-time validation passed, the runtime check cannot fail unless the validation drifts. Workspace lint `clippy::unwrap_used` is denied; `.expect()` is allowed but every panic path is a footgun the policy ostensibly forbids.

**Long-term hurt:** the moment someone "refactors" the compile-time validation (e.g. swaps the `cron` crate version), the runtime `.expect()` masks the regression because the schedule is constructed at startup, not test time. It also sets a bad example for users reading expanded macro output.

**Fix:** generate the schedule as a `const`/`OnceCell` populated from parsed components, or thread the validated schedule structure through directly. No `.expect()` in macro output.

---

## 6. Integer type validation disagrees between macros and codegen

**Severity:** Medium
**Files:** `crates/forge-macros/src/utils.rs:152` (`is_primitive_arg_type` accepts `u32`/`u64`/`i8`/`u8` etc.) vs `crates/forge-codegen/src/parser.rs:105-119` (`unsupported_type_reason` rejects the same set).

A handler with `arg: u32` compiles fine (macros think it's primitive) but `forge generate` fails with "unsupported type". The user has to discover this by running codegen.

**Long-term hurt:** every new handler author hits this. The error is far from the cause — the macro accepted it, so users think the codegen is buggy rather than their type choice being out of bounds.

**Fix:** unify the supported-primitive list in one place (`forge-core` constants or a single helper) and have both macros and codegen consult it. Better: reject unsupported integers at macro expansion with a span pointing at the argument, not at `forge generate` time.

---

## 7. Three copies of `to_snake_case` plus a buggy `pluralize`

**Severity:** Medium
**Files:** `crates/forge-macros/src/model.rs:146-159`, `crates/forge-macros/src/enum_type.rs:160-173`, plus `forge_core::util` already exposes one for codegen.

The macro-side copies treat `HTTPRequest` as `h_t_t_p_request` (each capital is its own boundary). `model.rs:219` documents that `pluralize("quiz")` returns `"quizes"` (should be `"quizzes"`). These have shipped because no integration test covers acronym-heavy or irregular model names.

**Long-term hurt:** picking one canonical implementation later is a breaking change for any user whose table names were derived through the buggy path. Fix now, before users name tables `HTTPRequest`.

**Fix:** move both helpers into `forge-core::util`, delete the macro-side copies, and add unit tests for `HTTPRequest`, `XMLParser`, `quiz`, `bus`, `index`.

---

## 8. Workflow signature uses type-name strings

**Severity:** Medium
**File:** `crates/forge-macros/src/workflow.rs:308-368`

The blake3 signature derivation feeds the workflow's input/output type into the hash as `quote!(#ty).to_string()`. Renaming `OrderInput` to `PurchaseOrder` (with `pub use OrderInput as PurchaseOrder;` to keep callers working) changes the signature and marks every in-flight run as `BlockedSignatureMismatch`. Conversely, two structurally-different types with the same short name (different modules) hash identically.

**Long-term hurt:** signature is the durability contract. Type aliasing or renames silently break running workflows in production. The failure mode (`BlockedSignatureMismatch` shown on `/_api/ready`) does not tell the operator that a harmless rename caused it.

**Fix:** hash structural type info (field name + emitted RustType for each input/output field) by reusing the codegen parser, not the source-level type ident. The signature should be stable across renames and detect real shape changes.

---

## 9. `darling::Error::custom` drops spans

**Severity:** Medium
**File:** `crates/forge-macros/src/attrs.rs` throughout (`RequireRole`, `TablesList`, `IdempotentMeta` validation paths)

Validation failures in attribute parsing build their error via `darling::Error::custom(...)` without `.with_span(&meta.span)`, so the compiler points at the entire `#[query(...)]` attribute instead of the offending value. For `tables = ["users", 123]` you get a generic "expected string" without knowing which entry.

**Long-term hurt:** every new validation rule starts spanless because it copies the prevailing pattern. Error quality is the framework's interface for users learning the macros; bad spans persist for years.

**Fix:** thread spans through every `Error::custom` site (use `darling::Error::custom(msg).with_span(&meta)`). Add a clippy-style internal lint or grep gate in CI to catch new spanless errors.

---

## 10. `inventory::submit!` auto-registration has no opt-out

**Severity:** Medium
**Files:** every handler macro (e.g. `crates/forge-macros/src/query.rs`, `mutation.rs`, etc.) emits `inventory::submit!(...)` unconditionally.

Every `#[query]` etc. registers globally at static-init time. This means:
- A test crate that links the parent app's handlers cannot opt out of auto-registering them.
- Two binaries sharing a library both pick up every handler from that library, even if one binary is "admin tools" and should not expose user queries.
- `cargo test` runs `inventory::iter` over everything, including handlers from unrelated dev-only modules.

**Long-term hurt:** as Forge apps grow into workspaces with multiple binaries (worker, gateway, CLI tools), the lack of segmentation pushes users toward feature flags or splitting crates aggressively. Both add friction the framework should absorb.

**Fix:** add `#[query(register = false)]` (or a default-on `#[query]` with explicit `#[forge::manual_register]` shim) and document the multi-binary workspace pattern. Keep the default on — opt-out, not opt-in.

---

## 11. `ts_hashmap` uses `splitn(2, ',')` and breaks on nested generics

**Severity:** Low
**File:** `crates/forge-codegen/src/emit.rs:121`

The `Custom` fallback path for `HashMap<K, V>` splits the generics string on the first comma. `HashMap<String, Vec<i32>>` parses fine, but `HashMap<String, HashMap<String, i32>>` or anything with multiple top-level commas inside a generic argument (e.g. `HashMap<(K1, K2), V>`) parses wrong.

**Long-term hurt:** users will hit this with multi-level maps and report it as a Forge bug. It's a paper cut now; it's a paper cut forever unless the parsing becomes structural.

**Fix:** parse generics by bracket-balance counting, not naive split. Or, again, walk the `syn::Type` AST and stop carrying string representations through the emitter.

---

## 12. Generated handler paths hardcode `forge::forge_core::...`

**Severity:** Low
**Files:** every macro's expansion (e.g. `crates/forge-macros/src/query.rs`, `mutation.rs`, `workflow.rs`, ...)

Generated code references types via `forge::forge_core::function::FunctionInfo` (and similar). This assumes the user has `forgex` in their dependencies as `forge` and that `forge` re-exports `forge_core`. Renaming the public crate (under consideration pre-1.0) or letting users alias it (`forge = { package = "forgex", ... }`) is fine, but the user cannot rename the import name (`forge = ... ` is required).

**Long-term hurt:** users who already have a `forge` crate in their dep graph (common name) have no escape hatch. Locks the framework into a specific public crate name.

**Fix:** emit `::forgex::forge_core::...` (absolute path to the published crate), or a single `$crate`-like resolution macro that consults a `#[forge::renamed = "..."]` attribute. Decide once before GA.

---

## 13. Codegen does not pin syn/quote to workspace versions

**Severity:** Low
**File:** `crates/forge-codegen/Cargo.toml`

Declares `syn = "2.0"` and `quote = "1.0"` directly rather than `syn.workspace = true`. The macros crate uses workspace pins. A `cargo update -p syn` could move the two crates onto different minor versions, with subtly different `parse_str` / `to_tokens` behavior.

**Long-term hurt:** parser drift between macros and codegen produces wrong bindings for code that compiles, which is the worst class of bug — silent, hard to reproduce, and only visible at the frontend boundary.

**Fix:** workspace-pin syn and quote across all four crates.

---

## 14. `looks_like_sql` heuristic false-positives on docstrings

**Severity:** Low
**File:** `crates/forge-macros/src/sql_extractor.rs:32-64`

The extractor scans every string literal in the function body and decides it is SQL if it contains `SELECT|INSERT|UPDATE|DELETE` (case-insensitive) plus a `FROM` or `WHERE` or `VALUES`. A doc-string or log message like `tracing::info!("Will UPDATE users WHERE active")` is flagged as SQL and runs through sqlparser, which usually fails and (in mutations) silently drops dependencies for the whole function.

**Long-term hurt:** combined with issue #4, a single user-facing log line can suppress reactivity for an unrelated mutation in the same function. This is a near-invisible footgun.

**Fix:** require an explicit marker. Either restrict extraction to literals passed to `sqlx::query!`/`sqlx::query_as!`/`ctx.db.execute`/... (a small allowlist of call sites) or require a `// @forge:sql` line tag. Heuristic SQL detection in arbitrary literals is too eager.

---

## 15. `ContractExtractor` silently skips non-literal workflow keys

**Severity:** Low
**File:** `crates/forge-macros/src/workflow.rs:282-296`

Step keys passed via `const STEP: &str = "process";` or any non-literal expression are not picked up by the signature derivation. The workflow compiles, but its signature is wrong (missing keys), so durability guarantees silently weaken.

**Long-term hurt:** the moment users factor step names into constants for reuse (a normal refactor), durability silently breaks. They will not discover this until a deploy strands running workflows.

**Fix:** error at compile time when a step name is not a string literal, or document and enforce that step names must be literals. Either way, no silent skips.

---

## Top 3 fixes before GA

1. **Consolidate `FunctionKind` and dedupe string-utility helpers (#1, #7).** Two enums and three `to_snake_case` copies will absorb every new handler kind's update cost and silently disagree. Pick one of each, delete the rest, and lock with `#[non_exhaustive]` plus exhaustive matches. Cheap to do now, expensive after users depend on the buggy `pluralize`/`to_snake_case` output for table names.

2. **Replace string-matching type parsing with structural syn walking, and reconcile primitive support (#2, #6).** `type_to_rust_type` in codegen is whitespace-sensitive and disagrees with the macro-side `is_primitive_arg_type` about which integers are allowed. The combined effect is "your handler compiles but its bindings are wrong, and we won't tell you which side is at fault." Walk the AST in both places and share the supported-primitives list from a single constant in `forge-core`.

3. **Enforce TS/Dioxus emitter parity with a generative test (#3).** `emit.rs` is documented as the single source of truth but nothing prevents one emitter from regressing to its fallback for a new variant. A test that enumerates every `RustType` variant via `EnumIter` and asserts both emitters return non-sentinel strings closes the drift window permanently and costs ~30 lines.

# SQL Safety, Tenant Scoping, and Data Isolation — Audit

Scope: `crates/forge-macros/src/{query,mutation,sql_extractor}.rs` plus the runtime path
that turns `QueryContext.auth` into actual SQL filtering, and the job/workflow contexts
that share helpers with queries.

Headline finding: the compile-time scope check is **shallow lexical sugar, not an
isolation primitive**. It verifies that an identifier named `user_id` / `owner_id` /
`tenant_id` appears somewhere in a WHERE/JOIN-ON, but never that the parameter bound
to it is the authenticated principal. There is no Postgres RLS, no `SET LOCAL`
session var, no row-level enforcement anywhere. Tenant isolation is "the
developer wrote `WHERE user_id = $1` and also remembered to pass `ctx.user_id()?`
as `$1`." Below are the concrete ways that contract can break or be bypassed,
many of them silently, all under the macro's currently-claimed guarantees.

---

## 1. Scope check is satisfied by *mentioning* the column, not *binding it to the caller*  — CRITICAL

`crates/forge-macros/src/sql_extractor.rs:785-829` (`expr_has_scope`) returns true
as soon as it sees an `Expr::Identifier("user_id")` anywhere in a WHERE/ON tree,
in any position, with any operator, against any value. There is no flow from
`ctx.user_id()` into the bound parameter.

Exploit:
```rust
#[forge::query]
async fn list_admin_secrets(ctx: &QueryContext) -> Result<Vec<Secret>> {
    // Passes the scope check. Returns every row whose owner_id is the
    // hard-coded UUID. ctx.user_id() is never consulted.
    sqlx::query_as!(Secret,
        "SELECT * FROM secrets WHERE owner_id = '00000000-0000-0000-0000-000000000001'")
        .fetch_all(ctx.db()).await
}
```
Or even more trivially: `WHERE user_id IS NOT NULL`, `WHERE user_id = user_id`,
`WHERE owner_id = ANY($1)` where `$1` is an attacker-controlled arg.

Fix: the only correct enforcement is at the data layer (Postgres RLS + a
session GUC set from `ctx.user_id()` inside the executor before handing the
connection to user code), or a typed query builder that *only* exposes scoped
parameter slots. Until then, drop the security framing in the docs and call
this what it is — a linter, not a guarantee.

---

## 2. Helper-function indirection bypasses the scope check entirely — CRITICAL

`crates/forge-macros/src/query.rs:284-316` only runs the scope check when
`!table_dependencies.is_empty()`, and table extraction only walks SQL string
literals **in the handler's own body** (`SqlStringExtractor::visit_block(fn_block)`
at line 290).

Exploit:
```rust
async fn fetch_all_secrets(db: DbConn<'_>) -> Result<Vec<Secret>> {
    sqlx::query_as!(Secret, "SELECT * FROM secrets").fetch_all(db).await  // unscoped
}

#[forge::query]                                       // not unscoped, not public
async fn get_dashboard(ctx: &QueryContext) -> Result<Vec<Secret>> {
    fetch_all_secrets(ctx.db_conn()).await            // no SQL literal here
}                                                     // table_dependencies = [] → check skipped
```
This compiles cleanly. The `unscoped` opt-out the macro advertises is not
required; any one-level helper hides the SQL from the visitor.

Fix: at minimum, error when `table_dependencies` is empty *and* the function
body contains a call into anything that takes `DbConn` / `&PgPool` / `ForgeDb`.
A real fix needs RLS (see #1).

---

## 3. `tables("...")` override silently disables the scope check — HIGH

`crates/forge-macros/src/query.rs:287` skips scope-checking when
`has_explicit_tables`. A developer using `#[query(tables("secrets"))]` to
work around an unparseable query loses the scope enforcement with no
warning, no logged opt-out, no separate flag.

Exploit: any handler that hit a sqlparser corner case (jsonb path, custom
operator, exotic CTE) and was "fixed" by adding `tables(...)` now also has
its scope check silently removed.

Fix: scope check and table-extraction are orthogonal. Run the scope check
regardless of whether tables were declared explicitly, and require the
caller to add `unscoped` to *also* opt out of scoping.

---

## 4. JOIN-with-scope makes the *other* tables in the join unscoped — CRITICAL

`crates/forge-macros/src/sql_extractor.rs:718-733`, `select_is_scoped`: if
*any* JOIN-ON references a scope column, the entire SELECT is treated as
scoped, including tables that are not filtered by that join condition.

Exploit:
```sql
-- compiles, "scoped"; returns every row in secrets
SELECT s.*
FROM   secrets s
JOIN   users u ON u.user_id = $1
```
Or LEFT JOIN, which never restricts the left side:
```sql
SELECT s.* FROM secrets s LEFT JOIN users u ON u.user_id = $1
```
The scope predicate touches `users`; `secrets` is a Cartesian-style fan-out.

Fix: the scope predicate must reference a column on the table the SELECT
actually reads from (or a CTE/derived subquery that itself is scoped on
that table). Track which table the scope column resolves to instead of
treating "scope column appears in some JOIN ON" as global.

---

## 5. UNION with `unscoped` branch under `unscoped` query, but ALSO: outer scope on a CTE doesn't restrict the CTE body — HIGH

`crates/forge-macros/src/sql_extractor.rs:1159-1168` documents this as
intended behaviour (test `scope_check_cte_body_unscoped_outer_scoped_passes`):
```sql
WITH all_t AS (SELECT * FROM tasks) SELECT * FROM all_t WHERE user_id = $1
```
is accepted. This is fine for `tasks` because `all_t` propagates `user_id`,
but only if `tasks` happens to have a `user_id` column. If the outer WHERE
references `user_id` against a column inherited from a join in `all_t`,
the macro can't tell, and the CTE itself reads the full table. A
materialized CTE (`AS MATERIALIZED`) or one read multiple times leaks the
whole table through other code paths if the same CTE name is reused.

Lower-severity than #4 but worth tightening: require the WHERE to bind
against a column originating in the unscoped table, not merely "named the
same thing somewhere upstream."

---

## 6. `tenant_id` is treated as an unconditional scope column, but no runtime path enforces it — HIGH

`crates/forge-macros/src/sql_extractor.rs:603` lists `tenant_id` as a
scope column. `QueryContext::tenant_id()` (`crates/forge-core/src/function/context.rs:797`)
returns `Option<Uuid>` — `None` for tokens that don't carry the claim. A
handler that filters by `tenant_id = $1` and binds `$1 = ctx.tenant_id()`
will silently bind `NULL`, which in Postgres matches *no* rows under `=`,
but `WHERE tenant_id IS NOT DISTINCT FROM $1` would match every row with
NULL tenant. Worse: the macro doesn't require `$1` to be `ctx.tenant_id()`
at all (#1).

Fix: same as #1, plus a runtime guard that the AuthContext actually carries
a tenant claim before any query depending on `tenant_id` is dispatched.

---

## 7. Mutations are not scope-checked at all — CRITICAL

`crates/forge-macros/src/mutation.rs` has no equivalent of `sql_references_identity_scope`.
The `unscoped` attribute exists on `DarlingMutationAttrs` (line 40) but is
never read after parsing. A `#[forge::mutation]` can `DELETE FROM users` or
`UPDATE secrets SET ...` with no WHERE at all and the macro is silent.

Exploit:
```rust
#[forge::mutation]                                      // not public, not unscoped
async fn delete_my_account(ctx: &MutationContext) -> Result<()> {
    sqlx::query!("DELETE FROM users").execute(ctx.db()).await?;   // deletes everyone
    Ok(())
}
```
The query macro will reject the SELECT equivalent. Symmetric handlers
should have symmetric checks. Mutations are *the* place data leaks become
data destruction.

Fix: run the same `sql_references_identity_scope` over INSERT/UPDATE/DELETE
in the mutation expander; require `WHERE user_id = ...` on UPDATE/DELETE
unless `unscoped`. For INSERTs, require either an explicit `user_id` /
`owner_id` column in the column list or `unscoped`.

---

## 8. `JobDispatcher::dispatch<J>` (typed entry) drops the principal — CRITICAL

`crates/forge-runtime/src/jobs/dispatcher.rs:27-34` (and `dispatch_in`,
`dispatch_at`, `dispatch_idempotent`, `dispatch_with_priority`): the typed
dispatch API hardcodes `owner_subject = None`. Only the dynamic
`dispatch_by_name` path, used by `MutationContext::dispatch_job`
(`crates/forge-core/src/function/context.rs:1252,1257`), carries the
auth principal.

Any code path that uses the typed `JobDispatcher::dispatch::<MyJob>(args)`
helper, including direct calls from a daemon or another job, enqueues with
`enqueued_by = NULL`. Subsequent `cancel_job(id, caller)` checks (queue.rs:534)
treat a NULL owner as "anyone can cancel," and any audit trail based on
`enqueued_by` is wrong.

Fix: either remove the typed `dispatch<J>` shorthand, or make it take
`&dyn HandlerContext` so the principal is mandatory.

---

## 9. JobContext / WorkflowContext are constructed unauthenticated — CRITICAL

`crates/forge-runtime/src/jobs/executor.rs:116-125`: `JobContext::new(...)`
is called without `.with_auth(...)`. The dispatching auth was persisted as
`owner_subject` (a string!) on the job row, but the executor doesn't
restore it. Same in `crates/forge-runtime/src/workflow/executor.rs:163-183`.

Consequence: any helper shared between a query and a job that reads
`ctx.auth.user_id()` will return `None` inside the job, and either:
- error out (best case), or
- be written defensively to "fall back to no filter" — which is exactly
  the silent admin-mode leak the doc warns about.

`auth.subject()` is a string, not a UUID, and claims/tenant/roles are not
persisted at all, so even if the executor *did* restore auth there is no
faithful round-trip.

Fix: persist a structured principal snapshot (user_id UUID + tenant_id +
required role claims) on the job/workflow row, restore it into a real
`AuthContext` before invoking the handler, and surface `JobContext::actor()`
that returns `Result<Uuid>` — never `Option`.

---

## 10. Outbox-dispatched jobs leak the dispatching tenant via shared queue — HIGH

`MutationContext::dispatch_job` (`function/context.rs:1252`) writes the
principal subject. But the JobQueue is a single global table claimed by
any worker. A job dispatched by tenant A and a job dispatched by tenant B
sit in the same queue; nothing prevents the handler from operating on
either via shared helpers (#9). If a handler reads `auth.subject()` and
fails closed when missing, it's safe; if it falls back to a service
identity (very common in framework code), tenant A's data and tenant B's
data are mixed under one principal.

Fix: enforce per-worker tenant context: when a job is claimed, set a
PG session GUC `forge.principal_id` / `forge.tenant_id` from the row's
stored principal, and combine with RLS (#1).

---

## 11. SQL extractor only inspects `sqlx::query*` macros and bare literals — MEDIUM

`crates/forge-macros/src/sql_extractor.rs:101-173`: `Visit` only descends
into specific method/macro/path names matching `query|query_as|query_scalar|query_as_unchecked`,
plus *any* `ExprLit` whose value passes `looks_like_sql`. Two gaps:

- A helper named `db.execute_sql(...)`, `db.fetch_all_dyn(...)`, or any
  custom wrapper does not trigger the SQL visitor — the string literal
  is still caught by `visit_expr_lit` only if it stands alone, but a
  `format!`/`String::from(...).push_str(...)` builder hides it from
  `looks_like_sql` once any concatenation happens.
- `sqlx::raw_sql(...)`, `sqlx::query_with(...)` (positional args), and
  the deprecated `query_unchecked` are not in the allow-list.

Fix: treat *every* string literal whose static value looks like SQL as a
mandatory extraction target (already partially the case), but also widen
the visitor to recognise `raw_sql`, `query_with`, and any
`sqlx::query!` variant. For builder patterns, error out: if you build SQL
from strings at runtime in a non-`unscoped` query, the macro can't verify
anything, and it should refuse to compile.

---

## 12. `looks_like_sql` lets innocent strings poison the parser — LOW/MEDIUM

`crates/forge-macros/src/sql_extractor.rs:32-64`: any 10+ char string
literal starting with `SELECT/INSERT/UPDATE/DELETE/WITH` and containing
the matching paired keyword gets parsed. Strings like log messages
(`"SELECT operation FROM cache failed"`) hit the matcher, fail sqlparser,
and produce `ParseFailed`, which turns into a compile error for the user
or — worse — gets the developer to silence it with `tables(...)` (see #3).

Fix: anchor SQL detection to actual `sqlx::query*` invocation contexts.
Drop the standalone `visit_expr_lit` SQL-sniffing path entirely; the cost
(missing a literal you concatenate in by hand) is already a #11 hole.

---

## 13. `.sqlx/` cache files are trust-on-first-use — MEDIUM

`.sqlx/*.json` are generated by `cargo sqlx prepare` against a developer
database and checked into the repo. Anyone with PR write access can:
- doctor a `.sqlx/query-<hash>.json` so a query that fails offline
  validation appears to succeed,
- swap a `nullable: true` for `false`, causing the runtime to `unwrap` a
  `NULL` and panic (CLAUDE.md forbids `.unwrap()` but `query_as!` generated
  code can still produce surprising None handling).

The CI step "Regenerate .sqlx cache" in CLAUDE.md is an honour system; CI
does not re-run `cargo sqlx prepare` and diff. A malicious PR can land a
backdoored cache.

Fix: in CI, regenerate `.sqlx/` against a fresh Postgres and `git diff
--exit-code` on the result. Anyone bypassing `SQLX_OFFLINE` on their dev
machine then can't ship a hand-edited cache.

---

## 14. `is_scope_col` is name-only, not type-checked — LOW

`crates/forge-macros/src/sql_extractor.rs:850-852`: `is_scope_col` does a
case-insensitive string compare. A table that has a non-FK column named
`user_id` (e.g. an int "user count id" on a metrics table) passes the
scope check trivially. The macro can't know the column's role.

Fix: this is fundamentally what RLS solves. Until then, document that
scope checking is name-based and explicitly *not* a security boundary.

---

## 15. Macro accepts `WHERE user_id IN (subquery)` where subquery is unscoped — MEDIUM

`sql_extractor.rs:816-818`: `Expr::InSubquery` returns true if either the
LHS is a scope expr OR the subquery is itself scoped. The LHS being a
scope identifier short-circuits to `true` regardless of what the subquery
returns.

Exploit:
```sql
-- accepted as "scoped"; returns every row whose user_id appears in the
-- (unscoped) other_users table — i.e. all rows with any valid user_id
SELECT * FROM secrets WHERE user_id IN (SELECT user_id FROM other_users)
```
Fix: require *both* that the LHS resolves to a scope predicate *and* that
the subquery is scoped, or that the LHS is bound to a parameter (the only
form `WHERE user_id = $1` shape).

---

## Must-fix before GA

1, 2, 4, 7, 8, 9, 10 are blockers. The framework's pitch is "private queries
are scoped"; today that claim is enforceable for the precise shape
`WHERE user_id = $1` in a single-table SELECT and only when the developer
also remembers to bind `$1 = ctx.user_id()`. Everything beyond that is on
trust. Either ship Postgres RLS with a session-GUC principal binding
(real fix), or rewrite the marketing as "lint, not isolation." Don't ship
both the current security framing and the current implementation into 1.0.

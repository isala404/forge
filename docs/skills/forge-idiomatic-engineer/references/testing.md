# Testing Reference

Prioritise failure paths over happy paths. If you write one test, write the failure case.

All backend tests use `#[tokio::test]`, live inline in `#[cfg(test)] mod tests {}`. DB-touching tests are gated behind `#[cfg(all(test, feature = "testcontainers"))]` and the `testcontainers` feature flag.

## Backend Context Builders

Each handler type has a `Test*Context::builder()`. Common setters on every builder:

- `.as_user(Uuid)` — sets the principal.
- `.with_role("admin")` / `.with_claim(k, v)` / `.with_tenant(id)` — JWT claim shaping.
- `.with_pool(pool)` — attach an `IsolatedTestDb` pool.
- `.with_env("KEY", "value")` — honoured by `ctx.env_*()`.
- `.mock_http(pattern, handler)` / `.mock_http_json(pattern, value)` — intercepts `ctx.http()`.

```rust
// Query
let ctx = TestQueryContext::builder()
    .as_user(user_id)
    .with_role("admin")
    .with_pool(db.pool())
    .build();

// Mutation — dispatches jobs/workflows into an in-memory outbox you can assert against
let ctx = TestMutationContext::builder()
    .as_user(user_id)
    .with_pool(db.pool())
    .mock_http_json("https://api.example.com/send", json!({ "ok": true }))
    .build();

// Job — progress, cancellation, heartbeat
let ctx = TestJobContext::builder("job_name").with_pool(db.pool()).build();
```

`TestCronContext`, `TestWorkflowContext`, `TestDaemonContext`, `TestWebhookContext`, and `TestMcpToolContext` follow the same shape.

## `IsolatedTestDb`

Creates a fresh DB, applies the Forge system schema and your migrations, tears down on drop.

Signature: `IsolatedTestDb::setup(test_name, internal_sql, migrations_dir)`. Always pass `&forge::get_internal_sql()` as the second argument — it installs `forge_jobs`, `forge_workflow_runs`, `forge_subscriptions`, `forge_signals_events`. Passing `""` skips them, which only works if the test never dispatches jobs, starts workflows, or subscribes.

```rust
#[cfg(all(test, feature = "testcontainers"))]
mod tests {
    use forge::testing::IsolatedTestDb;
    use std::path::Path;

    #[tokio::test]
    async fn creates_user() {
        let db = IsolatedTestDb::setup(
            "creates_user",
            &forge::get_internal_sql(),
            Path::new("migrations"),
        ).await.unwrap();
        let ctx = TestMutationContext::builder().with_pool(db.pool()).build();
        let result = CreateUserMutation::execute(&ctx, Args { name: "Alice".into() }).await;
        assert_ok!(result);
    }
}
```

## Assertion Macros

| Macro | Check |
|---|---|
| `assert_ok!(result)` | `Ok(_)` |
| `assert_err!(result)` | `Err(_)` |
| `assert_err_variant!(result, ForgeError::NotFound(_))` | specific variant |
| `assert_job_dispatched!(ctx, "job_name")` | job queued during the mutation |
| `assert_workflow_started!(ctx, "workflow_name")` | workflow started during the mutation |
| `assert_http_called!(ctx, "https://api.example.com/*")` | HTTP call was made via `ctx.http()` |

## Failure Scenarios to Always Cover

| Scenario | Strategy |
|---|---|
| Auth boundary | `TestQueryContext::minimal()` — expect 401. |
| Role enforcement | Context built with a non-privileged role — expect 403. |
| Missing resource | Call with a random `Uuid` — expect `NotFound`. |
| Ownership | Create as User A, read as User B — expect `NotFound` (not `Forbidden`). |
| Concurrent delete | Create, delete directly in the DB, attempt update — expect `NotFound`. |
| Job idempotency | Dispatch twice with the same key — DB reflects one effect. |

```rust
#[tokio::test]
async fn returns_not_found_for_missing_item() {
    let db = IsolatedTestDb::setup("missing_item", &forge::get_internal_sql(), Path::new("migrations")).await.unwrap();
    let ctx = TestQueryContext::builder().as_user(Uuid::new_v4()).with_pool(db.pool()).build();
    let result = GetItemQuery::execute(&ctx, Args { id: Uuid::new_v4() }).await;
    assert_err_variant!(result, ForgeError::NotFound(_));
}
```

## Frontend E2E (Playwright)

Exercise the real browser; do not call RPC handlers directly from frontend tests.

| Scenario | Strategy |
|---|---|
| Session expiry | `localStorage.clear()` + reload → redirected to login. |
| Real-time sync | Mutate in tab A; tab B updates via SSE within ~15s. |
| Submission throttling | Rapid-click a button; assert only one RPC on the backend. |
| Loading / disabled states | Button is disabled while `mutation.loading` is true. |
| Offline | `context.setOffline(true)` → offline indicator shows. |
| SSE reconnection | Abort `/_api/events` → client reconnects + refreshes state. |

Use the `rpc`, `gotoReady`, `uniqueId`, `ACTION_TIMEOUT` fixtures from `tests/fixtures.ts`. Always `gotoReady()` before asserting reactive data — it waits for `/_api/subscribe` to return 200, the reliable signal that reactivity is wired.

```typescript
test('real-time sync', async ({ authedPage: page }) => {
  await gotoReady(page, '/items');
  await expect(page.getByTestId('item-count')).toHaveText('1', { timeout: ACTION_TIMEOUT });
});
```

## Pre-Merge Checklist

- [ ] 401 on missing auth, 403 on missing role.
- [ ] `NotFound` on missing entity (not `Forbidden`, not 500).
- [ ] Ownership prevents User B from reading User A's data.
- [ ] Jobs are idempotent and gracefully handle missing targets.
- [ ] UI shows loading + disables buttons during mutations.
- [ ] Live subscriptions survive a simulated SSE disconnect.
- [ ] User-facing error copy is actionable.

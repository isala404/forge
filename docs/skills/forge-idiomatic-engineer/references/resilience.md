# Resilience Patterns

Assume every flow will be interrupted: tokens expire, entities vanish, networks drop, replicas lag. Build graceful degradation into every handler.

## 1. Auth and Session

| Scenario | Backend | Frontend |
|---|---|---|
| User deleted mid-session | `fetch_optional` → `Unauthorized` if the principal no longer exists. | `auth.clearAuth()` → redirect to "account deleted" page. |
| Refresh token revoked | Return `Unauthorized` on `refresh_token`. | Catch → clear storage → redirect to login with a reason. |
| Roles changed mid-session | Re-check critical roles in DB — don't trust the JWT. | On 403, refresh auth state. |
| JWT expires mid-mutation | Validate at start, keep the operation transactional. | One automatic `auth.tryRefresh()` + retry on 401, then surface. |
| Multi-tab logout | — | `storage` event or `BroadcastChannel` to sync. |
| Long tab hibernation | — | On `visibilitychange`, try refresh; logout if unrecoverable. |

## 2. Database and Data Integrity

| Scenario | Backend | Frontend |
|---|---|---|
| Entity deleted during action | 404 with context (`format!("item {id}")`). | Remove from local state, toast the deletion. |
| Concurrent update | `version` column → 409 on mismatch. | Show diff or force refresh. |
| FK target deleted | Match `is_foreign_key_violation`, return `Validation` with context. | Inform which parent went missing. |
| Pool exhaustion | Raise `pool_size`, lower `statement_timeout`, run heavy workloads on dedicated worker nodes. | "System busy" toast. |
| Read replica lag | `#[query(consistent)]` after a write. | Prefer the mutation's response body. |

## 3. SSE and Realtime

- Subscriptions re-register automatically on reconnect. Don't cache reactive data on the client.
- The Reactor re-evaluates permissions with the current auth state — stale data isn't pushed.
- Treat each push as the full current state, not a delta.
- Reconnect on `visibilitychange` to catch up after hibernation.

## 4. Jobs and Workflows

- Always verify the target entity still exists at the start of each job / step. Exit gracefully if missing.
- `idempotent(key = "...")` on every dispatchable job.
- Re-verify business preconditions at each workflow step — long runs outlive their invariants.
- Every `wait_for_event` needs a timeout or it stalls forever.
- Dispatch only from mutations (transactions are on by default). See [Patterns](./patterns.md#background-jobs).

## 5. Client Resilience

- `beforeNavigate` guard on in-flight mutations.
- Disable action buttons while `mutation.loading` is true.
- Wrap `localStorage` in try/catch and fall back to `sessionStorage` or in-memory.
- Validate file size + MIME client-side before uploading.

## Checklist

- [ ] Survives revoked auth mid-execution.
- [ ] Checks entity existence before acting.
- [ ] Handles concurrent modification (version / etag).
- [ ] UI handles network drops.
- [ ] Double-click prevention on all buttons.
- [ ] Frontend survives hibernation and token expiry.
- [ ] Jobs are idempotent across retries.
- [ ] Clear, actionable error feedback for every failure mode.
- [ ] Subscriptions re-established after reconnect.

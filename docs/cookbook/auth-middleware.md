# Auth middleware: validating sessions and API keys

Forge owns the credential, your app owns the identity. A `Session` or `ApiKeyInfo` only ever hands back an opaque `user_id`/`owner_id` string that *you* supplied at creation time — Forge has no users table, no identity model, no RBAC. So an auth "middleware" in Forge is just two calls: `auth().validate_session(token)` for browser sessions (sliding idle timeout), falling back to `auth().verify_api_key(token)` for programmatic clients. Both treat a bad credential as a normal negative (`Ok(None)`), never an error, so the resolver below is total: a token either maps to a principal or it doesn't.

## The pattern

This is the chatapp `rust-be` shape: a `CurrentUser` principal built once at the HTTP edge, with a single `principal()` resolver that tries a session first, then an API key. It's wired into an axum handler, but the resolver itself is framework-agnostic.

```rust
use axum::http::{header, HeaderMap};
use forge::Forge;
use uuid::Uuid;

/// The authenticated principal for a request. `user_id`/`owner_id` are opaque
/// app strings as far as Forge is concerned; here we know they're UUIDs.
#[derive(Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    /// Raw session token, kept so `logout` can revoke exactly this session.
    /// Empty when the principal authenticated with an API key.
    pub token: String,
}

fn strip_bearer(raw: &str) -> &str {
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim()
}

/// Resolve a bearer token to a principal: a live session (which slides the idle
/// deadline as a side effect) or an API key. `None` means "no valid credential",
/// which is a normal outcome, not an error.
async fn principal(forge: &Forge, token: &str) -> Option<CurrentUser> {
    // Sessions first. validate_session returns Ok(None) for unknown / expired /
    // revoked tokens — never an Err — and on success slides the idle timeout.
    if let Ok(Some(session)) = forge.auth().validate_session(token).await
        && let Ok(uid) = Uuid::parse_str(&session.user_id)
    {
        return Some(CurrentUser { id: uid, token: token.to_string() });
    }
    // Fall back to API keys (fk_-prefixed). Same Ok(None)-on-miss contract.
    if let Ok(Some(info)) = forge.auth().verify_api_key(token).await
        && let Ok(uid) = Uuid::parse_str(&info.owner_id)
    {
        return Some(CurrentUser { id: uid, token: String::new() });
    }
    None
}

/// Pull the bearer from the Authorization header and resolve it. Absent or
/// malformed header => anonymous (None), not a rejection.
pub async fn user_from_bearer(forge: &Forge, headers: &HeaderMap) -> Option<CurrentUser> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    principal(forge, strip_bearer(raw)).await
}
```

Minting the credentials these resolve against — the other side of the boundary. The `user_id`/`owner_id` you pass in is whatever your app calls a user; Forge stores only a hash of the token/key:

```rust
use forge::{SessionOpts, Forge};
use std::time::Duration;

// On successful login: mint a session token. Hand `token.as_str()` to the
// client exactly once; only its SHA-256 is stored.
let token = forge.auth()
    .create_session(
        &user_id.to_string(),
        SessionOpts::new()
            .with_idle_timeout(Duration::from_secs(30 * 60))
            .with_absolute_timeout(Duration::from_secs(7 * 24 * 60 * 60)),
    )
    .await?;

// For programmatic access: mint an fk_ key. key.secret.as_str() is shown ONCE.
let key = forge.auth().create_api_key(&user_id.to_string(), "ci-bot").await?;

// Logout revokes exactly this session (idempotent — Ok(()) even if already gone).
forge.auth().revoke_session(&token.as_str().to_string()).await?;
```

Once `CurrentUser` is in your request context (e.g. `request.data(user)` in async-graphql), the "require auth" check is presence, not re-validation:

```rust
// async-graphql resolver: pull the principal that the edge already resolved.
fn me(ctx: &Context<'_>) -> Result<CurrentUser> {
    ctx.data_opt::<CurrentUser>()
        .cloned()
        .ok_or_else(|| err("UNAUTHENTICATED", "not authenticated"))
}
```

## Notes and gotchas

- **The contract boundary.** `validate_session`/`verify_api_key` give you an opaque `user_id`/`owner_id` and nothing more. `verify_*` returns *identity, not authorization* — there's no RBAC, scopes, or org model in Forge. Membership and permission checks (e.g. "is this user in this chat") are your app's SQL, against your own tables.
- **Miss is `Ok(None)`, not `Err`.** Unknown, expired, or revoked credentials resolve to `Ok(None)`. This surface never produces `NotFound` or `Precondition`. An `Err` from these calls means a real backend problem (`Unavailable`, `Backend`), so don't collapse `Err` into "unauthenticated" — the chatapp uses `if let Ok(Some(...))` precisely to treat only a live credential as success while letting transient errors fall through to `None` (or, if you prefer, surface them).
- **Session validation has a side effect.** A successful `validate_session` *slides* the idle deadline to `now + idle_timeout` (capped at the absolute deadline). Calling it per request is how "active users stay logged in" works; the absolute timeout from `created_at` is never extended.
- **Order matters only for cost, not correctness.** Sessions are tried first because they're the common browser path. API keys are `fk_`-prefixed, so if you wanted to skip the session lookup for obvious keys you could branch on `token.starts_with("fk_")` — the chatapp doesn't bother, since both lookups are an indexed hash probe.
- **Keep the raw token only if you need to revoke it.** `CurrentUser.token` holds the raw session token so a logout can call `revoke_session` on exactly that session. API-key principals leave it empty — keys are revoked by `id` via `revoke_api_key`, not by the secret. Don't log either; the secret newtypes (`SessionToken`, `ApiKeySecret`) have redacted `Debug` for this reason.
- **No bearer header is anonymous, not forbidden.** `user_from_bearer` returns `None` for a missing/garbled header. Decide per route whether `None` means 401 or "anonymous access allowed" — the resolver stays neutral. (In the chatapp's WS handshake, an *absent* token is anonymous but a *provided* token that fails to resolve is rejected, rather than silently downgraded.)

### Node / Python bindings

Same two-step shape. The methods are `auth.validateSession(token)` / `auth.verifyApiKey(token)` in Node and `auth.validate_session(token)` / `auth.verify_api_key(token)` in Python; both return the session/key info or a null/`None` on miss rather than throwing. Rust is canonical for exact field names (`user_id`, `owner_id`); the bindings expose the same `Session` and `ApiKeyInfo` fields.

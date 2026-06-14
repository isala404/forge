# Session cookie setup

Forge's `auth` surface mints opaque session tokens, but it has no opinion about transport. You decide how the token reaches the browser. For a server-rendered or same-site app the right answer is almost always a cookie: `create_session` at login, hand the returned token to the client as a `Secure; HttpOnly; SameSite` cookie, `validate_session` on every request (which slides the idle deadline), and `revoke_session` on logout to clear it. The token is the secret — it exists once, in the response that sets the cookie. Only its SHA-256 is ever stored.

## Example

This uses [axum](https://docs.rs/axum) and the [`cookie`](https://docs.rs/cookie) crate to show the full round trip. The Forge calls are the load-bearing part; the cookie plumbing is ordinary HTTP.

```rust
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use cookie::{Cookie, SameSite};
use forge::{Forge, SessionOpts};
use serde::Deserialize;

const COOKIE_NAME: &str = "fsession";
const SESSION_IDLE: Duration = Duration::from_secs(30 * 60); // 30 min sliding
const SESSION_ABSOLUTE: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 day ceiling

#[derive(Clone)]
struct AppState {
    forge: Arc<Forge>,
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

/// POST /login — verify the password (your users table), mint a session, set the cookie.
async fn login(
    State(st): State<AppState>,
    Json(body): Json<LoginBody>,
) -> Result<Response, StatusCode> {
    // Your app owns the users table. Look up the stored PHC hash and the user id.
    let (user_id, phc) = lookup_user(&body.username)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // verify_password is constant-time; a mismatch is Ok(false), not an error.
    let ok = st
        .forge
        .auth()
        .verify_password(&body.password, &phc)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !ok {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Mint the session. Both timeouts are always applied: idle slides on each
    // validate, absolute is a hard ceiling from creation that is never extended.
    let token = st
        .forge
        .auth()
        .create_session(
            &user_id,
            SessionOpts::new()
                .with_idle_timeout(SESSION_IDLE)
                .with_absolute_timeout(SESSION_ABSOLUTE),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // The token plaintext exists exactly once — here. Put it in the cookie.
    let cookie = Cookie::build((COOKIE_NAME, token.as_str().to_owned()))
        .http_only(true) // not readable from JS; mitigates XSS token theft
        .secure(true) // HTTPS only (drop in local dev over plain HTTP)
        .same_site(SameSite::Strict) // CSRF defense; Lax if you need top-level nav
        .path("/")
        .max_age(cookie::time::Duration::seconds(SESSION_ABSOLUTE.as_secs() as i64))
        .build();

    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, cookie.to_string())],
    )
        .into_response())
}

/// Pull the session token out of the Cookie header.
fn session_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    Cookie::split_parse(raw)
        .filter_map(Result::ok)
        .find(|c| c.name() == COOKIE_NAME)
        .map(|c| c.value().to_owned())
}

/// Validate on each request. validate_session slides the idle deadline on success.
/// Unknown, expired, or revoked tokens come back as Ok(None) — never an error.
async fn require_user(st: &AppState, headers: &HeaderMap) -> Result<String, StatusCode> {
    let token = session_token(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    match st.forge.auth().validate_session(&token).await {
        Ok(Some(session)) => Ok(session.user_id),
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// POST /logout — revoke the session and clear the cookie. Revoke is idempotent,
/// so a stale or already-gone token is fine.
async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    if let Some(token) = session_token(&headers) {
        st.forge
            .auth()
            .revoke_session(&token)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    // Expire the cookie by setting Max-Age=0 with the same attributes.
    let cleared = Cookie::build((COOKIE_NAME, ""))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(cookie::time::Duration::ZERO)
        .build();
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, cleared.to_string())],
    )
        .into_response())
}

pub fn router(forge: Arc<Forge>) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .with_state(AppState { forge })
}

// Your application owns identity. Forge never reads or writes the users table.
async fn lookup_user(_username: &str) -> Option<(String, forge::PhcString)> {
    unimplemented!("query your users table for (user_id, stored PHC hash)")
}
```

## Notes and contract guarantees

- **`SessionToken` is write-once.** `create_session` returns the plaintext exactly once; the server stores only its SHA-256. `SessionToken` has a redacted `Debug`, so it won't leak into logs — call `.as_str()` only to write it into the cookie. There is no API to read a token back later.
- **Both timeouts always apply.** `idle_timeout` is sliding: every successful `validate_session` refreshes it to `now + idle_timeout`, capped at the absolute deadline. `absolute_timeout` is a hard ceiling measured from `created_at` and is never extended. A session is live only while `now` is before both. Defaults (via `SessionOpts::new()` / `default()`) are 30 min idle, 12 h absolute. The contract requires `1s <= idle_timeout <= absolute_timeout` and `absolute_timeout` up to a ~10-year ceiling; out-of-range values are `Invalid` (and over the absolute ceiling, `Limit`). Deadlines are second-precision; sub-second inputs round up.
- **Validation failure is `Ok(None)`, not an error.** Unknown, expired, or revoked tokens return `Ok(None)`. Reserve the `Err` arm for genuine backend trouble (`Unavailable`, `Backend`). Don't treat a missing session as a 500.
- **Revoke is idempotent.** `revoke_session` on an unknown, already-revoked, or expired token returns `Ok(())`. Always pair it with clearing the cookie (`Max-Age=0`, same `Path`/`Domain`/attributes) so the browser stops sending a dead token. To log out every device, use `revoke_all_sessions(user_id)`, which returns the count removed.
- **Cookie attributes are yours, not Forge's.** Forge mints the token; it has no view of HTTP. `HttpOnly` keeps the token out of JavaScript, `Secure` keeps it off plaintext connections (turn it off only for local HTTP dev), and `SameSite=Strict`/`Lax` is your CSRF posture. Use `Lax` if a cross-site top-level navigation must arrive authenticated; `Strict` otherwise. None of this substitutes for the app's own CSRF strategy on state-changing requests.
- **Cookie lifetime vs. session lifetime are independent.** The cookie `Max-Age` is a client-side hint only; the server is the source of truth. Setting `Max-Age` to the absolute timeout is a reasonable default, but expect the server to reject a cookie whose session has idled out well before the cookie itself expires — that's the idle timeout doing its job.
- **Forge owns the users table boundary.** `user_id` is an opaque app string; `create_session` neither validates nor stores anything about it beyond the session row. Password verification, lookup, and `needs_rehash`-based rehashing all happen against your own table.

### Node / Python bindings

The bindings flatten this surface. `createSession` / `create_session` take optional `idleSeconds` and `absoluteSeconds` numbers instead of a `SessionOpts` struct (omit them for the defaults). More importantly, `validateSession` / `validate_session` return **only the user id** (`string | null` in Node, `str | None` in Python), not a full `Session` object — there's no `created_at` / `expires_at` exposed. `revokeSession` / `revoke_session` are unchanged. The cookie wiring itself lives in your web framework (Express, FastAPI, etc.), exactly as it does in Rust.

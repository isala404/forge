# auth — lineage: OWASP cheat sheets + PHC + PHP `password_*` + Stripe/GitHub keys

Password hashing, sessions, and API keys — **primitives, not a product.**

## Lineage

Three independent surfaces, each mirroring the battle-tested standard for its job:

- **Passwords** follow the **OWASP Password Storage cheat sheet**: argon2id at OWASP-current
  parameters, hashes stored as **PHC strings** (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`). The
  method shape (`hash` / `verify` / `needs_rehash`) is lifted from PHP's `password_hash` /
  `password_verify` / `password_needs_rehash` — the API everyone already knows.
- **Sessions** follow the **OWASP Session Management cheat sheet**: opaque, `>= 256-bit` random
  tokens, **stored hashed** (SHA-256), with both an **idle (sliding) timeout** and an **absolute
  timeout**, named verbatim.
- **API keys** follow the **Stripe / GitHub** prefixed-key convention: an `fk_`-prefixed token, the
  full secret shown **exactly once** at creation, SHA-256 at rest.

Forge does **not** own the `users` table. `user_id` / `owner_id` are opaque strings the application
supplies; auth never reads, writes, or validates them. The default backend is Postgres; the contract
is the lowest common denominator that Postgres and the lineage standards can both honor.

> **Only hashes are ever stored or logged.** Plaintext passwords, raw session tokens, and raw API
> keys exist for one call and are never persisted, never emitted in spans, never in error messages.
> The secret newtypes (`PhcString`, `SessionToken`, `ApiKey::secret`) have **redacted `Debug`**.

## Trait (this doc is normative for semantics; the shipped trait signatures are normative for shape)

```rust
#[async_trait]
pub trait Auth: Send + Sync {
    // ---- passwords (argon2id; PHC strings; shape mirrors PHP password_*) ----
    async fn hash_password(&self, plain: &str) -> Result<PhcString>;
    async fn verify_password(&self, plain: &str, hash: &PhcString) -> Result<bool>; // constant-time
    fn needs_rehash(&self, hash: &PhcString) -> bool; // true if params < Forge-current

    // ---- sessions (opaque >=256-bit; stored SHA-256; idle + absolute timeouts) ----
    async fn create_session(&self, user_id: &str, opts: SessionOpts) -> Result<SessionToken>;
    async fn validate_session(&self, token: &str) -> Result<Option<Session>>; // slides idle expiry
    async fn revoke_session(&self, token: &str) -> Result<()>;                 // idempotent
    async fn revoke_all_sessions(&self, user_id: &str) -> Result<u64>;         // count revoked

    // ---- API keys (Stripe/GitHub fk_ prefix; secret shown once; SHA-256 at rest) ----
    async fn create_api_key(&self, owner_id: &str, label: &str) -> Result<ApiKey>;
    async fn verify_api_key(&self, key: &str) -> Result<Option<ApiKeyInfo>>;   // constant-time
    async fn revoke_api_key(&self, key_id: &str) -> Result<bool>;              // false if unknown
}

#[non_exhaustive]
pub struct SessionOpts {
    /// OWASP idle timeout. Sliding: refreshed on each successful validate. Default 30 min.
    pub idle_timeout: Duration,
    /// OWASP absolute timeout. Hard ceiling from creation; never extended. Default 12 h.
    pub absolute_timeout: Duration,
}

#[non_exhaustive]
pub struct Session {
    pub user_id: String,
    pub created_at: SystemTime,     // TIMESTAMPTZ, seconds precision
    pub expires_at: SystemTime,     // min(now + idle, created_at + absolute) at validate time
}

/// PHC string ($argon2id$v=19$...). Portable to/from the password_hash ecosystem.
/// Debug is REDACTED.
pub struct PhcString(/* opaque; redacted Debug */);

/// Opaque session token (>=256-bit random). Plaintext exists once; only its SHA-256
/// is stored. Debug is REDACTED.
pub struct SessionToken(/* opaque; redacted Debug */);

#[non_exhaustive]
pub struct ApiKey {
    pub id: String,                 // stable id (safe to store/log)
    pub label: String,
    pub secret: ApiKeySecret,       // `fk_...` shown ONCE; redacted Debug
    pub created_at: SystemTime,
}

#[non_exhaustive]
pub struct ApiKeyInfo { pub id: String, pub owner_id: String, pub label: String }
```

`user_id` and `owner_id` are opaque, app-owned. `id` / `key_id` are non-secret identifiers (safe to
persist and log). The raw `token` / `key` strings passed to `validate_session` / `verify_api_key` are
the secrets — never stored, never logged.

## Semantics

| op | behavior |
|----|----------|
| `hash_password` | Generates a fresh random salt, hashes with argon2id at **Forge-owned current params**, returns the PHC string. Never reuses a salt. |
| `verify_password` | **Constant-time** compare of `plain` against `hash`. `Ok(true)` on match, `Ok(false)` on mismatch. A malformed/unparseable PHC `hash` is a caller bug → `Invalid`, never `Ok(false)`. |
| `needs_rehash` | Pure, synchronous. `true` iff `hash`'s algorithm/params are below Forge-current — call after a successful `verify_password` and transparently re-hash + re-store. `false` for an up-to-date hash. A malformed/unparseable `hash` returns `true` (it cannot be trusted, so rehash it) — `needs_rehash` returns `bool` and never errors. |
| `create_session` | Mints a `>= 256-bit` random token, stores only its **SHA-256** with `created_at`, idle deadline (`now + idle_timeout`), and absolute deadline (`created_at + absolute_timeout`). Returns the plaintext `SessionToken` — shown **once**. |
| `validate_session` | Constant-time lookup by token hash. If present, live, and within **both** timeouts: **slides** the idle deadline to `now + idle_timeout` (capped at the absolute deadline) and returns `Some(Session)`. Unknown, expired (idle or absolute), or revoked → `Ok(None)`, never an error. |
| `revoke_session` | Removes the session by token hash. **Idempotent**: revoking an unknown/already-revoked/expired token is `Ok(())`. |
| `revoke_all_sessions` | Removes every live session for `user_id`. Returns the count revoked (`0` if none). |
| `create_api_key` | Mints an `fk_`-prefixed random secret (`>= 256-bit` entropy), stores only its **SHA-256** plus `id`, `owner_id`, `label`, `created_at`. Returns `ApiKey` with the full secret — shown **exactly once**. |
| `verify_api_key` | Constant-time lookup by key hash. `Some(ApiKeyInfo)` if present and not revoked; unknown or revoked → `Ok(None)`, never an error. |
| `revoke_api_key` | Revokes by `key_id`. `Ok(true)` if a key was revoked, `Ok(false)` if no such id (or already revoked) — not an error. |

## Security guarantees

- **Opaque, hashed tokens.** Session tokens and API keys are high-entropy random strings, never
  JWTs (see Deviations). The server stores only a SHA-256 digest; a database leak does not yield
  usable credentials, and any token can be revoked instantly.
- **Constant-time everywhere it matters.** `verify_password` uses argon2id's built-in constant-time
  verification. `validate_session` and `verify_api_key` never compare a raw secret in application
  code: they SHA-256 the presented token/key and look it up by that (indexed) digest, so request
  timing does not reveal the stored secret or whether the principal exists. A leaked database holds
  only digests; there is nothing to time-probe.
- **Argon2id parameters are Forge-owned.** The application does not choose cost parameters. Forge
  tracks the OWASP-current baseline; `needs_rehash` + re-hash on next successful login upgrades old
  hashes transparently, with no migration and no forced password reset.
- **PHC portability.** Because hashes are PHC strings, they import from / export to any
  `password_hash`-compatible system (PHP, libsodium, Python `argon2-cffi`, etc.). Forge can verify a
  hash another tool produced (and flag it via `needs_rehash`).
- **Secrets are write-once in the API.** The only time a raw token/key is observable is the return
  value of its `create_*` call. There is no "reveal" or "list secret" path.

## Timeouts / expiry

- **Two session deadlines, both explicit (OWASP terms).** The **idle timeout** is *sliding* —
  refreshed to `now + idle_timeout` on each successful `validate_session`. The **absolute timeout**
  is a *hard ceiling* from `created_at` and is **never** extended; once it passes, the session is
  dead even if it was active a moment ago. A session is live iff `now < idle_deadline` **and**
  `now < absolute_deadline`.
- **`Session.expires_at`** is the effective deadline at validate time:
  `min(now + idle_timeout, created_at + absolute_timeout)`.
- **Precision: seconds.** All deadlines are `TIMESTAMPTZ` at seconds precision; sub-second timeout
  inputs round up to the next whole second and a positive timeout never rounds to 0.
- **Lazy + background sweep.** Expiry is enforced on every read (`validate_session` treats an expired
  session as absent → `Ok(None)`) and reclaimed by a background sweep. A `validate_session` after
  expiry returns `None`, **guaranteed**, regardless of sweep timing.
- **API keys do not expire.** They live until `revoke_api_key`. (Key rotation = create new, revoke
  old; expiry windows are post-v1.)

## Limits

| thing | limit | over-limit error |
|-------|-------|------------------|
| password plaintext | `>= 1` and `<= 4096` bytes (argon2 input cap; blocks DoS via huge inputs) | empty → `Invalid`; over → `Limit` |
| PHC hash string | `<= 1 KiB` | `Invalid` if unparseable; `Limit` if over |
| session/API-key token entropy | `>= 256-bit` random (Forge-fixed) | n/a (server-generated) |
| `user_id` / `owner_id` | non-empty, `<= 255` bytes UTF-8 | empty → `Invalid`; over → `Limit` |
| `label` | `<= 255` bytes UTF-8 (may be empty) | over → `Limit` |
| `idle_timeout` | `>= 1s`, `<= absolute_timeout` | `Invalid` if zero/negative or `> absolute_timeout` |
| `absolute_timeout` | `>= 1s`, `<=` ~10-year ceiling | `Invalid` if zero/negative; `Limit` if over the ceiling |

Token/key byte lengths are server-controlled and not a caller limit. The argon2 cost parameters
(memory, time, parallelism) are Forge constants, not caller-tunable.

## Error mapping

| condition | variant | retryable |
|-----------|---------|-----------|
| unknown / expired / revoked session on `validate_session` | `Ok(None)`, not an error | — |
| unknown / revoked API key on `verify_api_key` | `Ok(None)`, not an error | — |
| `verify_password` mismatch | `Ok(false)`, not an error | — |
| `revoke_session` on unknown/already-revoked token | `Ok(())`, idempotent | — |
| `revoke_api_key` on unknown id | `Ok(false)`, not an error | — |
| malformed / unparseable PHC `hash` passed to `verify_password` | `Invalid` | no |
| empty password, empty `user_id`/`owner_id`, oversized label/id, out-of-range timeout | `Invalid` / `Limit` (per Limits) | no |
| argon2 hashing/verification failure (allocator, internal error) | `Backend` (carries retryability flag) | per flag |
| transient backend outage (pool timeout, dropped conn, `08xxx`/`57014`) | `Unavailable` | yes |
| other vendor/SDK error | `Backend` (carries retryability flag) | per flag |
| misconfiguration (bad DSN, missing migration) at `Forge::init()` | `Config` | no — init only |

`NotFound` and `Precondition` are **never** produced by this surface. Absence on every read/verify
path is `None` / `false`, matching the lineage (validating a bad token is a normal negative result,
not an exception). Revokes are idempotent (`revoke_session` → `Ok(())`; `revoke_api_key` → `Ok(false)`),
so they never raise `NotFound`. `Config` is init-time only — a missing migration fails inside
`Forge::init()`, never lazily on first call. Error messages never contain passwords, raw tokens, raw
keys, salts, or hash material.

## Deviations from lineage

- **Opaque hashed tokens, no JWT (v1).** The "JWT everywhere" fashion trades server state for
  statelessness and loses instant revocation. Forge stores a hashed random token instead:
  revocation is a row delete, there is no signing-key rotation problem, and a leaked database yields
  no usable credentials. Stateless/JWT sessions are explicitly out of scope.
- **SHA-256 (not argon2) for session tokens and API keys at rest.** argon2 is for *low-entropy*
  human passwords; a `>= 256-bit` random token has nothing to brute-force, so a fast cryptographic
  hash is sufficient and keeps `validate`/`verify` cheap. Passwords still use argon2id.
- **`fk_` key prefix.** Stripe uses `sk_`/`pk_`; GitHub uses `ghp_`. Forge fixes `fk_` so keys are
  greppable in logs and identifiable on leak (enabling secret-scanning), with a one-obvious-way
  prefix rather than a configurable one.
- **Forge owns argon2id parameters.** PHP/libsodium let the caller pass cost options. Forge fixes
  them to the OWASP-current baseline and upgrades old hashes via `needs_rehash`, so apps never pick
  (or freeze) their own cost factors.
- **App owns `user_id`.** Unlike a full auth product (Auth0, Supabase Auth), Forge has no users
  table and no identity model. `user_id` / `owner_id` are opaque strings; the app composes the rest.
- **Both session timeouts are mandatory and explicit.** Many frameworks expose only one. Forge
  surfaces OWASP's idle *and* absolute timeouts as distinct `SessionOpts` fields, both always applied.

## Observability

Span `forge.auth.<op>` — `hash_password`, `verify_password`, `needs_rehash`, `create_session`,
`validate_session`, `revoke_session`, `revoke_all_sessions`, `create_api_key`, `verify_api_key`,
`revoke_api_key`. Fields:

| field | notes |
|-------|-------|
| `auth.op` | operation name |
| `auth.user_hash` | stable hash of `user_id`/`owner_id` — never the raw value |
| `auth.token_hash` | the at-rest SHA-256 of the session token / API key — never the raw token |
| `auth.key_id` | non-secret API-key id (safe to emit) |
| `auth.session_valid` | `validate_session`: whether a live session was found |
| `auth.key_valid` | `verify_api_key`: whether a live key was found |
| `auth.verify_ok` | `verify_password`: match outcome (bool) |
| `auth.revoked_count` | `revoke_all_sessions`: number of sessions removed |
| `auth.idle_secs`, `auth.absolute_secs` | resolved session timeouts in seconds |
| `auth.outcome` | `ok` / error variant |

**Never emitted:** passwords, raw session tokens, raw API keys, salts, PHC hash bytes, argon2
output. Only hashes, the non-secret `key_id`, booleans, counts, and durations.

## Non-goals

- **No users table / identity model.** `user_id` is app-owned and opaque.
- **No JWT / stateless sessions.** Revocable hashed tokens only (see Deviations).
- **No OAuth / social login, no SSO / SAML.** A post-v1 OAuth *helper* at most; not in v1.
- **No MFA / TOTP / WebAuthn.** Out of scope.
- **No RBAC / permissions / org / team management.** `verify_*` returns identity, not authorization.
- **No email-verification, password-reset, or magic-link flows.** The app composes these from `auth`
  + `queue` (send mail) + `kv` (short-lived tokens).
- **No password-strength / breach (HIBP) checking, no rate limiting / lockout.** Caller's concern.
- **No API-key expiry or scopes in v1.** Keys live until revoked; rotation is create-new-revoke-old.

# Auth, RPC, CORS, Rate Limit, SSRF — Security Audit

Scope: `crates/forge-runtime/src/gateway/`, `crates/forge-core/src/auth/*`,
`crates/forge-core/src/http/*`, `crates/forge-runtime/src/rate_limit/*`,
`crates/forge-runtime/src/function/router.rs`. SQL, macros, and codegen are
covered by other agents and intentionally out of scope here.

Severity is calibrated to the **default `forge.toml`** an operator gets from
`forge new`, not to the most-locked-down configuration. A finding that requires
the operator to opt-in to an insecure flag is rated lower; one that ships
insecure by default is rated higher.

---

## 1. JWKS fallback returns an arbitrary key when token has no `kid` — HIGH

**File:** `crates/forge-runtime/src/gateway/jwks.rs:153` (`get_any_key`)

When a JWT arrives without a `kid` header, the validator falls back to
`JwksClient::get_any_key()`, which returns the first RSA key it has cached.
If the configured JWKS endpoint serves keys for multiple tenants/projects
(Firebase shared issuer, Auth0 tenants behind a custom domain, Clerk multi-app
JWKS), an attacker who can mint a token signed by **any** key the JWKS exposes
gets that token accepted against this gateway.

**Exploit sketch:**
1. Two Firebase projects A and B share `https://www.googleapis.com/.../securetoken@system.gserviceaccount.com` as issuer but rotate kids.
2. Forge is configured for project A. Operator forgets `audience` (see #3).
3. Attacker signs a token for project B with the `kid` header stripped.
4. `decode_header` returns `kid=None`. Validator falls back to `get_any_key`,
   pulls the first cached key (could be B's), and accepts the token.

**Fix:** When `kid` is missing, **refuse** the token unless explicitly opted
into via a config flag (`jwks.allow_kidless = true`). Production JWKS-issued
tokens always carry a `kid`; a missing `kid` is itself a signal.

---

## 2. `validate_aud = false` by default — HIGH

**File:** `crates/forge-runtime/src/gateway/auth.rs:475`
(`Validation::validate_aud = false` when `audience.is_none()`).

The gateway disables audience validation when no audience is configured.
Combined with the bundled OAuth server (`oauth.rs`) which mints tokens with
`aud=forge:mcp` for MCP clients, this means: any token the same issuer mints
for **any other audience** (a sibling service, an MCP-only client, a CI bot)
is accepted at the RPC gateway.

**Exploit sketch:** Two Forge apps share an Auth0 tenant. App A configures
`issuer = "https://tenant.auth0.com/"` but no `audience`. App B mints tokens
with `aud=https://api.b.example.com`. A user with a B token now authenticates
against A.

**Fix:** Make `audience` **required** when `issuer` is set, or refuse to boot
when both are unset outside dev mode. Validate `aud` strictly — reject tokens
whose `aud` is missing or does not match.

---

## 3. CORS wildcard ships with a log warning, not a hard refusal — HIGH

**File:** `crates/forge-runtime/src/gateway/server.rs:435-444`

`allow_origins = ["*"]` plus `allow_credentials = true` is normalized to
"any origin without credentials" with a `warn!`. But the default still
permits `allow_headers(Any)` and `allow_methods(Any)`, and a wildcard origin
in production is essentially always a misconfiguration. The warning gets
swallowed in operator logs and never blocks startup.

**Exploit sketch:** Operator copies a sample config that has
`allow_origins = ["*"]`. Their RPC endpoint now accepts cross-origin POSTs
from any site. Even without credentials, this exposes idempotent mutations
that rely on bearer tokens leaked via other channels (e.g. extension storage,
prior CSRF on a sibling app) — and any public query without auth is now a
free CORS-readable data source.

**Fix:** Outside dev mode (FORGE_ENV != development), refuse to boot when
`allow_origins` contains `"*"` or when both `allow_headers` and
`allow_methods` are `Any`. Force the operator to enumerate.

---

## 4. OAuth rate limiter sees every request as the same IP — HIGH

**File:** `crates/forge-runtime/src/gateway/oauth.rs:1055` (`client_ip()`
returns `"unknown"` unconditionally).

OAuth endpoints (register, token, login, authorize) build rate-limit keys
from `client_ip()`, which always returns the string `"unknown"`. Every
caller across the entire planet shares one bucket. Limits like
`REGISTER_RATE_LIMIT = 10/min` and `LOGIN_FAIL_RATE_LIMIT = 5/min` become
global, not per-IP.

**Exploit sketch:** Attacker hits `/oauth/register` 10 times in a minute.
Legitimate users cannot register or log in for the rest of that window.
For `LOGIN_FAIL_RATE_LIMIT`, attacker can also burn the bucket to block
legitimate users from retrying after a typo.

**Fix:** Wire `ResolveClientIp` (already used by the main gateway middleware
stack) into OAuth handlers and have `client_ip()` read the resolved value
from request extensions. Fall back to `peer_addr()` rather than the literal
string `"unknown"`.

---

## 5. `HybridRateLimiter` per-user/per-IP scope is local-only — MEDIUM

**File:** `crates/forge-runtime/src/rate_limit/limiter.rs:256` and around.

The default limiter stores per-user and per-IP counters in an in-process
`DashMap`. Only `Scope::Global` reaches the DB. In a cluster of N nodes a
caller gets N× the configured limit, and a stateless load balancer routing
a hot user round-robin gets exactly N× amplification.

This is documented, but the default backend for `require_auth`-enforced
rate limits is `HybridRateLimiter`, so the default is unsound for any
multi-node deploy.

**Exploit sketch:** A "5 mutations/min/user" abuse limit becomes 5×N. An
attacker that can pin a session to round-robin gets unbounded burst.

**Fix:** Either default to `StrictRateLimiter` (DB-backed) for all scopes
when more than one node is registered in `forge_nodes`, or document
prominently and require an explicit opt-in to the hybrid backend. At
minimum, refuse silent local-only behavior in clustered mode.

---

## 6. JSON-depth middleware is bypassable via content-type — MEDIUM

**File:** `crates/forge-runtime/src/gateway/server.rs:1056-1080`

The depth-check middleware only inspects the body when the request
`Content-Type` starts with `application/json`. Axum's `Json<T>` extractor
deserializes the body based on the extractor type, not the content-type
header — a client can send a 10,000-deep nested JSON body with
`Content-Type: text/plain` and pass the depth gate, then hit `serde_json`
in the handler and exhaust the stack or CPU.

**Exploit sketch:** `curl -H 'Content-Type: text/plain' --data @bomb.json
https://app/_api/rpc/some_mutation`. Bomb is `{"a":` repeated 50k times.

**Fix:** Apply the depth check unconditionally for bodies sent to
`/_api/rpc/*` regardless of content-type, or alternately constrain the
RPC dispatcher to refuse non-`application/json` and non-`multipart/*`
content types up front.

---

## 7. `dev_mode()` only refuses when `FORGE_ENV=production` — MEDIUM

**File:** `crates/forge-runtime/src/gateway/auth.rs:141`

The dev-mode guard checks exactly one env var. Most managed platforms set
their own conventions: `NODE_ENV=production`, `RAILWAY_ENVIRONMENT=production`,
`FLY_APP_NAME=...`, `K_SERVICE=...` (Cloud Run), `KUBERNETES_SERVICE_HOST`,
`AWS_EXECUTION_ENV`. A user who deploys with `bun run start` (which sets
`NODE_ENV=production`) but never sets `FORGE_ENV` boots into dev auth mode,
where `dev_mode()` accepts unsigned tokens.

**Exploit sketch:** New user follows quickstart, deploys to Railway. Forgets
to set `FORGE_ENV`. Their gateway accepts any HS256 token signed with the
sentinel dev secret, effectively running unauthenticated.

**Fix:** Expand the guard to refuse when any of the common production
indicators are set, or invert the logic — require an explicit
`FORGE_ENV=development` (or `--dev` flag) to enable `dev_mode()`. Fail
closed.

---

## 8. Session cookie has no IP/UA binding — MEDIUM

**File:** `crates/forge-runtime/src/gateway/auth.rs:740` and surrounding
HMAC sign/verify of the OAuth session cookie.

The session cookie is an HMAC over `{session_id, expires_at}`. There is no
IP or User-Agent binding. A stolen cookie (XSS on a sibling app sharing the
parent domain, leaked through a browser-extension exfil, network capture
on a misconfigured TLS termination) is usable until expiry from anywhere.

**Fix:** Bind the cookie to a coarse IP class (e.g. /24 for IPv4, /48 for
IPv6) and/or a UA fingerprint hash, accepting some friction for mobile
roamers. At minimum: rotate the cookie on every privilege-affecting action
and invalidate the prior session_id server-side on logout (not just
delete the cookie).

---

## 9. SSRF guard does not resolve DNS — MEDIUM

**File:** `crates/forge-core/src/http/mod.rs:160` (`url_targets_private_ip`).

The guard checks only literal IPs in the URL. A URL with a hostname that
resolves to `169.254.169.254`, `127.0.0.1`, a ULA, or an IPv6 link-local
address sails through. DNS rebinding (TTL=0 dual-record) also defeats any
TOCTOU-safe check that ignores the resolver layer.

The code documents this as out-of-scope, but the framework markets a
"safe HTTP client" with `allow_private = false`. The current behavior is a
foot-gun: operators reasonably believe the guard protects them.

**Exploit sketch:** Mutation receives a `webhook_url` field from user input.
Attacker submits `http://metadata.example.com` whose A record points to
`169.254.169.254`. The guard accepts. The HTTP client fetches AWS/GCP
metadata. Exfil over the response body.

**Fix:** Resolve the hostname at connect time and re-check the resolved
IP, **and** force the socket to bind to that resolved IP (no second DNS
lookup) to defeat rebinding. `reqwest` supports custom resolvers — use
one. At minimum, document the gap loudly in the SSRF guard's rustdoc and
in `docs/docs/ship/`.

---

## 10. `audience()` builder bypasses reserved-claim guard — MEDIUM

**File:** `crates/forge-core/src/auth/claims.rs:193` (and reserved-list
filter in `sanitized_custom` / `get_claim`).

`ClaimsBuilder::claim("aud", _)` is filtered by the reserved-name list, but
the typed `audience()` builder writes through to `custom["aud"]` directly,
sidestepping the same guard. The resulting `Claims` carry an `aud` in the
custom map that competing code paths (especially anything that round-trips
through `sanitized_custom`) may or may not surface.

**Fix:** Store `aud` as a typed top-level field on `Claims`, not in the
custom map. The whole reason `claim("aud", …)` is reserved is that `aud`
must round-trip via the typed JWT field; the typed builder should write to
the same place. Today, `audience()` and `claim()` write to different
locations.

---

## 11. Unbounded `RegisterRequest` fields — LOW

**File:** `crates/forge-runtime/src/gateway/oauth.rs:194-208`

`client_name`, `redirect_uris`, `grant_types`, `response_types`, `scope`
and `contacts` are all `Option<String>` / `Option<Vec<String>>` with no
length caps. A single registration can be many megabytes (body size limit
caps the whole request, but a 1MB `client_name` is still indexed and
returned by every `/oauth/clients` listing).

**Fix:** Validate per-field caps (RFC 7591 doesn't dictate them, but
sensible bounds are 256 chars for names, 20 entries for URI lists, 2048
chars per URI).

---

## 12. Legacy `extract_client_ip` trusts XFF unconditionally — LOW

**File:** `crates/forge-runtime/src/gateway/mod.rs:89` (`extract_client_ip`).

The new `ResolveClientIp` middleware honors `trusted_proxies`. The legacy
function does not — anything still calling it reads `X-Forwarded-For`
without proxy validation. Spoofable.

**Fix:** Delete the legacy function or have it call `resolve_client_ip`.
Audit callers (`grep -rn extract_client_ip crates/`).

---

## 13. Attacker-controlled `kid` echoed into operator logs — LOW

**File:** `crates/forge-runtime/src/gateway/auth.rs:391` (and similar
`tracing::warn!` sites in jwks.rs).

When a JWT references an unknown `kid`, the kid string is logged verbatim.
A `kid` of `\x1b[2K\rok` or with newlines (`%0A`) injects forged log lines
into operators' log pipeline. Worse on JSON-line collectors that don't
escape control chars.

**Fix:** Sanitize `kid` (and any other client-provided string) before
logging — strip ASCII control characters and cap length to 64.

---

## Must-fix before GA

1, 2, 3, 4 — HIGH. These are insecure-by-default postures: any of them
turns a stock-config deployment into a token-acceptance hazard or a global
abuse vector.

5, 6, 7, 8, 9, 10 — MEDIUM. Either silent foot-guns (5, 7, 9) or weakened
defenses against credential theft and large-input attacks (6, 8, 10). All
need fixing before GA so the threat model documented in `docs/` matches
runtime behavior.

11, 12, 13 — LOW. Cleanup, but ship-blocking if the goal is a clean 1.0:
the framework brand is "secure by default", and log injection plus
unbounded inputs in the OAuth server undermine that claim.

//! OAuth 2.1 Authorization Server for MCP.
//!
//! Implements Authorization Code + PKCE flow so MCP clients like Claude Code
//! can auto-authenticate via browser login.

use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use forge_core::auth::Claims;
use forge_core::oauth::{self, validate_redirect_uri};
use forge_core::rate_limit::{RateLimitConfig, RateLimitKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::auth::AuthMiddleware;
use crate::rate_limit::StrictRateLimiter;

const AUTHORIZE_PAGE: &str = include_str!("oauth_authorize.html");
const AUTH_CODE_TTL_SECS: i64 = 60;
const MAX_REGISTERED_CLIENTS: i64 = 1000;
const CHALLENGE_METHOD_S256: &str = "S256";
const MCP_AUDIENCE: &str = "forge:mcp";

// Rate limiting constants
const REGISTER_RATE_LIMIT: u32 = 10; // per minute per IP
const LOGIN_FAIL_RATE_LIMIT: u32 = 5; // per minute per IP
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// CSRF token TTL. OAuth authorize -> form-post is interactive; 5 minutes is
/// generous for a human consent step while still bounding replay surface.
const CSRF_TTL_SECS: u64 = 300;

/// Length of the random nonce inside a CSRF token, in bytes. 16 bytes of
/// entropy is plenty when paired with an HMAC over (timestamp || nonce).
const CSRF_NONCE_LEN: usize = 16;

/// Stateless HMAC-signed CSRF token: `base64url(ts_be_u64 || nonce || hmac)`.
///
/// Lives entirely in the cookie and the form field — no node-local state — so
/// the OAuth flow tolerates a load balancer that bounces the authorize and the
/// POST callback to different nodes.
fn mint_csrf_token(secret: &[u8]) -> String {
    // CSPRNG nonce sourced from UUIDv4 (avoids pulling `rand` into the gateway
    // crate). 16 bytes matches the constant.
    let nonce: [u8; CSRF_NONCE_LEN] = *Uuid::new_v4().as_bytes();
    let ts: u64 = chrono::Utc::now().timestamp().max(0) as u64;

    let mut payload = Vec::with_capacity(8 + CSRF_NONCE_LEN);
    payload.extend_from_slice(&ts.to_be_bytes());
    payload.extend_from_slice(&nonce);

    let mac = match Hmac::<Sha256>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let mut mac = mac;
    mac.update(&payload);
    let sig = mac.finalize().into_bytes();

    let mut out = Vec::with_capacity(payload.len() + sig.len());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&sig);
    URL_SAFE_NO_PAD.encode(&out)
}

/// Verify a CSRF token: signature must match and the timestamp must be inside
/// the TTL window. Returns true only when both checks pass.
fn verify_csrf_token(token: &str, secret: &[u8]) -> bool {
    let bytes = match URL_SAFE_NO_PAD.decode(token) {
        Ok(b) => b,
        Err(_) => return false,
    };
    // 8 byte ts + 16 byte nonce + 32 byte HMAC-SHA256 output.
    const EXPECTED_LEN: usize = 8 + CSRF_NONCE_LEN + 32;
    if bytes.len() != EXPECTED_LEN {
        return false;
    }
    let (payload, sig) = match bytes.split_at_checked(8 + CSRF_NONCE_LEN) {
        Some(parts) => parts,
        None => return false,
    };

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(payload);
    if mac.verify_slice(sig).is_err() {
        return false;
    }

    // Bounds-checked: payload is exactly 8 + CSRF_NONCE_LEN bytes, so the
    // first 8 are always present.
    let ts_slice = match payload.get(..8) {
        Some(s) => s,
        None => return false,
    };
    let mut ts_bytes = [0u8; 8];
    ts_bytes.copy_from_slice(ts_slice);
    let ts = u64::from_be_bytes(ts_bytes);

    let now = chrono::Utc::now().timestamp().max(0) as u64;
    // Reject tokens issued in the (significant) future, allowing only minor
    // clock drift.
    if ts > now.saturating_add(60) {
        return false;
    }
    now.saturating_sub(ts) <= CSRF_TTL_SECS
}

/// Shared state for OAuth endpoints.
#[derive(Clone)]
pub struct OAuthState {
    pool: sqlx::PgPool,
    auth_middleware: Arc<AuthMiddleware>,
    token_issuer: Arc<dyn forge_core::TokenIssuer>,
    access_token_ttl_secs: i64,
    refresh_token_ttl_days: i64,
    auth_is_hmac: bool,
    project_name: String,
    jwt_secret: String,
    session_cookie_ttl_secs: i64,
    /// PG-backed rate limiter, shared across nodes. The OAuth endpoints used
    /// to keep their own in-memory limiter, which multiplied the effective
    /// quota by the cluster size.
    rate_limiter: Arc<StrictRateLimiter>,
    /// Whether `POST /_api/oauth/register` accepts unauthenticated callers.
    /// Mirrors `mcp.allow_unauthenticated_dcr` from forge.toml. Defaults
    /// to `false` so a fresh deployment is not an open client registry.
    allow_unauthenticated_dcr: bool,
}

impl OAuthState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: sqlx::PgPool,
        auth_middleware: Arc<AuthMiddleware>,
        token_issuer: Arc<dyn forge_core::TokenIssuer>,
        access_token_ttl_secs: i64,
        refresh_token_ttl_days: i64,
        auth_is_hmac: bool,
        project_name: String,
        jwt_secret: String,
        session_cookie_ttl_secs: i64,
        allow_unauthenticated_dcr: bool,
    ) -> Self {
        let rate_limiter = Arc::new(StrictRateLimiter::new(pool.clone()));
        Self {
            pool,
            auth_middleware,
            token_issuer,
            access_token_ttl_secs,
            refresh_token_ttl_days,
            auth_is_hmac,
            project_name,
            jwt_secret,
            session_cookie_ttl_secs,
            rate_limiter,
            allow_unauthenticated_dcr,
        }
    }

    /// Cluster-wide rate-limit check. Returns `true` when the request is
    /// inside the configured quota. On DB errors we fail closed — the OAuth
    /// surface is a known abuse target and unbounded access during a DB hiccup
    /// is worse than rejecting a few legitimate retries.
    async fn rate_check(&self, key: &str, limit: u32) -> bool {
        let cfg = RateLimitConfig::new(limit, RATE_WINDOW).with_key(RateLimitKey::Global);
        match self.rate_limiter.check(key, &cfg).await {
            Ok(r) => r.allowed,
            Err(e) => {
                tracing::warn!(error = %e, key = %key, "OAuth rate-limit check failed; denying");
                false
            }
        }
    }

    fn mint_csrf(&self) -> String {
        mint_csrf_token(self.jwt_secret.as_bytes())
    }

    fn validate_csrf(&self, token: &str) -> bool {
        verify_csrf_token(token, self.jwt_secret.as_bytes())
    }
}

// ── Well-known metadata endpoints ──────────────────────────────────────

#[derive(Serialize)]
pub struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
}

pub async fn well_known_oauth_metadata(
    headers: HeaderMap,
    State(_state): State<Arc<OAuthState>>,
) -> Json<AuthorizationServerMetadata> {
    let base = base_url_from_headers(&headers);
    Json(AuthorizationServerMetadata {
        issuer: base.clone(),
        authorization_endpoint: format!("{base}/_api/oauth/authorize"),
        token_endpoint: format!("{base}/_api/oauth/token"),
        registration_endpoint: format!("{base}/_api/oauth/register"),
        response_types_supported: vec!["code".into()],
        grant_types_supported: vec!["authorization_code".into(), "refresh_token".into()],
        code_challenge_methods_supported: vec![CHALLENGE_METHOD_S256.into()],
        token_endpoint_auth_methods_supported: vec!["none".into()],
    })
}

#[derive(Serialize)]
pub struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
}

pub async fn well_known_resource_metadata(
    headers: HeaderMap,
    State(_state): State<Arc<OAuthState>>,
) -> Json<ProtectedResourceMetadata> {
    let base = base_url_from_headers(&headers);
    Json(ProtectedResourceMetadata {
        resource: base.clone(),
        authorization_servers: vec![base],
    })
}

// ── Dynamic client registration ────────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub grant_types: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub client_id: String,
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub token_endpoint_auth_method: String,
}

pub async fn oauth_register(
    headers: HeaderMap,
    Extension(resolved_ip): Extension<super::ResolvedClientIp>,
    State(state): State<Arc<OAuthState>>,
    Json(req): Json<RegisterRequest>,
) -> Response {
    // RFC 7591 Dynamic Client Registration. By default we require the caller
    // to present a valid JWT, so a fresh deployment is not an open client
    // registry. Operators that want public registration (e.g. for IDE
    // integrations) flip `mcp.allow_unauthenticated_dcr = true` in forge.toml.
    if !state.allow_unauthenticated_dcr {
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(str::trim);
        let authenticated = match bearer {
            Some(token) if !token.is_empty() => state
                .auth_middleware
                .validate_token_async(token)
                .await
                .is_ok(),
            _ => false,
        };
        if !authenticated {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "registration_not_supported",
                    "error_description": "Dynamic client registration requires authentication. \
                        Set `[mcp] allow_unauthenticated_dcr = true` in forge.toml to enable \
                        anonymous registration."
                })),
            )
                .into_response();
        }
    }

    let ip = resolved_ip.0.as_deref().unwrap_or("unknown");
    let rate_key = format!("oauth:register:{ip}");
    if !state.rate_check(&rate_key, REGISTER_RATE_LIMIT).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "too_many_requests",
                "error_description": "Rate limit exceeded for client registration"
            })),
        )
            .into_response();
    }

    // Check client cap
    let count: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM forge_oauth_clients")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(Some(0))
        .unwrap_or(0);
    if count >= MAX_REGISTERED_CLIENTS {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "too_many_clients",
                "error_description": "Maximum number of registered clients reached"
            })),
        )
            .into_response();
    }

    if req.client_name.as_ref().is_some_and(|n| n.len() > 256) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_client_metadata",
                "error_description": "client_name must not exceed 256 characters"
            })),
        )
            .into_response();
    }
    if req.redirect_uris.len() > 20 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_client_metadata",
                "error_description": "redirect_uris must not exceed 20 entries"
            })),
        )
            .into_response();
    }
    for uri in &req.redirect_uris {
        if uri.len() > 2048 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_client_metadata",
                    "error_description": "each redirect_uri must not exceed 2048 characters"
                })),
            )
                .into_response();
        }
    }
    if req.grant_types.len() > 10 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_client_metadata",
                "error_description": "grant_types must not exceed 10 entries"
            })),
        )
            .into_response();
    }

    if req.redirect_uris.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_client_metadata",
                "error_description": "redirect_uris is required"
            })),
        )
            .into_response();
    }

    // Validate redirect URIs: reject fragments, require HTTPS for non-localhost
    for uri in &req.redirect_uris {
        // Reject URIs with fragments (per OAuth 2.1 spec)
        if uri.contains('#') {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_redirect_uri",
                    "error_description": "redirect_uri must not contain a fragment"
                })),
            )
                .into_response();
        }
        // Check scheme and host
        let is_localhost = uri.starts_with("http://localhost")
            || uri.starts_with("http://127.0.0.1")
            || uri.starts_with("http://[::1]");
        let is_https = uri.starts_with("https://");
        if !is_localhost && !is_https {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_redirect_uri",
                    "error_description": "redirect_uri must use HTTPS for non-localhost URIs"
                })),
            )
                .into_response();
        }
    }

    let client_id = Uuid::new_v4().to_string();
    let auth_method = req.token_endpoint_auth_method.as_deref().unwrap_or("none");

    let result = sqlx::query!(
        "INSERT INTO forge_oauth_clients (client_id, client_name, redirect_uris, token_endpoint_auth_method) \
         VALUES ($1, $2, $3, $4)",
        &client_id,
        req.client_name as _,
        &req.redirect_uris,
        auth_method,
    )
    .execute(&state.pool)
    .await;

    if let Err(e) = result {
        tracing::error!("Failed to register OAuth client: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "server_error",
                "error_description": "Failed to register client"
            })),
        )
            .into_response();
    }

    let grant_types = if req.grant_types.is_empty() {
        vec!["authorization_code".into()]
    } else {
        req.grant_types
    };

    (
        StatusCode::CREATED,
        Json(RegisterResponse {
            client_id,
            client_name: req.client_name,
            redirect_uris: req.redirect_uris,
            grant_types,
            token_endpoint_auth_method: auth_method.to_string(),
        }),
    )
        .into_response()
}

// ── Authorization endpoint ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AuthorizeQuery {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    #[serde(default = "default_s256")]
    pub code_challenge_method: String,
    pub state: Option<String>,
    pub scope: Option<String>,
    pub response_type: Option<String>,
}

fn default_s256() -> String {
    CHALLENGE_METHOD_S256.into()
}

pub async fn oauth_authorize_get(
    headers: HeaderMap,
    Extension(resolved_ip): Extension<super::ResolvedClientIp>,
    Query(params): Query<AuthorizeQuery>,
    State(state): State<Arc<OAuthState>>,
) -> Response {
    // Validate client_id
    let client = sqlx::query!(
        "SELECT client_id, client_name, redirect_uris FROM forge_oauth_clients WHERE client_id = $1",
        &params.client_id,
    )
    .fetch_optional(&state.pool)
    .await;

    let (_, client_name, redirect_uris) = match client {
        Ok(Some(c)) => (c.client_id, c.client_name, c.redirect_uris),
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_client",
                    "error_description": "Unknown client_id"
                })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("OAuth client lookup failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "server_error"
                })),
            )
                .into_response();
        }
    };

    // Validate redirect_uri (exact match, T2)
    if !validate_redirect_uri(&params.redirect_uri, &redirect_uris) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_redirect_uri",
                "error_description": "redirect_uri does not match any registered URI"
            })),
        )
            .into_response();
    }

    if params.code_challenge_method != CHALLENGE_METHOD_S256 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_request",
                "error_description": "Only S256 code_challenge_method is supported"
            })),
        )
            .into_response();
    }

    // Check for existing session cookie (set by auth middleware on API calls)
    let verify_ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let session_subject = extract_cookie(&headers, "forge_session").and_then(|v| {
        super::auth::verify_session_cookie(
            &v,
            &state.jwt_secret,
            resolved_ip.0.as_deref(),
            verify_ua.as_deref(),
        )
    });
    let has_session = session_subject.is_some();

    // Generate CSRF token (T5). Stateless HMAC-signed token so the form-post
    // callback can verify it on any node behind a load balancer.
    let csrf_token = state.mint_csrf();

    let auth_mode = if has_session {
        "session" // user is known from cookie, show consent directly
    } else if state.auth_is_hmac {
        "hmac" // show email/password form
    } else {
        "external" // show "log in to your app first"
    };
    let display_name = client_name.as_deref().unwrap_or(&params.client_id);

    let html = AUTHORIZE_PAGE
        .replace("{{app_name}}", &html_escape(&state.project_name))
        .replace("{{client_name}}", &html_escape(display_name))
        .replace("{{csrf_token}}", &csrf_token)
        .replace("{{client_id}}", &html_escape(&params.client_id))
        .replace("{{redirect_uri}}", &html_escape(&params.redirect_uri))
        .replace("{{code_challenge}}", &html_escape(&params.code_challenge))
        .replace(
            "{{code_challenge_method}}",
            &html_escape(&params.code_challenge_method),
        )
        .replace(
            "{{state}}",
            &html_escape(params.state.as_deref().unwrap_or("")),
        )
        .replace(
            "{{scope}}",
            &html_escape(params.scope.as_deref().unwrap_or("")),
        )
        .replace("{{auth_mode}}", &html_escape(auth_mode))
        .replace("{{authorize_url}}", "/_api/oauth/authorize")
        .replace("{{error_message}}", "");

    let mut response = (StatusCode::OK, Html(html)).into_response();
    // T17: clickjacking protection
    response
        .headers_mut()
        .insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    response.headers_mut().insert(
        "Content-Security-Policy",
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    // Set CSRF cookie (T1, T5). Cookie max-age tracks the token's HMAC TTL so
    // the browser drops the cookie at roughly the same time the server would
    // reject the token.
    let csrf_secure_flag = "; Secure";
    let cookie = format!(
        "forge_oauth_csrf={csrf_token}; Path=/_api/oauth/; HttpOnly; SameSite=Lax; Max-Age={CSRF_TTL_SECS}{csrf_secure_flag}"
    );
    if let Ok(cookie_val) = HeaderValue::from_str(&cookie) {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, cookie_val);
    }
    response
}

#[derive(Deserialize)]
pub struct AuthorizeForm {
    pub csrf_token: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub state: Option<String>,
    pub scope: Option<String>,
    pub response_type: Option<String>,
    // Consent flow: existing token from localStorage
    pub token: Option<String>,
    // Login flow: email/password
    pub email: Option<String>,
    pub password: Option<String>,
}

pub async fn oauth_authorize_post(
    headers: HeaderMap,
    Extension(resolved_ip): Extension<super::ResolvedClientIp>,
    State(state): State<Arc<OAuthState>>,
    axum::Form(form): axum::Form<AuthorizeForm>,
) -> Response {
    // Validate CSRF (T5): check both cookie and form value. The token itself
    // is HMAC-verified — node-local state no longer constrains which node
    // serves the form-post callback.
    let csrf_from_cookie = extract_cookie(&headers, "forge_oauth_csrf");
    let csrf_valid = if let Some(cookie_csrf) = csrf_from_cookie {
        let cookie_match: bool = cookie_csrf
            .as_bytes()
            .ct_eq(form.csrf_token.as_bytes())
            .into();
        cookie_match && state.validate_csrf(&form.csrf_token)
    } else {
        false
    };
    if !csrf_valid {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "csrf_validation_failed",
                "error_description": "Invalid or expired CSRF token. Please try again."
            })),
        )
            .into_response();
    }

    // Rate limit login failures (T7). PG-backed key so the budget is shared
    // across cluster nodes.
    let ip = resolved_ip.0.as_deref().unwrap_or("unknown");
    let rate_key = format!("oauth:login:{ip}");

    // Validate client and redirect_uri again (form could be tampered)
    let client = sqlx::query!(
        "SELECT redirect_uris FROM forge_oauth_clients WHERE client_id = $1",
        &form.client_id,
    )
    .fetch_optional(&state.pool)
    .await;

    let redirect_uris = match client {
        Ok(Some(c)) => c.redirect_uris,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_client"
                })),
            )
                .into_response();
        }
    };

    if !validate_redirect_uri(&form.redirect_uri, &redirect_uris) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_redirect_uri"
            })),
        )
            .into_response();
    }

    // Authenticate user: try session cookie first, then token, then email/password
    let user_id: Uuid;

    let post_verify_ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let session_subject = extract_cookie(&headers, "forge_session").and_then(|v| {
        super::auth::verify_session_cookie(
            &v,
            &state.jwt_secret,
            resolved_ip.0.as_deref(),
            post_verify_ua.as_deref(),
        )
    });

    if let Some(subject) = session_subject {
        // Session cookie flow: user identified by signed cookie from previous API calls.
        // Subject may be a UUID (HMAC auth) or an external provider ID (Firebase, Clerk).
        user_id = subject.parse::<Uuid>().unwrap_or_else(|_| {
            // Non-UUID subject (Firebase UID, etc.): deterministic UUID from subject hash.
            use sha2::Digest;
            let hash: [u8; 32] = sha2::Sha256::digest(subject.as_bytes()).into();
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&hash[..16]);
            Uuid::from_bytes(bytes)
        });
    } else if let Some(token) = &form.token {
        // Consent flow: validate existing JWT
        match state.auth_middleware.validate_token_async(token).await {
            Ok(claims) => {
                user_id = claims
                    .user_id()
                    .ok_or(())
                    .map_err(|_| ())
                    .unwrap_or_default();
                if user_id.is_nil() {
                    return authorize_error_redirect(
                        &form.redirect_uri,
                        form.state.as_deref(),
                        "access_denied",
                        "Invalid user identity in token",
                    );
                }
            }
            Err(_) => {
                return authorize_error_redirect(
                    &form.redirect_uri,
                    form.state.as_deref(),
                    "access_denied",
                    "Invalid or expired token. Please log in again.",
                );
            }
        }
    } else if let (Some(email), Some(password)) = (&form.email, &form.password) {
        // Login flow (HMAC mode only)
        if !state.auth_is_hmac {
            return authorize_error_redirect(
                &form.redirect_uri,
                form.state.as_deref(),
                "access_denied",
                "Direct login not supported with external auth provider",
            );
        }

        if !state.rate_check(&rate_key, LOGIN_FAIL_RATE_LIMIT).await {
            return authorize_error_redirect(
                &form.redirect_uri,
                form.state.as_deref(),
                "access_denied",
                "Too many login attempts. Please try again later.",
            );
        }

        // Query users table by convention
        let row = sqlx::query!(
            "SELECT id, password_hash, role::TEXT FROM users WHERE email = $1",
            email,
        )
        .fetch_optional(&state.pool)
        .await;

        // Constant-time login: always run argon2id verify even if user not
        // found to prevent timing side-channels that reveal valid emails.
        const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$BVZdp6MuG5LPIhHn/YNmhk/MWyLDoR//ljnfCNAr8Wg";
        let (found_id, hash) = match &row {
            Ok(Some(r)) if r.password_hash.is_some() => {
                (Some(r.id), r.password_hash.as_deref().unwrap_or(DUMMY_HASH))
            }
            _ => (None, DUMMY_HASH),
        };
        let password_valid = {
            use password_hash::PasswordHash;
            PasswordHash::new(hash)
                .ok()
                .and_then(|parsed| {
                    use argon2::{Algorithm, Argon2, Params, PasswordVerifier, Version};
                    let params = Params::new(65536, 3, 1, None).expect("valid argon2 params");
                    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
                        .verify_password(password.as_bytes(), &parsed)
                        .ok()
                })
                .is_some()
        };
        if password_valid {
            if let Some(id) = found_id {
                user_id = id;
            } else {
                return authorize_error_redirect(
                    &form.redirect_uri,
                    form.state.as_deref(),
                    "access_denied",
                    "Invalid email or password",
                );
            }
        } else {
            return authorize_error_redirect(
                &form.redirect_uri,
                form.state.as_deref(),
                "access_denied",
                "Invalid email or password",
            );
        }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_request",
                "error_description": "Must provide either a token or email/password"
            })),
        )
            .into_response();
    }

    // Generate authorization code
    let code = oauth::generate_random_token();
    let expires_at = Utc::now() + chrono::Duration::seconds(AUTH_CODE_TTL_SECS);
    let scopes: Vec<String> = form
        .scope
        .as_deref()
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    let result = sqlx::query!(
        "INSERT INTO forge_oauth_codes \
         (code, client_id, user_id, redirect_uri, code_challenge, code_challenge_method, scopes, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &code,
        &form.client_id,
        user_id,
        &form.redirect_uri,
        &form.code_challenge,
        &form.code_challenge_method,
        &scopes,
        expires_at,
    )
    .execute(&state.pool)
    .await;

    if let Err(e) = result {
        tracing::error!("Failed to store authorization code: {e}");
        return authorize_error_redirect(
            &form.redirect_uri,
            form.state.as_deref(),
            "server_error",
            "Failed to generate authorization code",
        );
    }

    // Redirect with code (T18: Referrer-Policy)
    let mut redirect_url = format!("{}?code={}", form.redirect_uri, urlencoding(&code));
    if let Some(st) = &form.state {
        redirect_url.push_str(&format!("&state={}", urlencoding(st)));
    }

    let mut response = Redirect::to(&redirect_url).into_response();
    response
        .headers_mut()
        .insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));

    // Set session cookie so the next authorize visit shows consent directly
    // instead of the login form. This is same-origin (backend serves both
    // the authorize page and this POST), so the cookie sticks.
    let cookie_ttl = state.session_cookie_ttl_secs;
    let sign_ua = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let cookie_value = super::auth::sign_session_cookie(
        &user_id.to_string(),
        &state.jwt_secret,
        cookie_ttl,
        resolved_ip.0.as_deref(),
        sign_ua,
    );
    let secure_flag = "; Secure";
    let session_cookie = format!(
        "forge_session={cookie_value}; Path=/_api/oauth/; HttpOnly; SameSite=Lax; Max-Age={cookie_ttl}{secure_flag}"
    );
    if let Ok(val) = HeaderValue::from_str(&session_cookie) {
        response.headers_mut().append(header::SET_COOKIE, val);
    }

    response
}

// ── Token endpoint ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub client_id: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: String,
}

/// Token endpoint accepts both `application/json` and
/// `application/x-www-form-urlencoded` (OAuth 2.1 standard).
pub async fn oauth_token(
    State(state): State<Arc<OAuthState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let req: TokenRequest = if content_type.starts_with("application/json") {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return token_error("invalid_request", &format!("Invalid JSON: {e}")),
        }
    } else {
        // Default to form-urlencoded (OAuth standard)
        match serde_urlencoded::from_bytes(&body) {
            Ok(r) => r,
            Err(e) => return token_error("invalid_request", &format!("Invalid form data: {e}")),
        }
    };

    match req.grant_type.as_str() {
        "authorization_code" => handle_code_exchange(&state, &req).await,
        "refresh_token" => handle_refresh(&state, &req).await,
        _ => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "unsupported_grant_type"
            })),
        )
            .into_response(),
    }
}

async fn handle_code_exchange(state: &OAuthState, req: &TokenRequest) -> Response {
    let code = match &req.code {
        Some(c) => c,
        None => return token_error("invalid_request", "code is required"),
    };
    let code_verifier = match &req.code_verifier {
        Some(v) => v,
        None => return token_error("invalid_request", "code_verifier is required"),
    };
    let redirect_uri = match &req.redirect_uri {
        Some(r) => r,
        None => return token_error("invalid_request", "redirect_uri is required"),
    };
    let client_id = match &req.client_id {
        Some(c) => c,
        None => return token_error("invalid_request", "client_id is required"),
    };

    // Atomic exchange: mark code as used and fetch in one query (T8)
    let row = sqlx::query!(
        "UPDATE forge_oauth_codes SET used_at = now() \
         WHERE code = $1 AND used_at IS NULL \
         RETURNING client_id, user_id, redirect_uri, code_challenge, code_challenge_method, expires_at",
        code,
    )
    .fetch_optional(&state.pool)
    .await;

    let (
        stored_client_id,
        user_id,
        stored_redirect,
        stored_challenge,
        challenge_method,
        expires_at,
    ) = match row {
        Ok(Some(r)) => (
            r.client_id,
            r.user_id,
            r.redirect_uri,
            r.code_challenge,
            r.code_challenge_method,
            r.expires_at,
        ),
        Ok(None) => {
            return token_error(
                "invalid_grant",
                "Invalid or already used authorization code",
            );
        }
        Err(e) => {
            tracing::error!("Failed to exchange authorization code: {e}");
            return token_error("server_error", "Failed to exchange code");
        }
    };

    // Check expiry
    if Utc::now() > expires_at {
        return token_error("invalid_grant", "Authorization code has expired");
    }

    // Verify client_id matches (T14)
    if *client_id != stored_client_id {
        return token_error("invalid_grant", "client_id does not match");
    }

    // Verify redirect_uri matches
    if *redirect_uri != stored_redirect {
        return token_error("invalid_grant", "redirect_uri does not match");
    }

    if challenge_method != CHALLENGE_METHOD_S256 {
        return token_error("invalid_request", "Unsupported code_challenge_method");
    }
    if !forge_core::oauth::pkce::verify_s256(code_verifier, &stored_challenge) {
        return token_error("invalid_grant", "PKCE verification failed");
    }

    let access_ttl = state.access_token_ttl_secs;
    let refresh_ttl = state.refresh_token_ttl_days;

    let pair = forge_core::auth::tokens::issue_token_pair_with_client(
        &state.pool,
        user_id,
        &["user"],
        access_ttl,
        refresh_ttl,
        Some(client_id),
        mcp_token_issuer(state.token_issuer.clone()),
    )
    .await;

    match pair {
        Ok(pair) => (
            StatusCode::OK,
            Json(TokenResponse {
                access_token: pair.access_token,
                token_type: "Bearer".into(),
                expires_in: access_ttl,
                refresh_token: pair.refresh_token,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to issue token pair: {e}");
            token_error("server_error", "Failed to issue tokens")
        }
    }
}

async fn handle_refresh(state: &OAuthState, req: &TokenRequest) -> Response {
    let refresh_token = match &req.refresh_token {
        Some(t) => t,
        None => return token_error("invalid_request", "refresh_token is required"),
    };
    let client_id = req.client_id.as_deref();

    let access_ttl = state.access_token_ttl_secs;
    let refresh_ttl = state.refresh_token_ttl_days;

    let pair = forge_core::auth::tokens::rotate_refresh_token_with_client(
        &state.pool,
        refresh_token,
        access_ttl,
        refresh_ttl,
        client_id,
        mcp_token_issuer(state.token_issuer.clone()),
    )
    .await;

    match pair {
        Ok(pair) => (
            StatusCode::OK,
            Json(TokenResponse {
                access_token: pair.access_token,
                token_type: "Bearer".into(),
                expires_in: access_ttl,
                refresh_token: pair.refresh_token,
            }),
        )
            .into_response(),
        Err(_) => token_error("invalid_grant", "Invalid or expired refresh token"),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Build a token-signing closure scoped to MCP audience.
fn mcp_token_issuer(
    issuer: Arc<dyn forge_core::TokenIssuer>,
) -> impl FnOnce(Uuid, &[&str], i64) -> forge_core::Result<String> {
    move |uid, roles, ttl| {
        let claims = Claims::builder()
            .subject(uid)
            .roles(roles.iter().map(|s| s.to_string()).collect())
            .audience(MCP_AUDIENCE)
            .duration_secs(ttl)
            .build()
            .map_err(forge_core::ForgeError::internal)?;
        issuer.sign(&claims)
    }
}

fn token_error(error: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": error,
            "error_description": description
        })),
    )
        .into_response()
}

fn authorize_error_redirect(
    redirect_uri: &str,
    state: Option<&str>,
    error: &str,
    description: &str,
) -> Response {
    let mut url = format!(
        "{}?error={}&error_description={}",
        redirect_uri,
        urlencoding(error),
        urlencoding(description),
    );
    if let Some(st) = state {
        url.push_str(&format!("&state={}", urlencoding(st)));
    }
    Redirect::to(&url).into_response()
}

fn base_url_from_headers(headers: &HeaderMap) -> String {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:9081");

    // Do not trust x-forwarded-proto: OAuth routes bypass the trusted-proxy
    // middleware, so any client can spoof the header. Default to "https" for
    // production safety; localhost gets "http" for local development.
    let scheme = if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
        "http"
    } else {
        "https"
    };

    format!("{scheme}://{host}")
}

fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').map(|c| c.trim()).find_map(|c| {
                let (k, v) = c.split_once('=')?;
                if k == name { Some(v.to_string()) } else { None }
            })
        })
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn urlencoding(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn html_escape_neutralizes_script_tags() {
        let xss = "<script>alert('xss')</script>";
        let escaped = html_escape(xss);
        assert_eq!(
            escaped,
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
    }

    #[test]
    fn html_escape_handles_quotes_and_ampersands() {
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape(r#"say "hi""#), "say &quot;hi&quot;");
        assert_eq!(html_escape("it's"), "it&#x27;s");
    }

    #[test]
    fn html_escape_orders_ampersand_first_to_avoid_double_escape() {
        // If we replaced `<` first as `&lt;`, then replaced `&` -> `&amp;`,
        // the literal `<` would render as `&amp;lt;` and break the page.
        // The current implementation escapes `&` first, so a raw `<` becomes
        // exactly `&lt;`.
        assert_eq!(html_escape("<"), "&lt;");
    }

    #[test]
    fn urlencoding_percent_encodes_non_alphanumeric() {
        // NON_ALPHANUMERIC encodes everything except [A-Za-z0-9].
        assert_eq!(urlencoding("hello world"), "hello%20world");
        assert_eq!(urlencoding("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
    }

    #[test]
    fn urlencoding_preserves_alphanumerics() {
        assert_eq!(urlencoding("AbCdEf123"), "AbCdEf123");
    }

    #[test]
    fn base_url_defaults_to_https_for_remote_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("api.example.com"));
        assert_eq!(base_url_from_headers(&headers), "https://api.example.com");
    }

    #[test]
    fn base_url_uses_http_for_localhost() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("localhost:9081"));
        assert_eq!(base_url_from_headers(&headers), "http://localhost:9081");

        headers.insert("host", HeaderValue::from_static("127.0.0.1:9081"));
        assert_eq!(base_url_from_headers(&headers), "http://127.0.0.1:9081");
    }

    #[test]
    fn base_url_ignores_x_forwarded_proto() {
        // Anyone can spoof X-Forwarded-Proto on OAuth routes (no trusted-proxy
        // middleware). Verify we ignore it and use the safe default.
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("api.example.com"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert_eq!(base_url_from_headers(&headers), "https://api.example.com");
    }

    #[test]
    fn base_url_falls_back_when_host_missing() {
        let headers = HeaderMap::new();
        assert_eq!(base_url_from_headers(&headers), "http://localhost:9081");
    }

    #[test]
    fn extract_cookie_finds_named_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("session=abc123; theme=dark"),
        );
        assert_eq!(extract_cookie(&headers, "session"), Some("abc123".into()));
        assert_eq!(extract_cookie(&headers, "theme"), Some("dark".into()));
        assert_eq!(extract_cookie(&headers, "missing"), None);
    }

    #[test]
    fn extract_cookie_handles_whitespace_between_pairs() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("a=1;   b=2;\tc=3"));
        assert_eq!(extract_cookie(&headers, "b"), Some("2".into()));
        assert_eq!(extract_cookie(&headers, "c"), Some("3".into()));
    }

    #[test]
    fn extract_cookie_returns_none_when_header_absent() {
        let headers = HeaderMap::new();
        assert_eq!(extract_cookie(&headers, "anything"), None);
    }

    #[test]
    fn extract_cookie_skips_malformed_pairs() {
        // A cookie segment with no `=` is simply skipped, not treated as a
        // match for any name.
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("malformed; real=value"),
        );
        assert_eq!(extract_cookie(&headers, "malformed"), None);
        assert_eq!(extract_cookie(&headers, "real"), Some("value".into()));
    }

    #[test]
    fn csrf_round_trip_accepts_freshly_minted_token() {
        let secret = b"oauth-csrf-secret-32-bytes-pad!!!";
        let token = mint_csrf_token(secret);
        assert!(!token.is_empty());
        assert!(verify_csrf_token(&token, secret));
    }

    #[test]
    fn csrf_verify_rejects_wrong_secret() {
        let token = mint_csrf_token(b"secret-A-32-bytes-pad!!!!!!!!!!!");
        assert!(!verify_csrf_token(
            &token,
            b"secret-B-32-bytes-pad!!!!!!!!!!!"
        ));
    }

    #[test]
    fn csrf_verify_rejects_tampered_payload() {
        let secret = b"oauth-csrf-secret-32-bytes-pad!!!";
        let token = mint_csrf_token(secret);
        // Flip the last char of the base64url string to perturb the HMAC.
        let mut tampered = token.clone();
        let last = tampered.pop().expect("token non-empty");
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(!verify_csrf_token(&tampered, secret));
    }

    #[test]
    fn csrf_verify_rejects_garbage_input() {
        let secret = b"oauth-csrf-secret-32-bytes-pad!!!";
        assert!(!verify_csrf_token("not-base64-!!!", secret));
        assert!(!verify_csrf_token("", secret));
        assert!(!verify_csrf_token("AAAA", secret)); // valid base64 but wrong length
    }

    #[test]
    fn csrf_verify_rejects_token_older_than_ttl() {
        let secret = b"oauth-csrf-secret-32-bytes-pad!!!";

        // Hand-craft a token with a stale timestamp so we don't have to wait
        // CSRF_TTL_SECS in a unit test.
        let stale_ts: u64 = (chrono::Utc::now().timestamp() - (CSRF_TTL_SECS as i64) - 5) as u64;
        let nonce: [u8; CSRF_NONCE_LEN] = *Uuid::new_v4().as_bytes();
        let mut payload = Vec::with_capacity(8 + CSRF_NONCE_LEN);
        payload.extend_from_slice(&stale_ts.to_be_bytes());
        payload.extend_from_slice(&nonce);

        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key");
        mac.update(&payload);
        let sig = mac.finalize().into_bytes();

        let mut out = Vec::with_capacity(payload.len() + sig.len());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&sig);
        let stale_token = URL_SAFE_NO_PAD.encode(&out);

        assert!(!verify_csrf_token(&stale_token, secret));
    }

    #[tokio::test]
    async fn token_error_returns_400_with_oauth_error_shape() {
        let resp = token_error("invalid_grant", "bad code");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_grant");
        assert_eq!(json["error_description"], "bad code");
    }

    #[test]
    fn authorize_error_redirect_encodes_query_params_and_state() {
        let resp = authorize_error_redirect(
            "https://client.example.com/cb",
            Some("xyz state"),
            "access_denied",
            "user said no",
        );
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(location.starts_with("https://client.example.com/cb?"));
        // Parameter names are hard-coded literals; only the values are passed
        // through urlencoding. NON_ALPHANUMERIC encodes both `_` (%5F) and ` `
        // (%20), so error VALUES carrying underscores get encoded too.
        assert!(location.contains("error=access%5Fdenied"), "got {location}");
        assert!(
            location.contains("error_description=user%20said%20no"),
            "got {location}"
        );
        assert!(location.contains("state=xyz%20state"), "got {location}");
    }

    #[test]
    fn authorize_error_redirect_omits_state_when_absent() {
        let resp = authorize_error_redirect(
            "https://client.example.com/cb",
            None,
            "invalid_request",
            "missing param",
        );
        let location = resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(!location.contains("state="));
        assert!(
            location.contains("error=invalid%5Frequest"),
            "got {location}"
        );
    }

    #[test]
    fn default_s256_returns_canonical_pkce_method() {
        assert_eq!(default_s256(), "S256");
    }
}

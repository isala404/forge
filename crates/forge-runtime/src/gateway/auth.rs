use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
use forge_core::auth::Claims;
use forge_core::config::JwtAlgorithm as CoreJwtAlgorithm;
use forge_core::function::AuthContext;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, dangerous, decode, encode};
use sha2::{Digest, Sha256};
use tracing::debug;

use super::jwks::JwksClient;

/// Derive a stable, opaque key id from an HMAC secret. We take the first
/// 8 hex chars of `SHA-256(secret_bytes)` — short enough to keep token
/// headers small, deterministic so the same secret always produces the
/// same kid, and one-way so it leaks nothing useful about the secret.
fn secret_kid(secret: &[u8]) -> String {
    let hash = Sha256::digest(secret);
    let prefix = hash.as_slice().get(..4).unwrap_or(&[]);
    let mut out = String::with_capacity(prefix.len() * 2);
    for byte in prefix {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Sanitize a client-provided string for safe logging. Strips ASCII control
/// characters and truncates to 64 bytes to prevent log injection.
fn sanitize_for_log(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_control())
        .take(64)
        .collect()
}

/// Environment snapshot for dev-mode guard. Each field represents a standard
/// production-environment indicator; the presence of any blocks dev mode.
#[derive(Default)]
struct DevModeEnv {
    forge_env: Option<String>,
    node_env: Option<String>,
    railway_environment: Option<String>,
    k_service: Option<String>,
    fly_app_name: Option<String>,
    kubernetes_service_host: Option<String>,
    aws_execution_env: Option<String>,
}

/// Operating mode for the auth middleware.
///
/// `Production` is the only mode that runs JWT signature verification.
/// `Development` accepts unsigned tokens for local iteration and is constructed
/// only via `AuthConfig::dev_mode()`, which refuses to build when
/// `FORGE_ENV=production`. There is no parsed config field that selects this —
/// the variant is chosen by the constructor, so production never has a chance
/// to land in `Development` accidentally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    #[default]
    Production,
    Development,
}

/// Authentication configuration for the runtime.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// JWT secret for HMAC algorithms (HS256).
    pub jwt_secret: Option<String>,
    /// JWT algorithm.
    pub algorithm: JwtAlgorithm,
    /// JWKS client for RSA algorithms.
    pub jwks_client: Option<Arc<JwksClient>>,
    /// Expected token issuer (iss claim).
    pub issuer: Option<String>,
    /// Expected audience (aud claim).
    pub audience: Option<String>,
    /// Clock-skew tolerance for `exp` / `nbf` validation, in seconds.
    pub leeway_secs: u64,
    /// Session cookie lifetime in seconds. Defaults to the access token TTL.
    pub session_cookie_ttl_secs: i64,
    /// Old HMAC secrets still accepted for validation (never signing). Each entry has a
    /// mandatory `valid_until`; expired entries are dropped at middleware construction.
    pub legacy_secrets: Vec<forge_core::config::LegacySecret>,
    /// JWT spec claims that must be present. Derived from `required_claims` in forge.toml.
    pub required_claims: Vec<String>,
    /// Reject RS256 tokens without a `kid` header. Default: true.
    pub jwks_require_kid: bool,
    /// Auth mode. Only `Development` skips signature verification, and the only
    /// constructor that yields `Development` already refuses production env.
    pub(crate) mode: AuthMode,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: None,
            algorithm: JwtAlgorithm::HS256,
            jwks_client: None,
            issuer: None,
            audience: None,
            leeway_secs: 60,
            session_cookie_ttl_secs: 3600,
            legacy_secrets: Vec::new(),
            required_claims: vec!["exp".into(), "sub".into()],
            jwks_require_kid: true,
            mode: AuthMode::Production,
        }
    }
}

impl AuthConfig {
    /// Create auth config from forge core config.
    pub fn from_forge_config(
        config: &forge_core::config::AuthConfig,
    ) -> Result<Self, super::jwks::JwksError> {
        let algorithm = JwtAlgorithm::from(config.jwt_algorithm);

        let jwks_client = config
            .jwks_url
            .as_ref()
            .map(|url| JwksClient::new(url.clone(), config.jwks_cache_ttl.as_secs()).map(Arc::new))
            .transpose()?;

        Ok(Self {
            jwt_secret: config.jwt_secret.clone(),
            algorithm,
            jwks_client,
            issuer: config.jwt_issuer.clone(),
            audience: config.jwt_audience.clone(),
            leeway_secs: config.jwt_leeway.as_secs(),
            session_cookie_ttl_secs: config.session_cookie_ttl_secs(),
            legacy_secrets: config.legacy_secrets.clone(),
            required_claims: config.required_claims.clone(),
            jwks_require_kid: config.jwks_require_kid,
            mode: AuthMode::Production,
        })
    }

    /// Create a new auth config with the given HMAC secret.
    pub fn with_secret(secret: impl Into<String>) -> Self {
        Self {
            jwt_secret: Some(secret.into()),
            ..Default::default()
        }
    }

    /// Create a dev mode config that skips signature verification.
    /// WARNING: Only use this for development and testing.
    ///
    /// Fails closed in production: returns `ForgeError::Config` when
    /// `FORGE_ENV=production` or any standard production environment indicator
    /// is detected. The startup must abort, not auto-correct.
    pub fn dev_mode() -> forge_core::Result<Self> {
        Self::dev_mode_with_env(DevModeEnv {
            forge_env: std::env::var("FORGE_ENV").ok(),
            node_env: std::env::var("NODE_ENV").ok(),
            railway_environment: std::env::var("RAILWAY_ENVIRONMENT").ok(),
            k_service: std::env::var("K_SERVICE").ok(),
            fly_app_name: std::env::var("FLY_APP_NAME").ok(),
            kubernetes_service_host: std::env::var("KUBERNETES_SERVICE_HOST").ok(),
            aws_execution_env: std::env::var("AWS_EXECUTION_ENV").ok(),
        })
    }

    /// Inner constructor that takes a resolved environment snapshot. Split out
    /// from `dev_mode()` so tests can exercise the production guard without
    /// touching process-global env state.
    fn dev_mode_with_env(env: DevModeEnv) -> forge_core::Result<Self> {
        if env
            .forge_env
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("production"))
        {
            return Err(forge_core::ForgeError::config(
                "AuthConfig::dev_mode() refused: FORGE_ENV=production. \
                 Configure a real jwt_secret or jwks_url instead.",
            ));
        }
        if env
            .node_env
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("production"))
        {
            return Err(forge_core::ForgeError::config(
                "AuthConfig::dev_mode() refused: NODE_ENV=production detected. \
                 Configure a real jwt_secret or jwks_url instead.",
            ));
        }
        let indicators = [
            ("RAILWAY_ENVIRONMENT", &env.railway_environment),
            ("K_SERVICE", &env.k_service),
            ("FLY_APP_NAME", &env.fly_app_name),
            ("KUBERNETES_SERVICE_HOST", &env.kubernetes_service_host),
            ("AWS_EXECUTION_ENV", &env.aws_execution_env),
        ];
        for (name, val) in &indicators {
            if val.is_some() {
                return Err(forge_core::ForgeError::config(format!(
                    "AuthConfig::dev_mode() refused: {name} is set, indicating a production \
                         environment. Configure a real jwt_secret or jwks_url instead."
                )));
            }
        }
        Ok(Self {
            jwt_secret: None,
            algorithm: JwtAlgorithm::HS256,
            jwks_client: None,
            issuer: None,
            audience: None,
            leeway_secs: 60,
            session_cookie_ttl_secs: 3600,
            legacy_secrets: Vec::new(),
            required_claims: vec!["exp".into(), "sub".into()],
            jwks_require_kid: true,
            mode: AuthMode::Development,
        })
    }

    /// Check if this config uses HMAC (symmetric) algorithms.
    pub fn is_hmac(&self) -> bool {
        matches!(self.algorithm, JwtAlgorithm::HS256)
    }

    /// Check if this config uses RSA (asymmetric) algorithms.
    pub fn is_rsa(&self) -> bool {
        matches!(self.algorithm, JwtAlgorithm::RS256)
    }

    /// True when running in development mode, which bypasses signature checks.
    /// Always false for configs built from `from_forge_config` or `with_secret`.
    pub fn skips_verification(&self) -> bool {
        matches!(self.mode, AuthMode::Development)
    }
}

/// Supported JWT algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JwtAlgorithm {
    #[default]
    HS256,
    RS256,
}

impl From<JwtAlgorithm> for Algorithm {
    fn from(alg: JwtAlgorithm) -> Self {
        match alg {
            JwtAlgorithm::HS256 => Algorithm::HS256,
            JwtAlgorithm::RS256 => Algorithm::RS256,
        }
    }
}

impl From<CoreJwtAlgorithm> for JwtAlgorithm {
    fn from(alg: CoreJwtAlgorithm) -> Self {
        match alg {
            CoreJwtAlgorithm::HS256 => JwtAlgorithm::HS256,
            CoreJwtAlgorithm::RS256 => JwtAlgorithm::RS256,
            // CoreJwtAlgorithm is #[non_exhaustive]; refuse to silently coerce
            // a future variant to HS256, log and fall back conservatively.
            _ => {
                tracing::error!(
                    "Unknown CoreJwtAlgorithm variant; falling back to HS256. \
                     Update forge-runtime to support this algorithm."
                );
                JwtAlgorithm::HS256
            }
        }
    }
}

/// Token issuer for HMAC-based JWT signing.
///
/// Created from the auth config when an HMAC algorithm is configured.
/// Passed into MutationContext so handlers can call `ctx.issue_token()`.
#[derive(Clone)]
pub struct HmacTokenIssuer {
    secret: String,
    /// Stable kid emitted in every token header so verifiers can pick the
    /// right key during rotation without trying every legacy secret.
    kid: String,
    algorithm: Algorithm,
}

impl HmacTokenIssuer {
    /// Create a token issuer from auth config, if HMAC auth is configured.
    pub fn from_config(config: &AuthConfig) -> Option<Self> {
        if !config.is_hmac() {
            return None;
        }
        let secret = config.jwt_secret.as_ref()?.clone();
        if secret.is_empty() {
            return None;
        }
        // Startup validation (ForgeConfig::validate) hard-fails on secrets < 32 bytes.
        // The warn here is a last-resort signal when the issuer is created outside
        // the normal startup path (e.g. integration tests with minimal configs).
        if secret.len() < 32 {
            tracing::warn!(
                secret_len = secret.len(),
                "JWT secret is shorter than 32 bytes; startup validation should have caught this"
            );
        }
        let kid = secret_kid(secret.as_bytes());
        Some(Self {
            secret,
            kid,
            algorithm: config.algorithm.into(),
        })
    }
}

impl forge_core::TokenIssuer for HmacTokenIssuer {
    fn sign(&self, claims: &Claims) -> forge_core::Result<String> {
        let mut header = jsonwebtoken::Header::new(self.algorithm);
        header.kid = Some(self.kid.clone());
        encode(
            &header,
            claims,
            &jsonwebtoken::EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| forge_core::ForgeError::internal_with("token signing error", e))
    }
}

/// Authentication middleware.
#[derive(Clone)]
pub struct AuthMiddleware {
    config: Arc<AuthConfig>,
    /// Pre-computed HMAC decoding key (for performance).
    hmac_key: Option<DecodingKey>,
    /// Stable kid for the active HMAC secret. `None` when no HMAC secret is
    /// configured (RSA, dev mode, or empty secret).
    hmac_kid: Option<String>,
    /// Pre-computed decoding keys for legacy HMAC secrets (rotation grace),
    /// each paired with the kid of the underlying secret. The kid lets the
    /// validator look up the right key directly when the token carries one.
    legacy_hmac_keys: Vec<(String, DecodingKey)>,
    /// Positive token cache: maps token hash -> (Claims, expiry). Avoids
    /// re-validating the same JWT on every request.
    token_cache: Arc<dashmap::DashMap<u64, (Claims, std::time::Instant)>>,
}

impl std::fmt::Debug for AuthMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthMiddleware")
            .field("config", &self.config)
            .field("hmac_key", &self.hmac_key.is_some())
            .field("hmac_kid", &self.hmac_kid)
            .field("legacy_hmac_keys", &self.legacy_hmac_keys.len())
            .finish()
    }
}

impl AuthMiddleware {
    /// Create a new auth middleware.
    pub fn new(config: AuthConfig) -> Self {
        if config.skips_verification() {
            tracing::warn!("JWT signature verification is DISABLED. Do not use in production.");
        }

        // Pre-compute HMAC key if using HMAC algorithm
        let active_secret = if !config.skips_verification() && config.is_hmac() {
            config.jwt_secret.as_deref().filter(|s| !s.is_empty())
        } else {
            None
        };
        let hmac_key = active_secret.map(|s| DecodingKey::from_secret(s.as_bytes()));
        let hmac_kid = active_secret.map(|s| secret_kid(s.as_bytes()));

        let legacy_hmac_keys = if config.is_hmac() && !config.skips_verification() {
            let now = chrono::Utc::now();
            config
                .legacy_secrets
                .iter()
                .filter(|ls| {
                    if ls.secret.is_empty() {
                        return false;
                    }
                    if ls.valid_until <= now {
                        tracing::warn!(
                            valid_until = %ls.valid_until,
                            "Legacy JWT secret is expired and will not be used for verification"
                        );
                        return false;
                    }
                    true
                })
                .map(|ls| {
                    (
                        secret_kid(ls.secret.as_bytes()),
                        DecodingKey::from_secret(ls.secret.as_bytes()),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        Self {
            config: Arc::new(config),
            hmac_key,
            hmac_kid,
            legacy_hmac_keys,
            token_cache: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Create a middleware that allows all requests (development mode).
    /// WARNING: skips signature verification. Refuses to construct when
    /// `FORGE_ENV=production`.
    pub fn permissive() -> forge_core::Result<Self> {
        Ok(Self::new(AuthConfig::dev_mode()?))
    }

    /// Get the config.
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Validate a JWT token and extract claims. Results are cached by token
    /// hash for up to 60 seconds (or until the token's `exp` claim, whichever
    /// is sooner) to avoid re-validating the same JWT on every request.
    pub async fn validate_token_async(&self, token: &str) -> Result<Claims, AuthError> {
        if self.config.skips_verification() {
            return self.decode_without_verification(token);
        }

        let token_hash = Self::hash_token(token);

        if let Some(entry) = self.token_cache.get(&token_hash) {
            let (claims, expires_at) = entry.value();
            if std::time::Instant::now() < *expires_at {
                return Ok(claims.clone());
            }
            drop(entry);
            self.token_cache.remove(&token_hash);
        }

        let claims = if self.config.is_hmac() {
            self.validate_hmac(token)?
        } else {
            self.validate_rsa(token).await?
        };

        let cache_ttl = Self::cache_ttl(&claims);
        if cache_ttl > std::time::Duration::ZERO {
            self.token_cache.insert(
                token_hash,
                (claims.clone(), std::time::Instant::now() + cache_ttl),
            );
        }

        self.evict_expired_cache_entries();

        Ok(claims)
    }

    /// Hash a token to a u64 for cache key. Uses FxHash-style fast hashing.
    fn hash_token(token: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        token.hash(&mut hasher);
        hasher.finish()
    }

    /// Compute cache TTL as `min(exp - now, 60s)`.
    fn cache_ttl(claims: &Claims) -> std::time::Duration {
        const MAX_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);
        let exp = claims.exp();
        let now = chrono::Utc::now().timestamp();
        let remaining = if exp > now {
            std::time::Duration::from_secs((exp - now) as u64)
        } else {
            std::time::Duration::ZERO
        };
        remaining.min(MAX_CACHE_TTL)
    }

    /// Periodically evict expired entries to prevent unbounded growth.
    fn evict_expired_cache_entries(&self) {
        const MAX_CACHE_SIZE: usize = 10_000;
        if self.token_cache.len() > MAX_CACHE_SIZE {
            let now = std::time::Instant::now();
            self.token_cache
                .retain(|_, (_, expires_at)| *expires_at > now);
        }
    }

    /// Validate HMAC-signed token. Uses the token's `kid` header to look up
    /// the right key directly when present; falls back to trying every key
    /// when the kid is missing (external issuers) or unknown.
    fn validate_hmac(&self, token: &str) -> Result<Claims, AuthError> {
        let primary = self.hmac_key.as_ref().ok_or_else(|| {
            AuthError::InvalidToken("JWT secret not configured for HMAC".to_string())
        })?;

        let token_kid = jsonwebtoken::decode_header(token).ok().and_then(|h| h.kid);

        if let Some(tkid) = token_kid.as_deref() {
            if self.hmac_kid.as_deref() == Some(tkid) {
                return self.decode_and_validate(token, primary);
            }
            for (kid, key) in &self.legacy_hmac_keys {
                if kid == tkid {
                    return self.decode_and_validate(token, key);
                }
            }
            debug!(kid = %sanitize_for_log(tkid), "Token kid not recognised; falling back to full key scan");
        }

        match self.decode_and_validate(token, primary) {
            Ok(claims) => Ok(claims),
            Err(AuthError::InvalidToken(_)) if !self.legacy_hmac_keys.is_empty() => {
                for (_, key) in &self.legacy_hmac_keys {
                    if let Ok(claims) = self.decode_and_validate(token, key) {
                        return Ok(claims);
                    }
                }
                Err(AuthError::InvalidToken("Invalid signature".to_string()))
            }
            Err(e) => Err(e),
        }
    }

    /// Validate RSA-signed token using JWKS.
    async fn validate_rsa(&self, token: &str) -> Result<Claims, AuthError> {
        let jwks = self.config.jwks_client.as_ref().ok_or_else(|| {
            AuthError::InvalidToken("JWKS URL not configured for RSA".to_string())
        })?;

        // Extract key ID from token header
        let header = jsonwebtoken::decode_header(token)
            .map_err(|e| AuthError::InvalidToken(format!("Invalid token header: {}", e)))?;

        let safe_kid = header.kid.as_deref().map(sanitize_for_log);
        debug!(kid = ?safe_kid, alg = ?header.alg, "Validating RSA token");

        // Get key from JWKS
        let key = if let Some(kid) = header.kid {
            jwks.get_key(&kid).await.map_err(|e| {
                AuthError::InvalidToken(format!("Failed to get key '{}': {}", kid, e))
            })?
        } else if self.config.jwks_require_kid {
            return Err(AuthError::InvalidToken(
                "RS256 token missing kid header; set auth.jwks_require_kid = false to allow kidless tokens".to_string(),
            ));
        } else {
            jwks.get_any_key()
                .await
                .map_err(|e| AuthError::InvalidToken(format!("Failed to get JWKS key: {}", e)))?
        };

        self.decode_and_validate(token, &key)
    }

    /// Decode and validate token with the given key.
    fn decode_and_validate(&self, token: &str, key: &DecodingKey) -> Result<Claims, AuthError> {
        // Defense-in-depth: pre-check the header `alg` against the configured
        // algorithm before key selection. `jsonwebtoken` already enforces this
        // inside `decode`, but failing earlier with a clearer error keeps the
        // key-confusion class of attacks out of the validation path entirely.
        let header = jsonwebtoken::decode_header(token)
            .map_err(|e| AuthError::InvalidToken(format!("Invalid token header: {}", e)))?;
        let expected: jsonwebtoken::Algorithm = self.config.algorithm.into();
        if header.alg != expected {
            return Err(AuthError::InvalidToken(format!(
                "Token algorithm {:?} does not match configured {:?}",
                header.alg, expected
            )));
        }

        let mut validation = Validation::new(self.config.algorithm.into());

        // Configure validation
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.leeway = self.config.leeway_secs;

        // Require configured spec claims (defaults: exp, sub)
        let required: Vec<&str> = self
            .config
            .required_claims
            .iter()
            .map(String::as_str)
            .collect();
        validation.set_required_spec_claims(&required);

        // Validate issuer if configured
        if let Some(ref issuer) = self.config.issuer {
            validation.set_issuer(&[issuer]);
        }

        // Validate audience if configured
        if let Some(ref audience) = self.config.audience {
            validation.set_audience(&[audience]);
        } else {
            validation.validate_aud = false;
        }

        let token_data =
            decode::<Claims>(token, key, &validation).map_err(|e| self.map_jwt_error(e))?;

        Ok(token_data.claims)
    }

    /// Map jsonwebtoken errors to AuthError.
    fn map_jwt_error(&self, e: jsonwebtoken::errors::Error) -> AuthError {
        match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
            jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                AuthError::InvalidToken("Invalid signature".to_string())
            }
            jsonwebtoken::errors::ErrorKind::InvalidToken => {
                AuthError::InvalidToken("Invalid token format".to_string())
            }
            jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(claim) => {
                AuthError::InvalidToken(format!("Missing required claim: {}", claim))
            }
            jsonwebtoken::errors::ErrorKind::InvalidIssuer => {
                AuthError::InvalidToken("Invalid issuer".to_string())
            }
            jsonwebtoken::errors::ErrorKind::InvalidAudience => {
                AuthError::InvalidToken("Invalid audience".to_string())
            }
            _ => AuthError::InvalidToken(e.to_string()),
        }
    }

    /// Decode JWT token without signature verification (DEV MODE ONLY).
    fn decode_without_verification(&self, token: &str) -> Result<Claims, AuthError> {
        let token_data =
            dangerous::insecure_decode::<Claims>(token).map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::InvalidToken => {
                    AuthError::InvalidToken("Invalid token format".to_string())
                }
                _ => AuthError::InvalidToken(e.to_string()),
            })?;

        // Still check expiration in dev mode
        if token_data.claims.is_expired() {
            return Err(AuthError::TokenExpired);
        }

        Ok(token_data.claims)
    }
}

/// Authentication errors.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthError {
    #[error("Missing authorization header")]
    MissingHeader,
    #[error("Invalid authorization header format")]
    InvalidHeader,
    #[error("Invalid token: {0}")]
    InvalidToken(String),
    #[error("Token expired")]
    TokenExpired,
}

/// Pull client IP + user-agent from a request for auth-failure signal emission.
fn extract_auth_diag(req: &Request<Body>) -> (Option<String>, Option<String>) {
    let ip = req
        .extensions()
        .get::<crate::gateway::ResolvedClientIp>()
        .and_then(|r| r.0.clone());
    let ua = crate::gateway::extract_header(req.headers(), "user-agent");
    (ip, ua)
}

/// Emit a diagnostic signal event for an authentication failure. Used by
/// dashboards to monitor attack patterns and by operators to debug client
/// token issues.
fn emit_auth_failure(
    reason: &str,
    detail: &str,
    path: &str,
    client_ip: Option<String>,
    user_agent: Option<String>,
) {
    let is_bot = crate::signals::bot::is_bot(user_agent.as_deref());
    crate::signals::emit_diagnostic(
        "auth.failed",
        serde_json::json!({
            "reason": reason,
            "detail": detail,
            "path": path,
        }),
        client_ip,
        user_agent,
        None,
        None,
        is_bot,
    );
}

/// Extract token from request headers.
pub fn extract_token(req: &Request<Body>) -> Result<Option<String>, AuthError> {
    let Some(header_value) = req.headers().get(axum::http::header::AUTHORIZATION) else {
        return Ok(None);
    };

    let header = header_value
        .to_str()
        .map_err(|_| AuthError::InvalidHeader)?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or(AuthError::InvalidHeader)?
        .trim();

    if token.is_empty() {
        return Err(AuthError::InvalidHeader);
    }

    Ok(Some(token.to_string()))
}

/// Extract auth context from token (async, supports both HMAC and RSA/JWKS).
pub async fn extract_auth_context_async(
    token: Option<String>,
    middleware: &AuthMiddleware,
) -> Result<AuthContext, AuthError> {
    match token {
        Some(token) => middleware
            .validate_token_async(&token)
            .await
            .map(build_auth_context_from_claims),
        None => Ok(AuthContext::unauthenticated()),
    }
}

/// Build auth context from validated claims.
///
/// This handles both UUID and non-UUID subjects properly:
/// - UUID subjects: uses `authenticated()` with the parsed UUID
/// - Non-UUID subjects: uses `authenticated_without_uuid()` and stores raw subject in claims
pub fn build_auth_context_from_claims(claims: Claims) -> AuthContext {
    // Capture exp before moving claims — needed for SSE session expiry checks.
    let exp = claims.exp();

    // Try to parse subject as UUID first (before moving claims)
    let user_id = claims.user_id();

    // Build custom claims with raw subject included, filtering out reserved JWT claims
    let mut custom_claims = claims.sanitized_custom();
    let sub = claims.sub().to_string();
    let roles = claims.into_roles();
    custom_claims.insert("sub".to_string(), serde_json::Value::String(sub));

    let ctx = match user_id {
        Some(uuid) => {
            // Subject is a valid UUID
            AuthContext::authenticated(uuid, roles, custom_claims)
        }
        None => {
            // Subject is not a UUID (e.g., Firebase uid, Clerk user_xxx, email)
            // Still authenticated, but user_id() will return None
            AuthContext::authenticated_without_uuid(roles, custom_claims)
        }
    };

    ctx.with_token_exp(exp)
}

/// Authentication middleware function.
pub async fn auth_middleware(
    State(middleware): State<Arc<AuthMiddleware>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let token = match extract_token(&req) {
        Ok(token) => token,
        Err(e) => {
            let (ip, ua) = extract_auth_diag(&req);
            tracing::warn!(error = %e, "Invalid authorization header");
            emit_auth_failure("invalid_header", &e.to_string(), req.uri().path(), ip, ua);
            return super::response::RpcResponse::error(super::response::RpcError::unauthorized(
                "Invalid authorization header",
            ))
            .into_response();
        }
    };
    tracing::trace!(
        token_present = token.is_some(),
        "Auth middleware processing request"
    );

    let auth_context = match extract_auth_context_async(token, &middleware).await {
        Ok(auth_context) => auth_context,
        Err(e) => {
            let (ip, ua) = extract_auth_diag(&req);
            let reason = match &e {
                AuthError::TokenExpired => "token_expired",
                AuthError::InvalidToken(_) => "invalid_token",
                AuthError::MissingHeader => "missing_token",
                AuthError::InvalidHeader => "invalid_header",
            };
            tracing::warn!(error = %e, "Token validation failed");
            emit_auth_failure(reason, &e.to_string(), req.uri().path(), ip, ua);
            return super::response::RpcResponse::error(super::response::RpcError::unauthorized(
                "Invalid authentication token",
            ))
            .into_response();
        }
    };
    tracing::trace!(
        authenticated = auth_context.is_authenticated(),
        "Auth context created"
    );

    // Set OAuth session cookie when user is authenticated and HMAC secret
    // is available. This identifies the user on the OAuth authorize page
    // (same backend origin) without needing cross-origin localStorage.
    // Requires CORS Access-Control-Allow-Credentials: true and frontend
    // fetch with credentials: 'include' for the Set-Cookie to stick.
    let should_set_cookie =
        auth_context.is_authenticated() && middleware.config.jwt_secret.is_some();

    // Skip cookie if one already exists (avoids resigning on every request)
    let has_session_cookie = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|c| c.contains("forge_session="))
        .unwrap_or(false);

    let should_set_cookie = should_set_cookie && !has_session_cookie;

    let cookie_ip = req
        .extensions()
        .get::<crate::gateway::ResolvedClientIp>()
        .and_then(|r| r.0.clone());
    let cookie_ua = crate::gateway::extract_header(req.headers(), "user-agent");

    let mut req = req;
    req.extensions_mut().insert(auth_context.clone());

    let mut response = next.run(req).await;

    if should_set_cookie
        && let Some(subject) = auth_context.subject()
        && let Some(secret) = &middleware.config.jwt_secret
    {
        let cookie_ttl = middleware.config.session_cookie_ttl_secs;
        let cookie_value = sign_session_cookie(
            subject,
            secret,
            cookie_ttl,
            cookie_ip.as_deref(),
            cookie_ua.as_deref(),
        );
        // Always emit `Secure`. We previously inferred TLS from
        // `x-forwarded-proto`, but that header is trivially spoofable when the
        // gateway is exposed without a trusted reverse proxy in front, so a
        // plain-HTTP attacker on the same network could downgrade the cookie.
        // OAuth session cookies are only meaningful over HTTPS in production —
        // browsers refuse to send `Secure` cookies over HTTP, which surfaces
        // misconfigured deployments as a clean failure rather than silently
        // weakening the session.
        let cookie = format!(
            "forge_session={cookie_value}; Path=/_api/oauth/; HttpOnly; SameSite=Lax; Secure; Max-Age={cookie_ttl}"
        );
        if let Ok(val) = axum::http::HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, val);
        }
    }

    response
}

/// Coarsen an IP address to /24 (IPv4) or /48 (IPv6) for cookie binding.
/// Returns a stable prefix that survives minor NAT/proxy changes.
fn coarsen_ip(ip: &str) -> String {
    if let Ok(addr) = ip.parse::<std::net::IpAddr>() {
        match addr {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                format!(
                    "{}.{}.{}",
                    o.first().copied().unwrap_or(0),
                    o.get(1).copied().unwrap_or(0),
                    o.get(2).copied().unwrap_or(0),
                )
            }
            std::net::IpAddr::V6(v6) => {
                let s = v6.segments();
                format!(
                    "{:x}:{:x}:{:x}",
                    s.first().copied().unwrap_or(0),
                    s.get(1).copied().unwrap_or(0),
                    s.get(2).copied().unwrap_or(0),
                )
            }
        }
    } else {
        String::new()
    }
}

/// Hash a user-agent string for cookie binding (truncated hex).
fn hash_ua(ua: &str) -> String {
    let hash = Sha256::digest(ua.as_bytes());
    let bytes = hash.as_slice();
    let (a, b, c, d) = (
        bytes.first().copied().unwrap_or(0),
        bytes.get(1).copied().unwrap_or(0),
        bytes.get(2).copied().unwrap_or(0),
        bytes.get(3).copied().unwrap_or(0),
    );
    format!("{a:x}{b:x}{c:x}{d:x}")
}

/// OAuth session cookie format: `base64(subject):expiry_unix.hmac_signature`
/// The cookie identifies a user for the OAuth consent page without requiring
/// localStorage (which doesn't work cross-origin in dev).
///
/// Subject is base64-encoded to avoid delimiter collisions with subjects
/// containing `.` or `:` (e.g. external provider IDs from Firebase/Clerk).
///
/// The HMAC covers the client's coarsened IP (/24 or /48) and a UA hash so a
/// stolen cookie cannot be replayed from a different network or browser.
pub fn sign_session_cookie(
    subject: &str,
    secret: &str,
    ttl_secs: i64,
    client_ip: Option<&str>,
    user_agent: Option<&str>,
) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let expiry = chrono::Utc::now().timestamp() + ttl_secs;
    let ip_prefix = client_ip.map(coarsen_ip).unwrap_or_default();
    let ua_hash = user_agent.map(hash_ua).unwrap_or_default();
    let encoded_subject = URL_SAFE_NO_PAD.encode(subject.as_bytes());
    let payload = format!("{encoded_subject}:{expiry}");
    let binding = format!("{payload}.{ip_prefix}.{ua_hash}");

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(binding.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    format!("{payload}.{sig}")
}

/// Verify and extract the subject from a session cookie.
/// Returns None if expired, tampered, binding mismatch, or malformed.
///
/// Only used by the OAuth flow; gated behind `mcp-oauth`.
#[cfg(feature = "mcp-oauth")]
pub fn verify_session_cookie(
    cookie_value: &str,
    secret: &str,
    client_ip: Option<&str>,
    user_agent: Option<&str>,
) -> Option<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // Format: "base64(subject):expiry.signature"
    let (payload, sig_encoded) = cookie_value.rsplit_once('.')?;

    // Recompute binding with the current client's IP and UA
    let ip_prefix = client_ip.map(coarsen_ip).unwrap_or_default();
    let ua_hash = user_agent.map(hash_ua).unwrap_or_default();
    let binding = format!("{payload}.{ip_prefix}.{ua_hash}");

    // Verify signature (HMAC verify_slice is constant-time)
    let sig_bytes = URL_SAFE_NO_PAD.decode(sig_encoded).ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(binding.as_bytes());
    mac.verify_slice(&sig_bytes).ok()?;

    // Extract subject and expiry from "base64(subject):expiry"
    let (encoded_subject, expiry_str) = payload.rsplit_once(':')?;
    let expiry: i64 = expiry_str.parse().ok()?;

    if chrono::Utc::now().timestamp() > expiry {
        return None;
    }

    let subject_bytes = URL_SAFE_NO_PAD.decode(encoded_subject).ok()?;
    let subject = String::from_utf8(subject_bytes).ok()?;

    Some(subject)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    #[cfg(feature = "mcp-oauth")]
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    #[cfg(feature = "mcp-oauth")]
    use hmac::{Hmac, Mac};
    use jsonwebtoken::{EncodingKey, Header, encode};
    #[cfg(feature = "mcp-oauth")]
    use sha2::Sha256;

    fn create_test_claims(expired: bool) -> Claims {
        use forge_core::auth::ClaimsBuilder;

        let mut builder = ClaimsBuilder::new().subject("test-user-id").role("user");

        if expired {
            builder = builder.duration_secs(-3600); // Expired 1 hour ago
        } else {
            builder = builder.duration_secs(3600); // Valid for 1 hour
        }

        builder.build().unwrap()
    }

    fn create_test_token(claims: &Claims, secret: &str) -> String {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[cfg(feature = "mcp-oauth")]
    fn session_cookie_with_expiry(subject: &str, secret: &str, expiry: i64) -> String {
        let encoded_subject = URL_SAFE_NO_PAD.encode(subject.as_bytes());
        let payload = format!("{encoded_subject}:{expiry}");
        let ip_prefix = coarsen_ip("192.168.1.42");
        let ua_hash = hash_ua("TestAgent/1.0");
        let binding = format!("{payload}.{ip_prefix}.{ua_hash}");
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
        mac.update(binding.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{payload}.{sig}")
    }

    #[cfg(feature = "mcp-oauth")]
    #[test]
    fn test_coarsen_ip_masks_correctly() {
        assert_eq!(coarsen_ip("192.168.1.42"), "192.168.1");
        assert_eq!(coarsen_ip("10.0.0.1"), "10.0.0");
        assert_eq!(coarsen_ip("2001:db8:85a3::8a2e:370:7334"), "2001:db8:85a3");
        assert_eq!(coarsen_ip("not-an-ip"), "");
    }

    #[cfg(feature = "mcp-oauth")]
    #[test]
    fn test_hash_ua_deterministic() {
        let h1 = hash_ua("Mozilla/5.0");
        let h2 = hash_ua("Mozilla/5.0");
        assert_eq!(h1, h2);
        assert_ne!(hash_ua("Mozilla/5.0"), hash_ua("Chrome/100"));
    }

    #[test]
    fn sanitize_for_log_strips_control_chars_and_truncates() {
        assert_eq!(sanitize_for_log("normal-kid"), "normal-kid");
        assert_eq!(sanitize_for_log("\x1b[2K\rok"), "[2Kok");
        assert_eq!(sanitize_for_log("a\n\r\tb"), "ab");
        let long = "x".repeat(100);
        assert_eq!(sanitize_for_log(&long).len(), 64);
    }

    #[test]
    fn test_auth_config_default() {
        let config = AuthConfig::default();
        assert_eq!(config.algorithm, JwtAlgorithm::HS256);
        assert_eq!(config.mode, AuthMode::Production);
        assert!(!config.skips_verification());
    }

    #[test]
    fn test_auth_config_dev_mode() {
        let config = AuthConfig::dev_mode().expect("dev_mode outside production");
        assert_eq!(config.mode, AuthMode::Development);
        assert!(config.skips_verification());
    }

    #[test]
    fn test_auth_middleware_permissive() {
        let middleware = AuthMiddleware::permissive().expect("permissive outside production");
        assert!(middleware.config.skips_verification());
    }

    #[test]
    fn test_dev_mode_refuses_in_production() {
        let result = AuthConfig::dev_mode_with_env(DevModeEnv {
            forge_env: Some("production".into()),
            ..DevModeEnv::default()
        });
        assert!(matches!(result, Err(forge_core::ForgeError::Config { .. })));
    }

    #[test]
    fn test_dev_mode_refuses_case_insensitive() {
        for v in ["Production", "PRODUCTION", "production"] {
            let result = AuthConfig::dev_mode_with_env(DevModeEnv {
                forge_env: Some(v.into()),
                ..DevModeEnv::default()
            });
            assert!(matches!(result, Err(forge_core::ForgeError::Config { .. })));
        }
    }

    #[test]
    fn test_dev_mode_refuses_node_env_production() {
        let result = AuthConfig::dev_mode_with_env(DevModeEnv {
            node_env: Some("production".into()),
            ..DevModeEnv::default()
        });
        assert!(matches!(result, Err(forge_core::ForgeError::Config { .. })));
    }

    #[test]
    fn test_dev_mode_refuses_cloud_platform_indicators() {
        for (field, val) in [
            ("RAILWAY_ENVIRONMENT", "production"),
            ("K_SERVICE", "my-svc"),
            ("FLY_APP_NAME", "my-app"),
            ("KUBERNETES_SERVICE_HOST", "10.0.0.1"),
            ("AWS_EXECUTION_ENV", "AWS_ECS_FARGATE"),
        ] {
            let mut env = DevModeEnv::default();
            match field {
                "RAILWAY_ENVIRONMENT" => env.railway_environment = Some(val.into()),
                "K_SERVICE" => env.k_service = Some(val.into()),
                "FLY_APP_NAME" => env.fly_app_name = Some(val.into()),
                "KUBERNETES_SERVICE_HOST" => env.kubernetes_service_host = Some(val.into()),
                "AWS_EXECUTION_ENV" => env.aws_execution_env = Some(val.into()),
                _ => {}
            }
            let result = AuthConfig::dev_mode_with_env(env);
            assert!(
                matches!(result, Err(forge_core::ForgeError::Config { .. })),
                "{field} should block dev mode"
            );
        }
    }

    #[test]
    fn test_dev_mode_allows_other_env_values() {
        for forge_env in [None, Some("development"), Some("staging"), Some("")] {
            let result = AuthConfig::dev_mode_with_env(DevModeEnv {
                forge_env: forge_env.map(String::from),
                ..DevModeEnv::default()
            });
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_valid_token_with_correct_secret() {
        let secret = "test-secret-key";
        let config = AuthConfig::with_secret(secret);
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let token = create_test_token(&claims, secret);

        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_ok());
        let validated_claims = result.unwrap();
        assert_eq!(validated_claims.sub(), "test-user-id");
    }

    #[tokio::test]
    async fn test_valid_token_with_wrong_secret() {
        let config = AuthConfig::with_secret("correct-secret");
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let token = create_test_token(&claims, "wrong-secret");

        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_err());
        match result {
            Err(AuthError::InvalidToken(_)) => {}
            _ => panic!("Expected InvalidToken error"),
        }
    }

    #[tokio::test]
    async fn test_expired_token() {
        let secret = "test-secret";
        let config = AuthConfig::with_secret(secret);
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(true); // Expired
        let token = create_test_token(&claims, secret);

        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_err());
        match result {
            Err(AuthError::TokenExpired) => {}
            _ => panic!("Expected TokenExpired error"),
        }
    }

    #[tokio::test]
    async fn test_tampered_token() {
        let secret = "test-secret";
        let config = AuthConfig::with_secret(secret);
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let mut token = create_test_token(&claims, secret);

        // Tamper with the token by modifying a character in the signature
        if let Some(last_char) = token.pop() {
            let replacement = if last_char == 'a' { 'b' } else { 'a' };
            token.push(replacement);
        }

        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dev_mode_skips_signature() {
        let config = AuthConfig::dev_mode().expect("dev_mode outside production");
        let middleware = AuthMiddleware::new(config);

        // Create token with any secret
        let claims = create_test_claims(false);
        let token = create_test_token(&claims, "any-secret");

        // Should still validate in dev mode
        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_dev_mode_still_checks_expiration() {
        let config = AuthConfig::dev_mode().expect("dev_mode outside production");
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(true); // Expired
        let token = create_test_token(&claims, "any-secret");

        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_err());
        match result {
            Err(AuthError::TokenExpired) => {}
            _ => panic!("Expected TokenExpired error even in dev mode"),
        }
    }

    #[tokio::test]
    async fn test_invalid_token_format() {
        let config = AuthConfig::with_secret("secret");
        let middleware = AuthMiddleware::new(config);

        let result = middleware.validate_token_async("not-a-valid-jwt").await;
        assert!(result.is_err());
        match result {
            Err(AuthError::InvalidToken(_)) => {}
            _ => panic!("Expected InvalidToken error"),
        }
    }

    #[test]
    fn test_algorithm_conversion() {
        assert_eq!(Algorithm::from(JwtAlgorithm::HS256), Algorithm::HS256);
        assert_eq!(Algorithm::from(JwtAlgorithm::RS256), Algorithm::RS256);
    }

    #[test]
    fn test_is_hmac_and_is_rsa() {
        let hmac_config = AuthConfig::with_secret("test");
        assert!(hmac_config.is_hmac());
        assert!(!hmac_config.is_rsa());

        let rsa_config = AuthConfig {
            algorithm: JwtAlgorithm::RS256,
            ..Default::default()
        };
        assert!(!rsa_config.is_hmac());
        assert!(rsa_config.is_rsa());
    }

    #[test]
    fn test_extract_token_rejects_non_bearer_header() {
        let req = Request::builder()
            .header(axum::http::header::AUTHORIZATION, "Basic abc")
            .body(Body::empty())
            .unwrap();

        let result = extract_token(&req);
        assert!(matches!(result, Err(AuthError::InvalidHeader)));
    }

    #[test]
    fn test_build_auth_context_from_non_uuid_claims_preserves_subject() {
        let claims = Claims::builder()
            .subject("clerk_user_123")
            .role("member")
            .claim("tenant_id", serde_json::json!("tenant-1"))
            .unwrap()
            .build()
            .unwrap();

        let auth = build_auth_context_from_claims(claims);
        assert!(auth.is_authenticated());
        assert!(auth.user_id().is_none());
        assert_eq!(auth.subject(), Some("clerk_user_123"));
        assert_eq!(auth.principal_id(), Some("clerk_user_123".to_string()));
        assert!(auth.has_role("member"));
        assert_eq!(
            auth.claim("sub"),
            Some(&serde_json::json!("clerk_user_123"))
        );
    }

    #[cfg(feature = "mcp-oauth")]
    #[test]
    fn test_verify_session_cookie_round_trip_and_tamper_detection() {
        let ip = Some("192.168.1.42");
        let ua = Some("TestAgent/1.0");
        let cookie = sign_session_cookie("user-123", "session-secret", 86400, ip, ua);

        assert_eq!(
            verify_session_cookie(&cookie, "session-secret", ip, ua),
            Some("user-123".to_string())
        );

        let mut tampered = cookie.clone();
        if let Some(last_char) = tampered.pop() {
            tampered.push(if last_char == 'a' { 'b' } else { 'a' });
        }

        assert_eq!(
            verify_session_cookie(&tampered, "session-secret", ip, ua),
            None
        );
        assert_eq!(verify_session_cookie(&cookie, "wrong-secret", ip, ua), None);
    }

    #[cfg(feature = "mcp-oauth")]
    #[test]
    fn test_verify_session_cookie_rejects_expired_cookie() {
        let expired_cookie = session_cookie_with_expiry(
            "user-123",
            "session-secret",
            chrono::Utc::now().timestamp() - 1,
        );

        assert_eq!(
            verify_session_cookie(
                &expired_cookie,
                "session-secret",
                Some("192.168.1.42"),
                Some("TestAgent/1.0"),
            ),
            None
        );
    }

    #[cfg(feature = "mcp-oauth")]
    #[test]
    fn test_verify_session_cookie_rejects_binding_mismatch() {
        let ip = Some("192.168.1.42");
        let ua = Some("TestAgent/1.0");
        let cookie = sign_session_cookie("user-123", "session-secret", 86400, ip, ua);

        // Different IP
        assert_eq!(
            verify_session_cookie(&cookie, "session-secret", Some("10.0.0.1"), ua),
            None
        );

        // Different UA
        assert_eq!(
            verify_session_cookie(&cookie, "session-secret", ip, Some("OtherBrowser/2.0")),
            None
        );

        // No binding at all
        assert_eq!(
            verify_session_cookie(&cookie, "session-secret", None, None),
            None
        );
    }

    #[cfg(feature = "mcp-oauth")]
    #[test]
    fn test_session_cookie_round_trips_subject_with_dots() {
        let ip = Some("192.168.1.42");
        let ua = Some("TestAgent/1.0");
        let subject = "clerk.user.abc.123";
        let cookie = sign_session_cookie(subject, "session-secret", 86400, ip, ua);

        assert_eq!(
            verify_session_cookie(&cookie, "session-secret", ip, ua),
            Some(subject.to_string())
        );
    }

    #[tokio::test]
    async fn test_extract_auth_context_async_invalid_token_errors() {
        let middleware = AuthMiddleware::new(AuthConfig::with_secret("secret"));
        let result = extract_auth_context_async(Some("bad.token".to_string()), &middleware).await;
        assert!(matches!(result, Err(AuthError::InvalidToken(_))));
    }

    fn legacy_secret(
        secret: &str,
        valid_for: chrono::Duration,
    ) -> forge_core::config::LegacySecret {
        forge_core::config::LegacySecret {
            secret: secret.into(),
            valid_until: chrono::Utc::now() + valid_for,
        }
    }

    #[tokio::test]
    async fn test_legacy_secret_validates_token_signed_by_old_key() {
        let old_secret = "old-secret-key-32-bytes-minimum!!";
        let new_secret = "new-secret-key-32-bytes-minimum!!";

        let config = AuthConfig {
            jwt_secret: Some(new_secret.into()),
            legacy_secrets: vec![legacy_secret(old_secret, chrono::Duration::hours(1))],
            ..AuthConfig::with_secret(new_secret)
        };
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let token_from_old_key = create_test_token(&claims, old_secret);

        // Token signed with the old key must still validate during the grace period
        let result = middleware.validate_token_async(&token_from_old_key).await;
        assert!(
            result.is_ok(),
            "legacy-signed token should be accepted: {result:?}"
        );

        // Token signed with the new (active) key must also validate
        let token_from_new_key = create_test_token(&claims, new_secret);
        assert!(
            middleware
                .validate_token_async(&token_from_new_key)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_legacy_secret_still_rejects_unknown_key() {
        let config = AuthConfig {
            legacy_secrets: vec![legacy_secret(
                "known-legacy-secret-32bytes-pad!!",
                chrono::Duration::hours(1),
            )],
            ..AuthConfig::with_secret("active-secret-key-32-bytes-pad!!")
        };
        let middleware = AuthMiddleware::new(config);

        let claims = create_test_claims(false);
        let token = create_test_token(&claims, "totally-unknown-secret-32bytes!!");

        let result = middleware.validate_token_async(&token).await;
        assert!(matches!(result, Err(AuthError::InvalidToken(_))));
    }

    #[tokio::test]
    async fn test_expired_legacy_secret_is_dropped_at_construction() {
        let active_secret = "active-secret-key-32-bytes-pad!!";
        let retired_secret = "retired-secret-key-32-bytes-pad!!";

        // valid_until in the past — must be filtered out, leaving zero legacy keys
        let config = AuthConfig {
            legacy_secrets: vec![legacy_secret(retired_secret, -chrono::Duration::seconds(1))],
            ..AuthConfig::with_secret(active_secret)
        };
        let middleware = AuthMiddleware::new(config);
        assert!(
            middleware.legacy_hmac_keys.is_empty(),
            "expired legacy secret should be dropped at construction"
        );

        // Tokens signed with the retired key must now fail
        let claims = create_test_claims(false);
        let token = create_test_token(&claims, retired_secret);
        let result = middleware.validate_token_async(&token).await;
        assert!(
            matches!(result, Err(AuthError::InvalidToken(_))),
            "expired legacy-signed token must not validate, got: {result:?}"
        );
    }

    /// Craft a raw JWT with a given `alg` string in the header (no signature
    /// verification intended — the test exercises the pre-check, not decode).
    fn raw_jwt_with_alg(alg: &str) -> String {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

        let header = URL_SAFE_NO_PAD.encode(format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#));
        // Minimal payload with required exp/sub claims (well in the future)
        let exp = chrono::Utc::now().timestamp() + 3600;
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"test","exp":{exp},"iat":0}}"#));
        // Signature is garbage — the pre-check fires before signature verification.
        let sig = URL_SAFE_NO_PAD.encode(b"fakesignature");
        format!("{header}.{payload}.{sig}")
    }

    /// G3.10: The algorithm pre-check must reject a token whose header `alg`
    /// differs from the configured algorithm BEFORE attempting key selection or
    /// signature verification. This blocks the RS256→HS256 confusion attack
    /// where an attacker uses the RSA public key as an HMAC secret.
    #[tokio::test]
    async fn g3_10_jwt_algorithm_pre_check() {
        // Validator is configured for HS256 but the token header claims RS256.
        let middleware = AuthMiddleware::new(AuthConfig::with_secret(
            "test-secret-key-32-bytes-minimum!!",
        ));

        let token = raw_jwt_with_alg("RS256");
        let result = middleware.validate_token_async(&token).await;

        match result {
            Err(AuthError::InvalidToken(msg)) => {
                assert!(
                    msg.contains("does not match"),
                    "expected alg-mismatch message, got: {msg}"
                );
            }
            other => panic!("expected InvalidToken from alg pre-check, got: {other:?}"),
        }
    }

    /// G3.10 (alg=none): A token with `alg: none` must be rejected. The
    /// `JwtAlgorithm` enum has no `None` variant, so `decode_header` will fail
    /// to deserialize the algorithm string — the token never reaches the
    /// pre-check comparison, let alone signature verification.
    #[tokio::test]
    async fn g1_jwt_alg_none_rejected() {
        let middleware = AuthMiddleware::new(AuthConfig::with_secret(
            "test-secret-key-32-bytes-minimum!!",
        ));

        let token = raw_jwt_with_alg("none");
        let result = middleware.validate_token_async(&token).await;

        assert!(
            matches!(result, Err(AuthError::InvalidToken(_))),
            "alg=none token must be rejected, got: {result:?}"
        );
    }

    /// `secret_kid` must be deterministic: same input bytes produce the same
    /// kid every call (needed because issuer and verifier compute it
    /// independently from the same secret).
    #[test]
    fn test_secret_kid_is_deterministic() {
        let kid_a = secret_kid(b"some-secret");
        let kid_b = secret_kid(b"some-secret");
        assert_eq!(kid_a, kid_b);
        assert_eq!(kid_a.len(), 8, "kid should be 8 hex chars (4 bytes)");
        assert_ne!(kid_a, secret_kid(b"different-secret"));
    }

    /// `HmacTokenIssuer::sign` must stamp every minted token with a `kid`
    /// header derived from the active secret. Without it, the verifier
    /// cannot disambiguate active vs legacy keys during rotation.
    #[tokio::test]
    async fn test_issued_token_carries_kid_header() {
        use forge_core::TokenIssuer;
        let secret = "issuer-secret-key-32-bytes-pad!!!";
        let config = AuthConfig::with_secret(secret);
        let issuer = HmacTokenIssuer::from_config(&config).expect("issuer for hmac");

        let claims = create_test_claims(false);
        let token = issuer.sign(&claims).expect("signed token");

        let header = jsonwebtoken::decode_header(&token).expect("decodable header");
        assert_eq!(
            header.kid.as_deref(),
            Some(secret_kid(secret.as_bytes()).as_str()),
            "kid in header must match SHA-256 prefix of the secret"
        );
    }

    /// A token whose kid matches a legacy secret must validate by the direct
    /// kid lookup path — even if the active key would also fail signature
    /// verification (which it would, since the token was signed with the
    /// legacy secret). This proves the kid is actually being consulted.
    #[tokio::test]
    async fn test_kid_matched_legacy_token_validates() {
        let active_secret = "active-secret-key-32-bytes-pad!!!";
        let retired_secret = "legacy-secret-key-32-bytes-pad!!!";
        let retired_kid = secret_kid(retired_secret.as_bytes());

        let config = AuthConfig {
            legacy_secrets: vec![legacy_secret(retired_secret, chrono::Duration::hours(1))],
            ..AuthConfig::with_secret(active_secret)
        };
        let middleware = AuthMiddleware::new(config);

        // Hand-craft a token signed by the retired secret with the matching kid set
        let claims = create_test_claims(false);
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(retired_kid);
        let token = encode(
            &header,
            &claims,
            &EncodingKey::from_secret(retired_secret.as_bytes()),
        )
        .expect("encode legacy-signed token");

        let result = middleware.validate_token_async(&token).await;
        assert!(
            result.is_ok(),
            "kid-tagged legacy token must validate: {result:?}"
        );
    }

    /// External issuers (e.g., third-party services minting HS256 tokens) may
    /// not set a `kid`. Those tokens must still validate via the fallback
    /// scan — the kid is an optimization, not a gate.
    #[tokio::test]
    async fn test_external_token_without_kid_still_validates() {
        let secret = "shared-hmac-secret-32-bytes-pad!!";
        let middleware = AuthMiddleware::new(AuthConfig::with_secret(secret));

        // create_test_token uses Header::default() which leaves kid = None
        let claims = create_test_claims(false);
        let token = create_test_token(&claims, secret);
        let header = jsonwebtoken::decode_header(&token).expect("decodable header");
        assert!(header.kid.is_none(), "test fixture must not set kid");

        let result = middleware.validate_token_async(&token).await;
        assert!(
            result.is_ok(),
            "kidless external token must validate: {result:?}"
        );
    }

    /// A token whose kid matches no configured key must still fall through to
    /// the full signature scan rather than being rejected solely on kid.
    #[tokio::test]
    async fn test_unknown_kid_falls_back_to_full_scan() {
        let secret = "active-secret-key-32-bytes-pad!!!";
        let middleware = AuthMiddleware::new(AuthConfig::with_secret(secret));

        let claims = create_test_claims(false);
        // Sign with the active secret but stamp a kid that doesn't match any key
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("deadbeef".to_string());
        let token = encode(
            &header,
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode token with unknown kid");

        let result = middleware.validate_token_async(&token).await;
        assert!(
            result.is_ok(),
            "unknown-kid token must still validate via fallback: {result:?}"
        );
    }

    #[tokio::test]
    async fn rsa_token_without_kid_rejected_when_jwks_require_kid_is_true() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

        let config = AuthConfig {
            algorithm: JwtAlgorithm::RS256,
            jwks_client: Some(Arc::new(
                JwksClient::new("http://example.invalid".into(), 3600).unwrap(),
            )),
            jwks_require_kid: true,
            ..AuthConfig::default()
        };
        let middleware = AuthMiddleware::new(config);

        // Build a minimal JWT with RS256 alg and no kid in the header.
        // The kid check fires before signature verification, so the sig is irrelevant.
        let header_json = r#"{"alg":"RS256","typ":"JWT"}"#;
        let claims = create_test_claims(false);
        let claims_json = serde_json::to_string(&claims).unwrap();
        let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims_json.as_bytes());
        let token = format!("{header_b64}.{claims_b64}.fake-signature");

        let result = middleware.validate_token_async(&token).await;
        assert!(result.is_err(), "kidless RS256 token must be rejected");
        let err = result.unwrap_err();
        match &err {
            AuthError::InvalidToken(msg) => {
                assert!(
                    msg.contains("missing kid"),
                    "error should mention missing kid, got: {msg}"
                );
            }
            other => panic!("expected InvalidToken, got: {other:?}"),
        }
    }
}

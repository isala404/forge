use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, SystemTime};

/// Max password plaintext (argon2 input cap; blocks DoS via huge inputs).
pub const MAX_PASSWORD_BYTES: usize = 4096;
/// Max PHC hash string accepted by `verify_password`/`needs_rehash`.
pub const MAX_PHC_BYTES: usize = 1024;
/// Max `user_id`/`owner_id` length.
pub const MAX_ID_BYTES: usize = 255;
/// Max API-key `label` length.
pub const MAX_LABEL_BYTES: usize = 255;
/// Max one-time-token `purpose` length.
pub const MAX_PURPOSE_BYTES: usize = 255;
/// Max opaque one-time-token payload.
pub const MAX_TOKEN_PAYLOAD_BYTES: usize = 4096;
/// Max API-key scopes and metadata serialized size.
pub const MAX_API_KEY_SCOPES: usize = 32;
pub const MAX_API_KEY_METADATA_BYTES: usize = 4096;

/// A PHC-format password hash (`$argon2id$v=19$...`). Portable to/from the wider
/// `password_hash` ecosystem. `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct PhcString(String);

impl PhcString {
    /// Wrap an existing PHC string (e.g. one loaded from your users table).
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The PHC string, to persist in your users table.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PhcString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PhcString(<redacted>)")
    }
}

/// An opaque session token (≥ 256-bit random). The plaintext exists once; only its
/// SHA-256 is stored. `Debug` is redacted.
#[derive(Clone)]
pub struct SessionToken(String);

impl SessionToken {
    /// Wrap a freshly minted token. For backend implementors; app code receives this
    /// from [`Auth::create_session`].
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The raw token, to hand to the client exactly once.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// A single-use, purpose-scoped token (≥ 256-bit random) for password reset, email
/// verification, magic links. The plaintext exists once; only its SHA-256 is stored.
/// `Debug` is redacted.
#[derive(Clone)]
pub struct OneTimeToken(String);

impl OneTimeToken {
    /// Wrap a freshly minted token. For backend implementors; app code receives this
    /// from [`Auth::create_token`].
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The raw token, to deliver to the user exactly once (e.g. in an email link).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OneTimeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OneTimeToken(<redacted>)")
    }
}

/// An API key secret (`fk_...`). Shown exactly once at creation; only its SHA-256 is
/// stored. `Debug` is redacted.
#[derive(Clone)]
pub struct ApiKeySecret(String);

impl ApiKeySecret {
    /// Wrap a freshly minted `fk_...` secret. For backend implementors; app code
    /// receives this from [`Auth::create_api_key`].
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The raw `fk_...` key, to hand to the user exactly once.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKeySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKeySecret(<redacted>)")
    }
}

/// Session timeouts (OWASP terms). Both always applied.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct SessionOpts {
    /// Sliding idle timeout, refreshed on each successful validate. Default 30 min.
    pub idle_timeout: Duration,
    /// Hard ceiling from creation; never extended. Default 12 h.
    pub absolute_timeout: Duration,
}

impl Default for SessionOpts {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(30 * 60),
            absolute_timeout: Duration::from_secs(12 * 60 * 60),
        }
    }
}

impl SessionOpts {
    /// Default options (30 min idle, 12 h absolute).
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    /// Set the absolute timeout (hard ceiling from creation).
    pub fn with_absolute_timeout(mut self, d: Duration) -> Self {
        self.absolute_timeout = d;
        self
    }
}

/// A live session returned by [`Auth::validate_session`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Session {
    /// The app-owned user id this session belongs to.
    pub user_id: String,
    /// When the session was created (seconds precision).
    pub created_at: SystemTime,
    /// Effective deadline at validate time: `min(now + idle, created + absolute)`.
    pub expires_at: SystemTime,
}

impl Session {
    /// Construct a live session. For backend implementors; app code receives this from
    /// [`Auth::validate_session`].
    pub fn new(user_id: String, created_at: SystemTime, expires_at: SystemTime) -> Self {
        Self {
            user_id,
            created_at,
            expires_at,
        }
    }
}

/// A freshly created API key. The `secret` is shown exactly once.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ApiKey {
    /// Stable, non-secret id (safe to store/log; used for revocation).
    pub id: String,
    /// The label given at creation.
    pub label: String,
    /// The full `fk_...` secret; capture it now, it is never recoverable.
    pub secret: ApiKeySecret,
    /// When the key was created.
    pub created_at: SystemTime,
    pub expires_at: Option<SystemTime>,
    pub scopes: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl ApiKey {
    /// Construct a freshly created API key. For backend implementors; app code receives
    /// this from [`Auth::create_api_key`].
    pub fn new(
        id: String,
        label: String,
        secret: ApiKeySecret,
        created_at: SystemTime,
        expires_at: Option<SystemTime>,
        scopes: Vec<String>,
        metadata: HashMap<String, String>,
    ) -> Self {
        Self {
            id,
            label,
            secret,
            created_at,
            expires_at,
            scopes,
            metadata,
        }
    }
}

/// Non-secret API-key metadata returned by [`Auth::verify_api_key`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ApiKeyInfo {
    /// Stable, non-secret id.
    pub id: String,
    /// The app-owned owner id.
    pub owner_id: String,
    /// The key's label.
    pub label: String,
    pub expires_at: Option<SystemTime>,
    pub scopes: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl ApiKeyInfo {
    /// Construct API-key metadata. For backend implementors; app code receives this
    /// from [`Auth::verify_api_key`].
    pub fn new(
        id: String,
        owner_id: String,
        label: String,
        expires_at: Option<SystemTime>,
        scopes: Vec<String>,
        metadata: HashMap<String, String>,
    ) -> Self {
        Self {
            id,
            owner_id,
            label,
            expires_at,
            scopes,
            metadata,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct ApiKeyOpts {
    pub expires_in: Option<Duration>,
    pub scopes: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl ApiKeyOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_expires_in(mut self, expires_in: Duration) -> Self {
        self.expires_in = Some(expires_in);
        self
    }

    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = scopes;
        self
    }

    pub fn with_metadata(mut self, metadata: HashMap<String, String>) -> Self {
        self.metadata = metadata;
        self
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenConsumption {
    pub user_id: String,
    pub payload: Bytes,
}

/// Auth primitives: argon2id passwords, opaque hashed sessions, `fk_` API keys, and
/// single-use one-time tokens. Object-safe; the facade hands out `Arc<dyn Auth>`.
///
/// Forge does NOT own the users table; `user_id`/`owner_id` are opaque app strings.
/// Exact semantics, timeouts, and error mapping: <https://tryforge.dev/primitives/#auth>.
#[async_trait]
pub trait Auth: Send + Sync {
    /// Hash a password with argon2id at Forge-owned current params (fresh salt).
    async fn hash_password(&self, plain: &str) -> Result<PhcString>;

    /// Constant-time verify. `Ok(true)`/`Ok(false)`; a malformed `hash` is
    /// [`crate::error::ForgeError::Invalid`], never `Ok(false)`.
    async fn verify_password(&self, plain: &str, hash: &PhcString) -> Result<bool>;

    /// `true` if `hash` is below Forge-current params (call after a successful verify
    /// and re-hash). A malformed hash returns `true` (rehash it).
    fn needs_rehash(&self, hash: &PhcString) -> bool;

    /// Mint a session, storing only its SHA-256 with idle + absolute deadlines.
    async fn create_session(&self, user_id: &str, opts: SessionOpts) -> Result<SessionToken>;

    /// Validate by token hash; on success slide the idle deadline. Unknown/expired/
    /// revoked => `Ok(None)`, never an error.
    async fn validate_session(&self, token: &str) -> Result<Option<Session>>;

    /// Revoke a session by token. Idempotent.
    async fn revoke_session(&self, token: &str) -> Result<()>;

    /// Revoke every session for `user_id`. Returns the count revoked.
    async fn revoke_all_sessions(&self, user_id: &str) -> Result<u64>;

    /// Mint an `fk_`-prefixed API key, storing only its SHA-256. Secret shown once.
    async fn create_api_key(&self, owner_id: &str, label: &str) -> Result<ApiKey>;

    async fn create_api_key_with(
        &self,
        owner_id: &str,
        label: &str,
        opts: ApiKeyOpts,
    ) -> Result<ApiKey>;

    /// Verify a key by hash. `Some(ApiKeyInfo)` if live, else `Ok(None)`.
    async fn verify_api_key(&self, key: &str) -> Result<Option<ApiKeyInfo>>;

    /// Revoke a key by id. `Ok(true)` if removed, `Ok(false)` if unknown.
    async fn revoke_api_key(&self, key_id: &str) -> Result<bool>;

    /// Mint a single-use token scoped to `purpose` (e.g. `"password-reset"`), storing
    /// only its SHA-256 with a hard expiry. Deliver it out of band (email link, SMS);
    /// Forge does not send anything. Custom auth backends that do not support one-time
    /// tokens receive a non-retryable backend error by default.
    async fn create_token(
        &self,
        _user_id: &str,
        _purpose: &str,
        _ttl: Duration,
    ) -> Result<OneTimeToken> {
        Err(ForgeError::backend(
            "one-time tokens are not supported by this auth backend",
        ))
    }

    async fn create_token_with_payload(
        &self,
        user_id: &str,
        purpose: &str,
        ttl: Duration,
        payload: Bytes,
    ) -> Result<OneTimeToken>;

    /// Atomically consume a token minted for `purpose`: delete it and return its
    /// `user_id`. Unknown/expired/already-consumed => `Ok(None)`, never an error.
    /// A live token presented with the wrong `purpose` is left intact. Custom auth
    /// backends that do not support one-time tokens receive a non-retryable backend
    /// error by default.
    async fn consume_token(&self, _token: &str, _purpose: &str) -> Result<Option<String>> {
        Err(ForgeError::backend(
            "one-time tokens are not supported by this auth backend",
        ))
    }

    async fn consume_token_with_payload(
        &self,
        token: &str,
        purpose: &str,
    ) -> Result<Option<TokenConsumption>>;
}

mod memory;
mod postgres;
pub(crate) use memory::MemAuth;
pub(crate) use postgres::PgAuth;

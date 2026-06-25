//! In-process `auth` backend. Contract: docs/contracts/auth.md.
//!
//! Passwords are stateless: [`MemAuth`] hashes and verifies argon2id PHC strings with
//! the *same* Forge-pinned parameters as [`super::PgAuth`] (Forge never owns the users
//! table), so a hash minted here verifies under Postgres and vice versa. Sessions and
//! API keys live in `Mutex<HashMap>`s keyed by the SHA-256 of the secret — only digests
//! are ever stored, exactly as the SQL backend stores them. The observable contract
//! matches [`super::PgAuth`]; only the storage differs — there is no SQL, and nothing
//! survives a restart.

use super::{
    ApiKey, ApiKeyInfo, ApiKeySecret, Auth, MAX_ID_BYTES, MAX_LABEL_BYTES, MAX_PASSWORD_BYTES,
    MAX_PHC_BYTES, PhcString, Session, SessionOpts, SessionToken,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::{ForgeError, Result};
use crate::util::{hex, namespaced, sha256_hex};
use argon2::password_hash::SaltString;
use argon2::{Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use rand_core::{OsRng, RngCore};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

// Forge-owned argon2id parameters. Deliberately duplicated from `super::PgAuth`'s pinned
// constants (the helpers there are module-private and unreachable from here) and kept
// byte-for-byte identical, so both backends hash at the same cost and agree on
// `needs_rehash`. A divergence here would silently mark every Postgres-minted hash as
// stale (or vice versa) the moment an app moved between backends.
const ARGON2_M_COST: u32 = 19_456; // KiB (19 MiB)
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;

/// An argon2id hasher built from Forge's pinned parameters.
fn forge_argon2() -> Result<Argon2<'static>> {
    let params = Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, None)
        .map_err(|e| ForgeError::backend(format!("invalid argon2 params: {e}")))?;
    Ok(Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        params,
    ))
}

/// Random token entropy: 32 bytes = 256 bits.
const TOKEN_BYTES: usize = 32;
/// Absolute-timeout ceiling (~10 years). Over => `Limit`.
const MAX_ABSOLUTE_SECS: f64 = 10.0 * 365.0 * 24.0 * 60.0 * 60.0;
/// API-key prefix (Stripe/GitHub convention), for greppability + secret scanning.
const KEY_PREFIX: &str = "fk_";

/// A fresh 256-bit random token rendered as hex.
fn random_token() -> String {
    let mut buf = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut buf);
    hex(&buf)
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(ForgeError::invalid("user/owner id must not be empty"));
    }
    if id.len() > MAX_ID_BYTES {
        return Err(ForgeError::limit(format!(
            "id is {} bytes; max is {MAX_ID_BYTES}",
            id.len()
        )));
    }
    Ok(())
}

fn check_label(label: &str) -> Result<()> {
    if label.len() > MAX_LABEL_BYTES {
        return Err(ForgeError::limit(format!(
            "label is {} bytes; max is {MAX_LABEL_BYTES}",
            label.len()
        )));
    }
    Ok(())
}

fn check_password(plain: &str) -> Result<()> {
    if plain.is_empty() {
        return Err(ForgeError::invalid("password must not be empty"));
    }
    if plain.len() > MAX_PASSWORD_BYTES {
        return Err(ForgeError::limit(format!(
            "password is {} bytes; max is {MAX_PASSWORD_BYTES}",
            plain.len()
        )));
    }
    Ok(())
}

fn check_session_opts(opts: &SessionOpts) -> Result<()> {
    if opts.idle_timeout.is_zero() || opts.absolute_timeout.is_zero() {
        return Err(ForgeError::invalid("session timeouts must be positive"));
    }
    if opts.idle_timeout > opts.absolute_timeout {
        return Err(ForgeError::invalid(
            "idle_timeout must be <= absolute_timeout",
        ));
    }
    if opts.absolute_timeout.as_secs_f64() > MAX_ABSOLUTE_SECS {
        return Err(ForgeError::limit("absolute_timeout exceeds the maximum"));
    }
    Ok(())
}

/// Take a map lock, recovering the guard if a previous holder panicked. The critical
/// sections are short and synchronous (no `await` held across the lock), so a poisoned
/// lock never reflects a half-updated invariant worth aborting for.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A stored session. Only the token's SHA-256 is the map key; the plaintext is never kept.
struct SessionRecord {
    user_id: String,
    created_at: SystemTime,
    /// The configured sliding window, re-applied on each successful validate.
    idle: Duration,
    /// Sliding idle deadline; advanced on validate, capped at `abs_deadline`.
    idle_deadline: SystemTime,
    /// Hard ceiling from creation; never extended.
    abs_deadline: SystemTime,
}

impl SessionRecord {
    /// Live iff both deadlines are still in the future, matching `PgAuth`'s
    /// `idle_deadline > now() AND abs_deadline > now()`.
    fn is_live(&self, now: SystemTime) -> bool {
        self.idle_deadline > now && self.abs_deadline > now
    }
}

/// A stored API key. The map key is the secret's SHA-256; the secret itself is never kept.
struct ApiKeyRecord {
    id: String,
    owner_id: String,
    label: String,
}

/// In-process [`Auth`]. Passwords are stateless; sessions and API keys are held as
/// digest-keyed maps. Not durable.
pub(crate) struct MemAuth {
    sessions: Mutex<HashMap<String, SessionRecord>>,
    api_keys: Mutex<HashMap<String, ApiKeyRecord>>,
    /// App namespace, the in-process analog of `PgAuth`'s `app` column: it scopes every
    /// stored digest so an app sharing a process can neither validate nor revoke another
    /// app's sessions or keys. Empty = the unnamespaced app.
    namespace: String,
}

impl MemAuth {
    pub(crate) fn new(namespace: String) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            api_keys: Mutex::new(HashMap::new()),
            namespace,
        }
    }

    /// Scope a digest to this app with the same `<namespace>:<key>` rule the SQL backends
    /// share, so two apps in one process never collide on the same secret digest.
    fn physical(&self, digest: &str) -> String {
        namespaced(&self.namespace, digest)
    }

    /// Drop expired sessions, mirroring `PgAuth::sweep`. Reads already treat them as
    /// absent; this reclaims the memory. Returns how many were purged. API keys never
    /// expire, so they are untouched.
    pub(crate) fn purge_expired(&self) -> u64 {
        let now = SystemTime::now();
        let mut sessions = lock(&self.sessions);
        let before = sessions.len();
        sessions.retain(|_, rec| rec.is_live(now));
        (before - sessions.len()) as u64
    }
}

#[async_trait]
impl Auth for MemAuth {
    async fn hash_password(&self, plain: &str) -> Result<PhcString> {
        check_password(plain)?;
        let owned = plain.to_string();
        let argon = forge_argon2()?;
        // argon2 is CPU-heavy; keep it off the async runtime threads.
        let phc = tokio::task::spawn_blocking(move || {
            let salt = SaltString::generate(&mut OsRng);
            argon
                .hash_password(owned.as_bytes(), &salt)
                .map(|h| h.to_string())
        })
        .await
        .map_err(|e| ForgeError::backend(format!("hash task failed: {e}")))?
        .map_err(|e| ForgeError::backend(format!("argon2 hashing failed: {e}")))?;
        Ok(PhcString::new(phc))
    }

    async fn verify_password(&self, plain: &str, hash: &PhcString) -> Result<bool> {
        check_password(plain)?;
        if hash.as_str().len() > MAX_PHC_BYTES {
            return Err(ForgeError::limit("password hash exceeds the maximum size"));
        }
        let owned = plain.to_string();
        let hash_str = hash.as_str().to_string();
        let argon = forge_argon2()?;
        let result = tokio::task::spawn_blocking(move || {
            let parsed = PasswordHash::new(&hash_str)?;
            argon.verify_password(owned.as_bytes(), &parsed)
        })
        .await
        .map_err(|e| ForgeError::backend(format!("verify task failed: {e}")))?;
        match result {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(argon2::password_hash::Error::Crypto) => {
                Err(ForgeError::backend("argon2 verification failed"))
            }
            Err(e) => Err(ForgeError::invalid(format!("malformed password hash: {e}"))),
        }
    }

    fn needs_rehash(&self, hash: &PhcString) -> bool {
        let Ok(parsed) = PasswordHash::new(hash.as_str()) else {
            return true;
        };
        if parsed.algorithm.as_str() != "argon2id" {
            return true;
        }
        let Ok(params) = Params::try_from(&parsed) else {
            return true;
        };
        params.m_cost() < ARGON2_M_COST
            || params.t_cost() < ARGON2_T_COST
            || params.p_cost() < ARGON2_P_COST
    }

    async fn create_session(&self, user_id: &str, opts: SessionOpts) -> Result<SessionToken> {
        check_id(user_id)?;
        check_session_opts(&opts)?;
        let token = random_token();
        let token_hash = sha256_hex(token.as_bytes());
        let now = SystemTime::now();
        let record = SessionRecord {
            user_id: user_id.to_string(),
            created_at: now,
            idle: opts.idle_timeout,
            idle_deadline: now + opts.idle_timeout,
            abs_deadline: now + opts.absolute_timeout,
        };
        lock(&self.sessions).insert(self.physical(&token_hash), record);
        Ok(SessionToken::new(token))
    }

    async fn validate_session(&self, token: &str) -> Result<Option<Session>> {
        let pk = self.physical(&sha256_hex(token.as_bytes()));
        let now = SystemTime::now();
        let mut sessions = lock(&self.sessions);
        match sessions.get_mut(&pk) {
            Some(rec) if rec.is_live(now) => {
                // Slide the idle deadline forward, capped at the absolute ceiling.
                rec.idle_deadline = (now + rec.idle).min(rec.abs_deadline);
                Ok(Some(Session::new(
                    rec.user_id.clone(),
                    rec.created_at,
                    rec.idle_deadline,
                )))
            }
            // Unknown, expired, or revoked all read as absent (never an error). Expired
            // records linger until `purge_expired`, exactly like the unswept SQL rows.
            _ => Ok(None),
        }
    }

    async fn revoke_session(&self, token: &str) -> Result<()> {
        let pk = self.physical(&sha256_hex(token.as_bytes()));
        lock(&self.sessions).remove(&pk);
        Ok(())
    }

    async fn revoke_all_sessions(&self, user_id: &str) -> Result<u64> {
        let mut sessions = lock(&self.sessions);
        let before = sessions.len();
        sessions.retain(|_, rec| rec.user_id != user_id);
        Ok((before - sessions.len()) as u64)
    }

    async fn create_api_key(&self, owner_id: &str, label: &str) -> Result<ApiKey> {
        check_id(owner_id)?;
        check_label(label)?;
        let id = Uuid::new_v4().to_string();
        let secret = format!("{KEY_PREFIX}{}", random_token());
        let key_hash = sha256_hex(secret.as_bytes());
        let created_at = SystemTime::now();
        let record = ApiKeyRecord {
            id: id.clone(),
            owner_id: owner_id.to_string(),
            label: label.to_string(),
        };
        lock(&self.api_keys).insert(self.physical(&key_hash), record);
        Ok(ApiKey::new(
            id,
            label.to_string(),
            ApiKeySecret::new(secret),
            created_at,
        ))
    }

    async fn verify_api_key(&self, key: &str) -> Result<Option<ApiKeyInfo>> {
        // Look the key up by the SHA-256 of the presented secret: the raw secret is never
        // compared, and the digest is high-entropy, so this leaks no timing signal about
        // it — the same property the SQL backend's indexed-digest lookup has.
        let pk = self.physical(&sha256_hex(key.as_bytes()));
        let api_keys = lock(&self.api_keys);
        Ok(api_keys
            .get(&pk)
            .map(|r| ApiKeyInfo::new(r.id.clone(), r.owner_id.clone(), r.label.clone())))
    }

    async fn revoke_api_key(&self, key_id: &str) -> Result<bool> {
        let mut api_keys = lock(&self.api_keys);
        // Revocation is keyed on the stable id, not the digest, so find the matching
        // entry first. All entries in this map already belong to this app's namespace.
        let pk = api_keys
            .iter()
            .find(|(_, r)| r.id == key_id)
            .map(|(k, _)| k.clone());
        match pk {
            Some(k) => {
                api_keys.remove(&k);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

#[async_trait]
impl BackendLifecycle for MemAuth {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Auth
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process, not durable"
    }
    async fn maintain(&self) -> Result<()> {
        self.purge_expired();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn password_hashes_and_verifies() {
        let auth = MemAuth::new(String::new());
        let phc = auth.hash_password("correct horse").await.unwrap();
        assert!(phc.as_str().starts_with("$argon2id$"));
        assert!(auth.verify_password("correct horse", &phc).await.unwrap());
        assert!(!auth.verify_password("wrong", &phc).await.unwrap());
    }

    #[tokio::test]
    async fn password_input_is_validated() {
        let auth = MemAuth::new(String::new());
        assert!(matches!(
            auth.hash_password("").await,
            Err(ForgeError::Invalid(_))
        ));
        let big = "a".repeat(MAX_PASSWORD_BYTES + 1);
        assert!(matches!(
            auth.hash_password(&big).await,
            Err(ForgeError::Limit(_))
        ));

        let phc = auth.hash_password("pw").await.unwrap();
        assert!(matches!(
            auth.verify_password("", &phc).await,
            Err(ForgeError::Invalid(_))
        ));
        // An over-cap hash is rejected as Limit rather than silently verified.
        let huge = PhcString::new("$argon2id$".to_string() + &"x".repeat(MAX_PHC_BYTES));
        assert!(matches!(
            auth.verify_password("pw", &huge).await,
            Err(ForgeError::Limit(_))
        ));
    }

    #[tokio::test]
    async fn verify_rejects_malformed_hash() {
        let auth = MemAuth::new(String::new());
        // A malformed (but in-size) hash is Invalid, never Ok(false).
        let bad = PhcString::new("definitely-not-a-phc-string");
        assert!(matches!(
            auth.verify_password("pw", &bad).await,
            Err(ForgeError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn needs_rehash_detects_weak_and_malformed() {
        let auth = MemAuth::new(String::new());
        let strong = auth.hash_password("pw").await.unwrap();
        assert!(!auth.needs_rehash(&strong), "current params are fine");
        assert!(auth.needs_rehash(&PhcString::new("garbage")), "garbage => rehash");

        // A valid argon2id hash below the current memory cost must be flagged.
        let weak = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            Params::new(8, ARGON2_T_COST, ARGON2_P_COST, None).unwrap(),
        )
        .hash_password(b"pw", &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string();
        assert!(auth.needs_rehash(&PhcString::new(weak)), "below params => rehash");
    }

    #[tokio::test]
    async fn session_create_validate_revoke() {
        let auth = MemAuth::new(String::new());
        let token = auth.create_session("user-1", SessionOpts::new()).await.unwrap();

        let s = auth
            .validate_session(token.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.user_id, "user-1");
        assert!(s.expires_at > s.created_at);

        assert!(auth.validate_session("unknown-token").await.unwrap().is_none());

        auth.revoke_session(token.as_str()).await.unwrap();
        assert!(auth.validate_session(token.as_str()).await.unwrap().is_none());
        // Revoke is idempotent.
        auth.revoke_session(token.as_str()).await.unwrap();
    }

    #[tokio::test]
    async fn sessions_expire_and_sweep_reclaims_them() {
        let auth = MemAuth::new(String::new());
        let opts = SessionOpts::new()
            .with_idle_timeout(Duration::from_millis(100))
            .with_absolute_timeout(Duration::from_millis(100));
        let token = auth.create_session("u", opts).await.unwrap();
        assert!(auth.validate_session(token.as_str()).await.unwrap().is_some());

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            auth.validate_session(token.as_str()).await.unwrap().is_none(),
            "past both deadlines => absent"
        );
        // The expired record lingers (validate does not delete) until a sweep reclaims it.
        assert_eq!(auth.purge_expired(), 1);
        assert_eq!(auth.purge_expired(), 0, "sweep is idempotent");
    }

    #[tokio::test]
    async fn validate_caps_slide_at_absolute_deadline() {
        let auth = MemAuth::new(String::new());
        // idle == absolute, so the slid deadline (now + idle) always overshoots the
        // ceiling (created + idle) by the elapsed time and must be clamped back to it.
        let opts = SessionOpts::new()
            .with_idle_timeout(Duration::from_secs(2))
            .with_absolute_timeout(Duration::from_secs(2));
        let token = auth.create_session("u", opts).await.unwrap();

        tokio::time::sleep(Duration::from_millis(400)).await;
        let s = auth
            .validate_session(token.as_str())
            .await
            .unwrap()
            .unwrap();
        // Sliding alone would push the deadline to ~now+2s (~created+2.4s); the absolute
        // ceiling at created+2s must clamp it instead.
        assert!(
            s.expires_at <= s.created_at + Duration::from_millis(2100),
            "idle slide is capped at the absolute deadline"
        );
    }

    #[tokio::test]
    async fn revoke_all_sessions_clears_only_that_user() {
        let auth = MemAuth::new(String::new());
        let a1 = auth.create_session("a", SessionOpts::new()).await.unwrap();
        let a2 = auth.create_session("a", SessionOpts::new()).await.unwrap();
        let b1 = auth.create_session("b", SessionOpts::new()).await.unwrap();

        assert_eq!(auth.revoke_all_sessions("a").await.unwrap(), 2);
        assert!(auth.validate_session(a1.as_str()).await.unwrap().is_none());
        assert!(auth.validate_session(a2.as_str()).await.unwrap().is_none());
        assert!(auth.validate_session(b1.as_str()).await.unwrap().is_some());
        assert_eq!(auth.revoke_all_sessions("a").await.unwrap(), 0, "idempotent");
    }

    #[tokio::test]
    async fn api_key_create_verify_revoke() {
        let auth = MemAuth::new(String::new());
        let key = auth.create_api_key("owner-9", "ci-deploy").await.unwrap();
        assert!(key.secret.as_str().starts_with("fk_"));

        let info = auth
            .verify_api_key(key.secret.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info.id, key.id);
        assert_eq!(info.owner_id, "owner-9");
        assert_eq!(info.label, "ci-deploy");

        assert!(auth.verify_api_key("fk_unknown").await.unwrap().is_none());

        assert!(auth.revoke_api_key(&key.id).await.unwrap());
        assert!(auth.verify_api_key(key.secret.as_str()).await.unwrap().is_none());
        assert!(
            !auth.revoke_api_key(&key.id).await.unwrap(),
            "revoking an unknown id => false"
        );
    }

    #[tokio::test]
    async fn namespaces_isolate_sessions_and_keys() {
        let a = MemAuth::new("app_a".to_string());
        let b = MemAuth::new("app_b".to_string());

        let token = a.create_session("u", SessionOpts::new()).await.unwrap();
        assert!(a.validate_session(token.as_str()).await.unwrap().is_some());
        assert!(
            b.validate_session(token.as_str()).await.unwrap().is_none(),
            "another app cannot validate the session"
        );

        let key = a.create_api_key("u", "k").await.unwrap();
        assert!(a.verify_api_key(key.secret.as_str()).await.unwrap().is_some());
        assert!(b.verify_api_key(key.secret.as_str()).await.unwrap().is_none());
        assert!(
            !b.revoke_api_key(&key.id).await.unwrap(),
            "another app cannot revoke the key"
        );
    }

    #[tokio::test]
    async fn rejects_bad_input() {
        let auth = MemAuth::new(String::new());
        assert!(matches!(
            auth.create_session("", SessionOpts::new()).await,
            Err(ForgeError::Invalid(_))
        ));
        let zero = SessionOpts::new().with_idle_timeout(Duration::ZERO);
        assert!(matches!(
            auth.create_session("u", zero).await,
            Err(ForgeError::Invalid(_))
        ));
        let inverted = SessionOpts::new()
            .with_idle_timeout(Duration::from_secs(100))
            .with_absolute_timeout(Duration::from_secs(10));
        assert!(matches!(
            auth.create_session("u", inverted).await,
            Err(ForgeError::Invalid(_))
        ));

        assert!(matches!(
            auth.create_api_key("", "label").await,
            Err(ForgeError::Invalid(_))
        ));
        let big_label = "x".repeat(MAX_LABEL_BYTES + 1);
        assert!(matches!(
            auth.create_api_key("owner", &big_label).await,
            Err(ForgeError::Limit(_))
        ));
    }
}

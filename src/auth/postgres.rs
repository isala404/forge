//! Postgres `auth` backend. Contract: docs/contracts/auth.md.
//!
//! Passwords: argon2id PHC strings (Forge-owned params), hashed on a blocking thread.
//! Sessions/API keys: high-entropy random secrets, stored only as their SHA-256;
//! validation hashes the presented secret and looks it up by that indexed digest, so
//! no raw secret is ever compared in app code (and a DB leak yields only digests).

use super::{
    ApiKey, ApiKeyInfo, ApiKeySecret, Auth, MAX_ID_BYTES, MAX_LABEL_BYTES, MAX_PASSWORD_BYTES,
    MAX_PHC_BYTES, PhcString, Session, SessionOpts, SessionToken,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::util::{hex, key_hash, sha256_hex};
use argon2::password_hash::SaltString;
use argon2::{Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use rand_core::{OsRng, RngCore};
use sqlx::PgPool;
use tracing::field::Empty;
use uuid::Uuid;

/// Forge-owned argon2id parameters (OWASP baseline as of 2026; identical to the
/// argon2 crate's current `Params::DEFAULT`, so pinning them changes nothing today).
/// They are pinned so a routine argon2-crate bump cannot silently change hashing
/// cost or flip `needs_rehash` for every stored hash. Bump these deliberately.
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

/// Postgres-backed [`Auth`].
pub(crate) struct PgAuth {
    pool: PgPool,
    /// The app namespace (`forge_sessions.app` / `forge_api_keys.app`), so an app
    /// sharing a database can neither validate nor revoke another app's sessions or
    /// keys. Empty = the unnamespaced app.
    app: String,
}

impl PgAuth {
    pub(crate) fn new(pool: PgPool, app: String) -> Self {
        Self { pool, app }
    }

    /// Delete expired sessions. Idempotent; call from `maintain`.
    pub(crate) async fn sweep(&self) -> Result<u64> {
        // `idle_deadline <= abs_deadline` is an invariant (enforced at create and
        // capped at validate), so the abs_deadline arm is redundant; dropping it lets
        // the delete use the idle_deadline index instead of seq-scanning.
        let r = sqlx::query!("DELETE FROM forge_sessions WHERE idle_deadline <= now()")
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

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

#[async_trait]
impl Auth for PgAuth {
    async fn hash_password(&self, plain: &str) -> Result<PhcString> {
        let span = tracing::info_span!(
            "forge.auth.hash_password",
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "hash_password", span, async move {
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
        })
        .await
    }

    async fn verify_password(&self, plain: &str, hash: &PhcString) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.auth.verify_password",
            auth.verify_ok = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "verify_password", span, async move {
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
            let ok = match result {
                Ok(()) => true,
                Err(argon2::password_hash::Error::Password) => false,
                Err(argon2::password_hash::Error::Crypto) => {
                    return Err(ForgeError::backend("argon2 verification failed"));
                }
                Err(e) => return Err(ForgeError::invalid(format!("malformed password hash: {e}"))),
            };
            tracing::Span::current().record("auth.verify_ok", ok);
            Ok(ok)
        })
        .await
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
        let span = tracing::info_span!(
            "forge.auth.create_session",
            auth.user_hash = %key_hash(user_id),
            auth.idle_secs = opts.idle_timeout.as_secs(),
            auth.absolute_secs = opts.absolute_timeout.as_secs(),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "create_session", span, async move {
            check_id(user_id)?;
            check_session_opts(&opts)?;
            let token = random_token();
            let token_hash = sha256_hex(token.as_bytes());
            let idle = opts.idle_timeout.as_secs_f64();
            let abs = opts.absolute_timeout.as_secs_f64();
            sqlx::query!(
                "INSERT INTO forge_sessions \
                   (token_hash, user_id, idle_secs, idle_deadline, abs_deadline, app) \
                 VALUES ($1, $2, $3, now() + make_interval(secs => $3), \
                         now() + make_interval(secs => $4), $5)",
                token_hash,
                user_id,
                idle,
                abs,
                self.app,
            )
            .execute(&self.pool)
            .await?;
            Ok(SessionToken::new(token))
        })
        .await
    }

    async fn validate_session(&self, token: &str) -> Result<Option<Session>> {
        let token_hash = sha256_hex(token.as_bytes());
        let span = tracing::info_span!(
            "forge.auth.validate_session",
            auth.token_hash = %token_hash,
            auth.session_valid = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "validate_session", span, async move {
            // Slide the idle deadline (capped at absolute) iff still live, atomically.
            let row = sqlx::query!(
                r#"UPDATE forge_sessions
                   SET idle_deadline = LEAST(now() + make_interval(secs => idle_secs), abs_deadline)
                   WHERE token_hash = $1 AND app = $2 AND idle_deadline > now() AND abs_deadline > now()
                   RETURNING user_id, created_at, idle_deadline AS expires_at"#,
                token_hash,
                self.app,
            )
            .fetch_optional(&self.pool)
            .await?;
            tracing::Span::current().record("auth.session_valid", row.is_some());
            Ok(row.map(|r| Session::new(r.user_id, r.created_at.into(), r.expires_at.into())))
        })
        .await
    }

    async fn revoke_session(&self, token: &str) -> Result<()> {
        let token_hash = sha256_hex(token.as_bytes());
        let span = tracing::info_span!(
            "forge.auth.revoke_session",
            auth.token_hash = %token_hash,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "revoke_session", span, async move {
            sqlx::query!(
                "DELETE FROM forge_sessions WHERE token_hash = $1 AND app = $2",
                token_hash,
                self.app,
            )
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn revoke_all_sessions(&self, user_id: &str) -> Result<u64> {
        let span = tracing::info_span!(
            "forge.auth.revoke_all_sessions",
            auth.user_hash = %key_hash(user_id),
            auth.revoked_count = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "revoke_all_sessions", span, async move {
            let r = sqlx::query!(
                "DELETE FROM forge_sessions WHERE user_id = $1 AND app = $2",
                user_id,
                self.app,
            )
            .execute(&self.pool)
            .await?;
            tracing::Span::current().record("auth.revoked_count", r.rows_affected());
            Ok(r.rows_affected())
        })
        .await
    }

    async fn create_api_key(&self, owner_id: &str, label: &str) -> Result<ApiKey> {
        let id = Uuid::new_v4().to_string();
        let span = tracing::info_span!(
            "forge.auth.create_api_key",
            auth.user_hash = %key_hash(owner_id),
            auth.key_id = %id,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "create_api_key", span, async move {
            check_id(owner_id)?;
            check_label(label)?;
            let secret = format!("{KEY_PREFIX}{}", random_token());
            let key_hash = sha256_hex(secret.as_bytes());
            let created_at = sqlx::query_scalar!(
                "INSERT INTO forge_api_keys (id, key_hash, owner_id, label, app) \
                 VALUES ($1, $2, $3, $4, $5) RETURNING created_at",
                id,
                key_hash,
                owner_id,
                label,
                self.app,
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(ApiKey::new(
                id,
                label.to_string(),
                ApiKeySecret::new(secret),
                created_at.into(),
            ))
        })
        .await
    }

    async fn verify_api_key(&self, key: &str) -> Result<Option<ApiKeyInfo>> {
        let key_hash = sha256_hex(key.as_bytes());
        let span = tracing::info_span!(
            "forge.auth.verify_api_key",
            auth.token_hash = %key_hash,
            auth.key_valid = Empty,
            auth.key_id = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "verify_api_key", span, async move {
            let row = sqlx::query!(
                "SELECT id, owner_id, label FROM forge_api_keys WHERE key_hash = $1 AND app = $2",
                key_hash,
                self.app,
            )
            .fetch_optional(&self.pool)
            .await?;
            let s = tracing::Span::current();
            s.record("auth.key_valid", row.is_some());
            if let Some(r) = &row {
                s.record("auth.key_id", r.id.as_str());
            }
            Ok(row.map(|r| ApiKeyInfo::new(r.id, r.owner_id, r.label)))
        })
        .await
    }

    async fn revoke_api_key(&self, key_id: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.auth.revoke_api_key",
            auth.key_id = %key_id,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("auth", "revoke_api_key", span, async move {
            let removed = sqlx::query_scalar!(
                "DELETE FROM forge_api_keys WHERE id = $1 AND app = $2 RETURNING id",
                key_id,
                self.app,
            )
            .fetch_optional(&self.pool)
            .await?
            .is_some();
            Ok(removed)
        })
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn random_tokens_are_unique_and_256_bit() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), TOKEN_BYTES * 2, "hex of 32 bytes = 64 chars");
        assert_ne!(a, b);
    }

    #[test]
    fn session_opts_validation() {
        assert!(check_session_opts(&SessionOpts::default()).is_ok());
        let zero = SessionOpts::default().with_idle_timeout(Duration::ZERO);
        assert!(matches!(
            check_session_opts(&zero),
            Err(ForgeError::Invalid(_))
        ));
        let inverted = SessionOpts::default()
            .with_idle_timeout(Duration::from_secs(100))
            .with_absolute_timeout(Duration::from_secs(10));
        assert!(matches!(
            check_session_opts(&inverted),
            Err(ForgeError::Invalid(_))
        ));
    }
}

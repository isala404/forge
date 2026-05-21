//! Refresh token management.
//!
//! Provides token pair issuance (access + refresh), rotation, and revocation.
//! Refresh tokens are stored as SHA-256 hashes in `forge_refresh_tokens`.
//!
//! ## Token format
//!
//! Raw refresh tokens encode their family UUID so chain reuse detection can
//! revoke the whole family even after the specific token row is deleted:
//!
//! ```text
//! <family_uuid_hex>.<random_uuid_hex>
//! ```
//!
//! Only the SHA-256 hash of the full string is stored in the database.
//!
//! ## Chain reuse detection
//!
//! Each token family represents a single login session. During rotation the old
//! token is deleted and a new one with the same `token_family` is inserted
//! atomically. If a previously-rotated (deleted) token is presented:
//!
//! 1. The DELETE returns 0 rows.
//! 2. The family UUID is decoded from the raw token value.
//! 3. All tokens sharing that family are revoked immediately.
//! 4. `Unauthorized` is returned to the caller.
//!
//! This terminates the session for both the legitimate user (who holds the
//! current token) and the attacker who replayed the old one.

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{ForgeError, Result};

/// An access token + refresh token pair.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
}

/// SHA-256 hash a raw token string for storage.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generate a raw refresh token that encodes its family UUID.
///
/// Format: `<family_hex>.<random_hex>` — the dot separator lets
/// `extract_family` recover the family without a DB lookup.
fn generate_refresh_token_for_family(family: Uuid) -> String {
    let random = Uuid::new_v4();
    format!("{}.{}", family.simple(), random.simple())
}

/// Extract the family UUID from a raw refresh token.
///
/// Returns `None` for legacy tokens that pre-date the family format.
fn extract_family(raw_token: &str) -> Option<Uuid> {
    let (family_hex, _) = raw_token.split_once('.')?;
    Uuid::parse_str(family_hex).ok()
}

/// Issue a token pair: sign an access JWT and store a refresh token.
///
/// `issue_access_fn` is called to sign the access token (wraps `ctx.issue_token`).
/// `client_id` binds the refresh token to an OAuth client (pass `None` for non-OAuth usage).
pub async fn issue_token_pair(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    roles: &[&str],
    access_token_ttl_secs: i64,
    refresh_token_ttl_days: i64,
    issue_access_fn: impl FnOnce(Uuid, &[&str], i64) -> Result<String>,
) -> Result<TokenPair> {
    issue_token_pair_with_client(
        pool,
        user_id,
        roles,
        access_token_ttl_secs,
        refresh_token_ttl_days,
        None,
        issue_access_fn,
    )
    .await
}

/// Issue a token pair with optional OAuth client binding.
///
/// When `client_id` is `Some`, the refresh token is bound to that client
/// and can only be rotated by presenting the same client_id.
pub async fn issue_token_pair_with_client(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    roles: &[&str],
    access_token_ttl_secs: i64,
    refresh_token_ttl_days: i64,
    client_id: Option<&str>,
    issue_access_fn: impl FnOnce(Uuid, &[&str], i64) -> Result<String>,
) -> Result<TokenPair> {
    let family = Uuid::new_v4();
    issue_token_in_family(
        pool,
        user_id,
        roles,
        access_token_ttl_secs,
        refresh_token_ttl_days,
        client_id,
        family,
        issue_access_fn,
    )
    .await
}

/// Internal: insert a new refresh token carrying an existing family ID.
///
/// Used both by `issue_token_pair_with_client` (new family) and
/// `rotate_refresh_token_with_client` (carry family forward).
#[allow(clippy::too_many_arguments)]
async fn issue_token_in_family(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    roles: &[&str],
    access_token_ttl_secs: i64,
    refresh_token_ttl_days: i64,
    client_id: Option<&str>,
    family: Uuid,
    issue_access_fn: impl FnOnce(Uuid, &[&str], i64) -> Result<String>,
) -> Result<TokenPair> {
    let access_token = issue_access_fn(user_id, roles, access_token_ttl_secs)?;

    let refresh_raw = generate_refresh_token_for_family(family);
    let refresh_hash = hash_token(&refresh_raw);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(refresh_token_ttl_days);

    let roles_owned: Vec<String> = roles.iter().map(|s| s.to_string()).collect();
    sqlx::query!(
        "INSERT INTO forge_refresh_tokens (user_id, token_hash, client_id, expires_at, token_family, roles) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        user_id,
        &refresh_hash,
        client_id,
        expires_at,
        family,
        &roles_owned,
    )
    .execute(pool)
    .await
    .map_err(|e| ForgeError::internal_with("Failed to store refresh token", e))?;

    Ok(TokenPair {
        access_token,
        refresh_token: refresh_raw,
    })
}

/// Rotate a refresh token: validate expiry, delete the old one, issue a new pair.
///
/// Roles are carried forward from the old token row — the rotated token has
/// the exact same role set as the original sign-in. New role grants only take
/// effect at next sign-in.
pub async fn rotate_refresh_token(
    pool: &sqlx::PgPool,
    old_refresh_token: &str,
    access_token_ttl_secs: i64,
    refresh_token_ttl_days: i64,
    issue_access_fn: impl FnOnce(Uuid, &[&str], i64) -> Result<String>,
) -> Result<TokenPair> {
    rotate_refresh_token_with_client(
        pool,
        old_refresh_token,
        access_token_ttl_secs,
        refresh_token_ttl_days,
        None,
        issue_access_fn,
    )
    .await
}

/// Rotate a refresh token with OAuth client binding validation.
///
/// When `client_id` is `Some`, the token must be bound to that client.
/// The new token is issued in the same family as the old one and inherits
/// its roles.
///
/// ## Chain reuse detection
///
/// If the DELETE returns 0 rows the token is either invalid, expired, or
/// already rotated. The family UUID is decoded from the raw token value
/// (no extra DB read needed). If it parses, all tokens in that family are
/// revoked immediately — the session is terminated for everyone holding a
/// token in the chain, cutting off both the attacker and the legitimate user.
pub async fn rotate_refresh_token_with_client(
    pool: &sqlx::PgPool,
    old_refresh_token: &str,
    access_token_ttl_secs: i64,
    refresh_token_ttl_days: i64,
    client_id: Option<&str>,
    issue_access_fn: impl FnOnce(Uuid, &[&str], i64) -> Result<String>,
) -> Result<TokenPair> {
    let hash = hash_token(old_refresh_token);

    // Atomically delete the token if valid, returning the family + roles so
    // the new token is issued in the same chain with the same role set.
    //
    // When client_id is provided, require exact match. When omitted, only
    // allow rotation of tokens that were NOT bound to any client (prevents
    // an attacker from bypassing client binding by omitting client_id).
    struct TokenRow {
        user_id: Uuid,
        token_family: Uuid,
        roles: Vec<String>,
    }

    let row = if let Some(cid) = client_id {
        sqlx::query!(
            "DELETE FROM forge_refresh_tokens \
             WHERE token_hash = $1 AND expires_at > now() AND client_id = $2 \
             RETURNING user_id, token_family, roles",
            hash,
            cid
        )
        .fetch_optional(pool)
        .await
        .map(|r| {
            r.map(|r| TokenRow {
                user_id: r.user_id,
                token_family: r.token_family,
                roles: r.roles,
            })
        })
    } else {
        sqlx::query!(
            "DELETE FROM forge_refresh_tokens \
             WHERE token_hash = $1 AND expires_at > now() AND client_id IS NULL \
             RETURNING user_id, token_family, roles",
            hash
        )
        .fetch_optional(pool)
        .await
        .map(|r| {
            r.map(|r| TokenRow {
                user_id: r.user_id,
                token_family: r.token_family,
                roles: r.roles,
            })
        })
    }
    .map_err(|e| ForgeError::internal_with("Failed to rotate refresh token", e))?;

    match row {
        Some(token) => {
            let roles_refs: Vec<&str> = token.roles.iter().map(String::as_str).collect();
            issue_token_in_family(
                pool,
                token.user_id,
                &roles_refs,
                access_token_ttl_secs,
                refresh_token_ttl_days,
                client_id,
                token.token_family,
                issue_access_fn,
            )
            .await
        }
        None => {
            // Token not found. Decode the family from the raw token value —
            // if the format matches, this is a previously-rotated token being
            // replayed (reuse attack). Nuke the whole family to terminate the
            // session for everyone, then return Unauthorized.
            if let Some(family_id) = extract_family(old_refresh_token) {
                let deleted = sqlx::query!(
                    "DELETE FROM forge_refresh_tokens WHERE token_family = $1",
                    family_id
                )
                .execute(pool)
                .await
                .map(|r| r.rows_affected())
                .unwrap_or(0);

                if deleted > 0 {
                    tracing::warn!(
                        %family_id,
                        revoked = deleted,
                        "Refresh token reuse detected — entire family revoked"
                    );
                }
            }

            Err(ForgeError::Unauthorized(
                "Invalid or expired refresh token".into(),
            ))
        }
    }
}

/// Revoke a specific refresh token.
pub async fn revoke_refresh_token(pool: &sqlx::PgPool, refresh_token: &str) -> Result<()> {
    let hash = hash_token(refresh_token);
    sqlx::query!(
        "DELETE FROM forge_refresh_tokens WHERE token_hash = $1",
        &hash
    )
    .execute(pool)
    .await
    .map_err(|e| ForgeError::internal_with("Failed to revoke refresh token", e))?;
    Ok(())
}

/// Revoke all refresh tokens for a user.
pub async fn revoke_all_refresh_tokens(pool: &sqlx::PgPool, user_id: Uuid) -> Result<()> {
    sqlx::query!(
        "DELETE FROM forge_refresh_tokens WHERE user_id = $1",
        user_id
    )
    .execute(pool)
    .await
    .map_err(|e| ForgeError::internal_with("Failed to revoke refresh tokens", e))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_refresh_token_for_family_encodes_family() {
        let family = Uuid::new_v4();
        let token = generate_refresh_token_for_family(family);

        assert!(token.contains('.'), "token must contain the dot separator");
        let recovered = extract_family(&token);
        assert_eq!(recovered, Some(family));
    }

    #[test]
    fn test_extract_family_returns_none_for_legacy_format() {
        let legacy = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        assert_eq!(extract_family(&legacy), None);
    }

    #[test]
    fn test_extract_family_returns_none_for_garbage() {
        assert_eq!(extract_family("not-a-token"), None);
        assert_eq!(extract_family(""), None);
    }

    #[test]
    fn test_hash_token_is_deterministic() {
        let token = "some-raw-token-value";
        assert_eq!(hash_token(token), hash_token(token));
    }

    #[test]
    fn test_hash_token_differs_for_different_inputs() {
        assert_ne!(hash_token("token-a"), hash_token("token-b"));
    }

    #[test]
    fn hash_token_returns_64_char_lowercase_hex() {
        let hash = hash_token("anything");
        assert_eq!(hash.len(), 64, "SHA-256 hex is exactly 64 chars");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex digits, got {hash}"
        );
    }

    #[test]
    fn hash_token_matches_known_sha256_for_empty_string() {
        // Pinning the hash for "" guards against accidental algorithm change
        // (would break every refresh token after the swap).
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(hash_token(""), expected);
    }

    #[test]
    fn generate_refresh_token_for_family_returns_unique_random_part() {
        let family = Uuid::new_v4();
        let a = generate_refresh_token_for_family(family);
        let b = generate_refresh_token_for_family(family);
        assert_ne!(a, b);
        assert_eq!(extract_family(&a), Some(family));
        assert_eq!(extract_family(&b), Some(family));
    }

    #[test]
    fn generate_refresh_token_for_family_has_expected_shape() {
        let family = Uuid::new_v4();
        let token = generate_refresh_token_for_family(family);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 2, "exactly one dot separator");
        assert_eq!(parts[0].len(), 32);
        assert_eq!(parts[1].len(), 32);
        assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn extract_family_returns_none_when_prefix_is_not_a_uuid() {
        // Has a dot but the prefix isn't a valid UUID — should not falsely
        // identify it as a family-format token.
        assert_eq!(extract_family("notauuid.suffix"), None);
    }

    #[test]
    fn extract_family_returns_first_segment_uuid_for_multi_dot_tokens() {
        // `split_once('.')` only splits on the first dot. As long as the first
        // segment parses as a UUID, additional dots after it don't matter.
        let family = Uuid::new_v4();
        let weird = format!("{}.a.b.c", family.simple());
        assert_eq!(extract_family(&weird), Some(family));
    }

    #[test]
    fn token_pair_round_trips_through_json() {
        let pair = TokenPair {
            access_token: "header.payload.sig".into(),
            refresh_token: "fam.rand".into(),
        };
        let s = serde_json::to_string(&pair).unwrap();
        let back: TokenPair = serde_json::from_str(&s).unwrap();
        assert_eq!(back.access_token, pair.access_token);
        assert_eq!(back.refresh_token, pair.refresh_token);
    }

    #[test]
    fn hash_token_is_independent_of_token_length() {
        // Tiny and very long inputs both yield 64-char hashes — bounded output.
        let huge = "x".repeat(10_000);
        assert_eq!(hash_token(&huge).len(), 64);
        assert_eq!(hash_token("a").len(), 64);
    }
}

//! Daily-rotating visitor ID generation.
//!
//! Generates a deterministic but non-reversible visitor identifier from
//! `SHA256(client_ip + user_agent + daily_salt)`. The salt rotates at
//! midnight UTC so visitors cannot be tracked across days.
//!
//! GDPR-safe: one-way hash, not reversible, not linkable across days.

use sha2::{Digest, Sha256};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Must stay in sync with `gateway::server::DEFAULT_SIGNAL_SECRET`. If a
/// caller passes this literal we refuse to emit a real visitor ID, since
/// the daily salt would be trivially reversible by anyone who reads the
/// open-source repo.
const DEFAULT_SIGNAL_SECRET: &str = "forge-default-signal-secret";

/// One-shot guard so we only log the "default secret in use" warning once
/// rather than on every request.
static WARNED_DEFAULT_SECRET: AtomicBool = AtomicBool::new(false);

/// Cached daily salt to avoid recomputing on every request.
struct DailySalt {
    date: String,
    salt: String,
}

/// RwLock (not Mutex) so concurrent readers don't contend on the hot path.
/// Poisoned locks are recovered via `.unwrap_or_else(|e| e.into_inner())`
/// so a panic in one handler doesn't take down visitor-id generation.
static CACHED_SALT: RwLock<Option<DailySalt>> = RwLock::new(None);

/// Generate a visitor ID from client IP and User-Agent.
///
/// The ID is stable within a single UTC day for the same IP+UA pair,
/// but changes at midnight. Returns a 32-character hex string (128 bits).
pub fn generate_visitor_id(
    client_ip: Option<&str>,
    user_agent: Option<&str>,
    server_secret: &str,
) -> String {
    if server_secret == DEFAULT_SIGNAL_SECRET {
        if !WARNED_DEFAULT_SECRET.swap(true, Ordering::Relaxed) {
            tracing::error!(
                "signals: default visitor-ID secret in use; refusing to emit a real visitor ID. \
                 Configure [auth] jwt_secret in forge.toml to enable visitor tracking."
            );
        }
        return String::new();
    }
    let ip = client_ip.unwrap_or("unknown");
    let ua = user_agent.unwrap_or("unknown");
    let salt = get_daily_salt(server_secret);

    let mut hasher = Sha256::new();
    hasher.update(ip.as_bytes());
    hasher.update(b"|");
    hasher.update(ua.as_bytes());
    hasher.update(b"|");
    hasher.update(salt.as_bytes());

    let hash = hasher.finalize();
    // 32 hex chars (128 bits) so birthday collisions require ~2^64 visitors.
    hash.iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// Get or compute the daily salt from the server secret + current date.
///
/// Fast path: read lock, clone cached salt. Slow path: upgrade to write lock,
/// recompute, store. Poisoned locks are recovered so visitor-ID generation
/// survives a panic in any handler holding the lock.
fn get_daily_salt(server_secret: &str) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    {
        let guard = CACHED_SALT.read().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.as_ref()
            && cached.date == today
        {
            return cached.salt.clone();
        }
    }

    let salt = compute_salt(server_secret, &today);

    let mut guard = CACHED_SALT.write().unwrap_or_else(|e| e.into_inner());
    // Another thread may have populated it between our read and write.
    if let Some(cached) = guard.as_ref()
        && cached.date == today
    {
        return cached.salt.clone();
    }
    *guard = Some(DailySalt {
        date: today,
        salt: salt.clone(),
    });

    salt
}

fn compute_salt(secret: &str, date: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"forge_signals_daily_salt:");
    hasher.update(secret.as_bytes());
    hasher.update(b":");
    hasher.update(date.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_inputs_same_day_produce_same_id() {
        let id1 = generate_visitor_id(Some("1.2.3.4"), Some("Chrome/120"), "secret");
        let id2 = generate_visitor_id(Some("1.2.3.4"), Some("Chrome/120"), "secret");
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn different_ip_produces_different_id() {
        let id1 = generate_visitor_id(Some("1.2.3.4"), Some("Chrome/120"), "secret");
        let id2 = generate_visitor_id(Some("5.6.7.8"), Some("Chrome/120"), "secret");
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn different_ua_produces_different_id() {
        let id1 = generate_visitor_id(Some("1.2.3.4"), Some("Chrome/120"), "secret");
        let id2 = generate_visitor_id(Some("1.2.3.4"), Some("Firefox/121"), "secret");
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn visitor_id_is_32_hex_chars() {
        let id = generate_visitor_id(Some("1.2.3.4"), Some("Chrome"), "secret");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn handles_missing_inputs() {
        let id = generate_visitor_id(None, None, "secret");
        assert_eq!(id.len(), 32);
    }
}

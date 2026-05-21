//! JWKS (JSON Web Key Set) client for RSA token validation.
//!
//! This module provides a client for fetching and caching public keys from
//! JWKS endpoints, used by providers like Firebase, Clerk, Auth0, etc.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

/// How long to remember that a `kid` was absent from the latest JWKS fetch.
/// Short enough to pick up a legitimate key rotation within one cache cycle,
/// long enough to absorb a flood of forged-kid tokens without hammering the
/// JWKS endpoint.
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);

/// JWKS response structure from providers.
#[derive(Debug, Deserialize)]
pub struct JwksResponse {
    /// List of JSON Web Keys.
    pub keys: Vec<JsonWebKey>,
}

/// Individual JSON Web Key.
#[derive(Debug, Deserialize)]
pub struct JsonWebKey {
    /// Key ID - used to match tokens to keys.
    pub kid: Option<String>,

    /// Key type (RSA, EC, etc.).
    pub kty: String,

    /// Algorithm (RS256, RS384, RS512, etc.).
    pub alg: Option<String>,

    /// Key use (sig = signature, enc = encryption).
    #[serde(rename = "use")]
    pub key_use: Option<String>,

    /// RSA modulus (base64url encoded).
    pub n: Option<String>,

    /// RSA exponent (base64url encoded).
    pub e: Option<String>,

    /// X.509 certificate chain (used by Firebase).
    pub x5c: Option<Vec<String>>,
}

/// Cached JWKS keys with TTL tracking.
struct CachedJwks {
    /// Map of key ID to decoding key.
    keys: HashMap<String, DecodingKey>,
    /// When the cache was last refreshed.
    fetched_at: Instant,
}

/// JWKS client with automatic caching.
///
/// Fetches public keys from a JWKS endpoint and caches them for efficient
/// token validation. Keys are automatically refreshed when the cache expires.
///
/// # Example
///
/// ```ignore
/// let client = JwksClient::new(
///     "https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com".to_string(),
///     3600, // 1 hour cache TTL
/// );
///
/// // Get key by ID from token header
/// let key = client.get_key("abc123").await?;
/// ```
pub struct JwksClient {
    /// JWKS endpoint URL.
    url: String,
    /// HTTP client for fetching keys.
    http_client: reqwest::Client,
    /// Cached keys with TTL.
    cache: Arc<RwLock<Option<CachedJwks>>>,
    /// Cache time-to-live.
    cache_ttl: Duration,
    /// Singleflight guard: only one refresh at a time. Concurrent callers
    /// wait on the same lock and re-read the cache once it's released,
    /// preventing a thundering herd of JWKS HTTP fetches after key rotation.
    refresh_lock: Arc<Mutex<()>>,
    /// Short-lived negative cache for unknown `kid` values. Without it, an
    /// attacker can amplify each forged token into one JWKS HTTP fetch.
    /// Populated only after a refresh completes and the kid is still missing
    /// — never before — so legitimate rotations are not blocked.
    negative_cache: Arc<DashMap<String, Instant>>,
}

impl std::fmt::Debug for JwksClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksClient")
            .field("url", &self.url)
            .field("cache_ttl", &self.cache_ttl)
            .finish_non_exhaustive()
    }
}

impl JwksClient {
    /// Create a new JWKS client.
    ///
    /// # Arguments
    ///
    /// * `url` - The JWKS endpoint URL
    /// * `cache_ttl_secs` - How long to cache keys (in seconds)
    pub fn new(url: String, cache_ttl_secs: u64) -> Result<Self, JwksError> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| JwksError::HttpClientError(e.to_string()))?;

        Ok(Self {
            url,
            http_client,
            cache: Arc::new(RwLock::new(None)),
            cache_ttl: Duration::from_secs(cache_ttl_secs),
            refresh_lock: Arc::new(Mutex::new(())),
            negative_cache: Arc::new(DashMap::new()),
        })
    }

    /// Get a decoding key by key ID.
    ///
    /// This will return a cached key if available and not expired,
    /// otherwise it will fetch fresh keys from the JWKS endpoint.
    pub async fn get_key(&self, kid: &str) -> Result<DecodingKey, JwksError> {
        // Try to get from cache first
        {
            let cache = self.cache.read().await;
            if let Some(ref cached) = *cache
                && cached.fetched_at.elapsed() < self.cache_ttl
                && let Some(key) = cached.keys.get(kid)
            {
                debug!(kid = %kid, "Using cached JWKS key");
                return Ok(key.clone());
            }
        }

        // Short-circuit forged-kid flood: if we previously failed to find
        // this kid right after a refresh and the negative cache entry is
        // still fresh, do not trigger another refresh. The entry is only
        // ever written *after* a refresh has run, so legitimate rotation is
        // not blocked.
        if let Some(entry) = self.negative_cache.get(kid) {
            if entry.value().elapsed() < NEGATIVE_CACHE_TTL {
                return Err(JwksError::KeyNotFound(kid.to_string()));
            }
            drop(entry);
            self.negative_cache.remove(kid);
        }

        // Cache miss or expired — refresh, but coalesce concurrent callers.
        debug!(kid = %kid, "JWKS cache miss, refreshing");
        self.refresh_if_needed().await?;

        // Try again from refreshed cache
        let cache = self.cache.read().await;
        if let Some(ref cached) = *cache {
            match cached.keys.get(kid).cloned() {
                Some(key) => Ok(key),
                None => {
                    drop(cache);
                    // Record the miss so subsequent requests for this kid
                    // don't each trigger another refresh.
                    self.negative_cache.insert(kid.to_string(), Instant::now());
                    Err(JwksError::KeyNotFound(kid.to_string()))
                }
            }
        } else {
            Err(JwksError::FetchFailed(
                "Cache empty after refresh".to_string(),
            ))
        }
    }

    /// Refresh once across concurrent callers. Holds a Mutex while the HTTP
    /// fetch is in flight; waiters re-check the cache after the lock is
    /// released so they pick up the freshly fetched keys without firing
    /// another request.
    async fn refresh_if_needed(&self) -> Result<(), JwksError> {
        let _guard = self.refresh_lock.lock().await;
        // Re-check the cache under the lock: if another caller already
        // refreshed while we were waiting, skip the second fetch.
        {
            let cache = self.cache.read().await;
            if let Some(ref cached) = *cache
                && cached.fetched_at.elapsed() < self.cache_ttl
            {
                return Ok(());
            }
        }
        self.refresh().await
    }

    /// Get any available key (for tokens without kid header).
    ///
    /// Some providers don't include a key ID in tokens. This method
    /// returns the first available key from the JWKS.
    pub async fn get_any_key(&self) -> Result<DecodingKey, JwksError> {
        // Try to get from cache first
        {
            let cache = self.cache.read().await;
            if let Some(ref cached) = *cache
                && cached.fetched_at.elapsed() < self.cache_ttl
                && let Some(key) = cached.keys.values().next()
            {
                debug!("Using first cached JWKS key (no kid specified)");
                return Ok(key.clone());
            }
        }

        // Cache miss or expired — refresh, coalescing concurrent callers.
        debug!("JWKS cache miss for any key, refreshing");
        self.refresh_if_needed().await?;

        let cache = self.cache.read().await;
        if let Some(ref cached) = *cache {
            cached
                .keys
                .values()
                .next()
                .cloned()
                .ok_or(JwksError::NoKeysAvailable)
        } else {
            Err(JwksError::FetchFailed("No keys in JWKS".to_string()))
        }
    }

    /// Force refresh the key cache.
    ///
    /// Fetches fresh keys from the JWKS endpoint regardless of cache state.
    pub async fn refresh(&self) -> Result<(), JwksError> {
        debug!(url = %self.url, "Fetching JWKS");

        let response = self
            .http_client
            .get(&self.url)
            .send()
            .await
            .map_err(|e| JwksError::FetchFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(JwksError::FetchFailed(format!(
                "HTTP {} from JWKS endpoint",
                response.status()
            )));
        }

        let jwks: JwksResponse = response
            .json()
            .await
            .map_err(|e| JwksError::ParseFailed(e.to_string()))?;

        let mut keys = HashMap::new();

        for jwk in jwks.keys {
            // Skip non-signature keys
            if let Some(ref key_use) = jwk.key_use
                && key_use != "sig"
            {
                continue;
            }

            let kid = jwk.kid.clone().unwrap_or_else(|| "default".to_string());

            match self.parse_jwk(&jwk) {
                Ok(Some(key)) => {
                    debug!(kid = %kid, kty = %jwk.kty, "Parsed JWKS key");
                    keys.insert(kid, key);
                }
                Ok(None) => {
                    debug!(kid = %kid, kty = %jwk.kty, "Skipping unsupported key type");
                }
                Err(e) => {
                    warn!(kid = %kid, error = %e, "Failed to parse JWKS key");
                }
            }
        }

        if keys.is_empty() {
            return Err(JwksError::NoKeysAvailable);
        }

        debug!(count = keys.len(), "Cached JWKS keys");

        // Drop negative-cache entries for any kid that's now present, so a
        // rotation that hands us a previously-missing kid takes effect
        // immediately rather than after the negative TTL.
        for kid in keys.keys() {
            self.negative_cache.remove(kid);
        }

        let mut cache = self.cache.write().await;
        *cache = Some(CachedJwks {
            keys,
            fetched_at: Instant::now(),
        });

        Ok(())
    }

    /// Parse a JWK into a DecodingKey.
    fn parse_jwk(&self, jwk: &JsonWebKey) -> Result<Option<DecodingKey>, JwksError> {
        match jwk.kty.as_str() {
            "RSA" => {
                // Try X.509 certificate chain first (used by Firebase)
                if let Some(ref x5c) = jwk.x5c
                    && let Some(cert) = x5c.first()
                {
                    let pem = format!(
                        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
                        cert
                    );
                    return DecodingKey::from_rsa_pem(pem.as_bytes()).map(Some).map_err(
                        |e: jsonwebtoken::errors::Error| JwksError::KeyParseFailed(e.to_string()),
                    );
                }

                // Fall back to n/e components (used by Clerk, Auth0, etc.)
                if let (Some(n), Some(e)) = (&jwk.n, &jwk.e) {
                    return DecodingKey::from_rsa_components(n, e).map(Some).map_err(
                        |e: jsonwebtoken::errors::Error| JwksError::KeyParseFailed(e.to_string()),
                    );
                }

                // RSA key but missing required components
                Ok(None)
            }
            _ => {
                // Unsupported key type (EC, oct, etc.)
                Ok(None)
            }
        }
    }

    /// Get the JWKS URL.
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Errors that can occur when working with JWKS.
#[derive(Debug, thiserror::Error)]
pub enum JwksError {
    /// Failed to fetch JWKS from endpoint.
    #[error("Failed to fetch JWKS: {0}")]
    FetchFailed(String),

    /// Failed to parse JWKS response.
    #[error("Failed to parse JWKS: {0}")]
    ParseFailed(String),

    /// Failed to parse individual key.
    #[error("Failed to parse key: {0}")]
    KeyParseFailed(String),

    /// Requested key ID not found in JWKS.
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    /// No usable keys in JWKS.
    #[error("No keys available in JWKS")]
    NoKeysAvailable,

    /// Failed to create HTTP client.
    #[error("Failed to create HTTP client: {0}")]
    HttpClientError(String),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jwk_with_n_e() {
        let client = JwksClient::new("http://example.com".to_string(), 3600).unwrap();

        // Example RSA public key components (minimal test)
        let jwk = JsonWebKey {
            kid: Some("test-key".to_string()),
            kty: "RSA".to_string(),
            alg: Some("RS256".to_string()),
            key_use: Some("sig".to_string()),
            // These are example values - not a real key
            n: Some("0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw".to_string()),
            e: Some("AQAB".to_string()),
            x5c: None,
        };

        let result = client.parse_jwk(&jwk);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_parse_jwk_unsupported_type() {
        let client = JwksClient::new("http://example.com".to_string(), 3600).unwrap();

        let jwk = JsonWebKey {
            kid: Some("test-key".to_string()),
            kty: "EC".to_string(), // Unsupported
            alg: Some("ES256".to_string()),
            key_use: Some("sig".to_string()),
            n: None,
            e: None,
            x5c: None,
        };

        let result = client.parse_jwk(&jwk);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Should return None for unsupported types
    }

    #[test]
    fn test_parse_jwk_missing_components() {
        let client = JwksClient::new("http://example.com".to_string(), 3600).unwrap();

        let jwk = JsonWebKey {
            kid: Some("test-key".to_string()),
            kty: "RSA".to_string(),
            alg: Some("RS256".to_string()),
            key_use: Some("sig".to_string()),
            n: None, // Missing
            e: None, // Missing
            x5c: None,
        };

        let result = client.parse_jwk(&jwk);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Should return None when missing components
    }

    #[test]
    fn jwks_client_exposes_configured_url() {
        let client =
            JwksClient::new("https://issuer.example.com/.well-known/jwks".into(), 60).unwrap();
        assert_eq!(client.url(), "https://issuer.example.com/.well-known/jwks");
    }

    #[test]
    fn parse_jwk_returns_none_for_non_rsa_kty() {
        // "oct" (symmetric) keys can't be used for asymmetric verification; we
        // skip them silently rather than erroring, so the caller can keep
        // processing the rest of the JWKS.
        let client = JwksClient::new("http://example.com".into(), 60).unwrap();
        let jwk = JsonWebKey {
            kid: Some("sym".into()),
            kty: "oct".into(),
            alg: None,
            key_use: Some("sig".into()),
            n: None,
            e: None,
            x5c: None,
        };
        assert!(client.parse_jwk(&jwk).unwrap().is_none());
    }

    #[test]
    fn parse_jwk_returns_none_when_only_modulus_present() {
        // RSA with `n` but no `e` is malformed; we drop it rather than crashing.
        let client = JwksClient::new("http://example.com".into(), 60).unwrap();
        let jwk = JsonWebKey {
            kid: Some("partial".into()),
            kty: "RSA".into(),
            alg: Some("RS256".into()),
            key_use: Some("sig".into()),
            n: Some("AQAB".into()),
            e: None,
            x5c: None,
        };
        assert!(client.parse_jwk(&jwk).unwrap().is_none());
    }

    #[test]
    fn parse_jwk_x5c_takes_precedence_over_n_e_and_fails_loudly_on_bad_cert() {
        // When x5c is present, the implementation uses it first. A garbage
        // cert string therefore surfaces as KeyParseFailed, not silent
        // fallthrough to the n/e branch (which would otherwise succeed).
        let client = JwksClient::new("http://example.com".into(), 60).unwrap();
        let jwk = JsonWebKey {
            kid: Some("bad-x5c".into()),
            kty: "RSA".into(),
            alg: Some("RS256".into()),
            key_use: Some("sig".into()),
            n: Some(
                "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw"
                    .into(),
            ),
            e: Some("AQAB".into()),
            x5c: Some(vec!["not-a-real-cert".into()]),
        };
        let err = client.parse_jwk(&jwk).unwrap_err();
        assert!(matches!(err, JwksError::KeyParseFailed(_)), "got {err:?}");
    }

    #[test]
    fn jwks_error_display_messages_are_descriptive() {
        // The error messages flow into operator logs; keep their shape stable.
        assert_eq!(
            JwksError::FetchFailed("HTTP 500".into()).to_string(),
            "Failed to fetch JWKS: HTTP 500"
        );
        assert_eq!(
            JwksError::ParseFailed("eof".into()).to_string(),
            "Failed to parse JWKS: eof"
        );
        assert_eq!(
            JwksError::KeyParseFailed("bad PEM".into()).to_string(),
            "Failed to parse key: bad PEM"
        );
        assert_eq!(
            JwksError::KeyNotFound("abc".into()).to_string(),
            "Key not found: abc"
        );
        assert_eq!(
            JwksError::NoKeysAvailable.to_string(),
            "No keys available in JWKS"
        );
        assert_eq!(
            JwksError::HttpClientError("tls".into()).to_string(),
            "Failed to create HTTP client: tls"
        );
    }

    #[tokio::test]
    async fn get_key_returns_cached_match_without_network() {
        // Pre-populate the cache so the read path is exercised without
        // touching the JWKS endpoint. Verifies the cached-key fast path.
        let client = JwksClient::new("http://example.invalid".into(), 3600).unwrap();
        let key = DecodingKey::from_secret(b"placeholder");
        let mut keys = HashMap::new();
        keys.insert("kid-1".to_string(), key);
        *client.cache.write().await = Some(CachedJwks {
            keys,
            fetched_at: Instant::now(),
        });

        // Hit
        let got = client.get_key("kid-1").await;
        assert!(got.is_ok());
    }

    #[tokio::test]
    async fn get_any_key_returns_first_cached_when_kid_absent() {
        // Some providers issue tokens without a `kid` header; the fallback
        // must return whichever key is cached.
        let client = JwksClient::new("http://example.invalid".into(), 3600).unwrap();
        let mut keys = HashMap::new();
        keys.insert("only".into(), DecodingKey::from_secret(b"placeholder"));
        *client.cache.write().await = Some(CachedJwks {
            keys,
            fetched_at: Instant::now(),
        });

        let got = client.get_any_key().await;
        assert!(got.is_ok());
    }
}

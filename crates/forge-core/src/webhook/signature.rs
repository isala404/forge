use std::time::Duration;

/// Default replay window for non-Stripe webhook signature schemes (5 minutes).
/// Stripe enforces its own 5-minute window via the `t=` field in its header
/// and ignores this value.
pub const DEFAULT_REPLAY_WINDOW_SECS: u64 = 300;

/// Header used by non-Stripe schemes to convey the request's unix-seconds
/// timestamp. Senders that don't ship this header are rejected when the
/// scheme requires replay protection.
pub const REPLAY_TIMESTAMP_HEADER: &str = "x-webhook-timestamp";

/// Configuration for webhook signature validation.
#[derive(Debug, Clone)]
pub struct SignatureConfig {
    /// Algorithm used for signature verification.
    pub algorithm: SignatureAlgorithm,
    /// Header name containing the signature.
    pub header_name: &'static str,
    /// Environment variable name containing the secret.
    pub secret_env: &'static str,
    /// Maximum age, in seconds, that a request may have before it is rejected
    /// as a replay. For non-Stripe schemes the runtime reads
    /// `x-webhook-timestamp` and compares to `now`. A value of `0` disables
    /// replay enforcement (not recommended). Stripe always uses its own
    /// 300s window from the `t=` field and ignores this setting.
    pub replay_window_secs: u64,
}

/// Supported signature algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignatureAlgorithm {
    /// HMAC-SHA256 (e.g., GitHub)
    HmacSha256,
    /// Stripe webhook format.
    ///
    /// Signs `{timestamp}.{body}` with HMAC-SHA256. Timestamp and signatures are
    /// extracted from the `Stripe-Signature` header (`t=...,v1=...`). Requests older
    /// than 5 minutes are rejected to prevent replay attacks.
    StripeWebhooks,
    /// HMAC-SHA256 with base64-encoded output (e.g., Shopify).
    ///
    /// Same algorithm as `HmacSha256` but the signature is base64-encoded instead of hex.
    HmacSha256Base64,
    /// Ed25519 asymmetric signature verification.
    ///
    /// The `secret_env` holds a base64-encoded Ed25519 public key (32 bytes).
    /// Used by services that sign with a private key and distribute a public key for verification.
    Ed25519,
}

impl SignatureAlgorithm {
    /// Get the algorithm prefix used in signatures (e.g., "sha256=").
    ///
    /// Returns an empty string for algorithms that don't use a simple prefix.
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::HmacSha256 => "sha256=",
            Self::StripeWebhooks | Self::HmacSha256Base64 | Self::Ed25519 => "",
        }
    }
}

/// Source for extracting idempotency key.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IdempotencySource {
    /// Extract from a header (e.g., "X-Request-Id").
    Header(&'static str),
    /// Extract from request body using JSONPath (e.g., "$.id").
    Body(&'static str),
}

impl IdempotencySource {
    /// Parse from attribute string (e.g., "header:X-Request-Id" or "body:$.id").
    ///
    /// NOTE: This uses `Box::leak` to produce `&'static str` references, so it
    /// must only be called at startup/registration time (e.g., from proc macros),
    /// never in request-handling hot paths. Calling this in a loop will leak memory.
    pub fn parse(s: &str) -> Option<Self> {
        let (prefix, value) = s.split_once(':')?;
        // SAFETY: This is intended to be called only at startup during webhook registration.
        // The leaked strings live for the program's lifetime, matching webhook configurations.
        match prefix {
            "header" => Some(Self::Header(Box::leak(value.to_string().into_boxed_str()))),
            "body" => Some(Self::Body(Box::leak(value.to_string().into_boxed_str()))),
            _ => None,
        }
    }
}

/// Configuration for webhook idempotency.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct IdempotencyConfig {
    /// Source for the idempotency key.
    pub source: IdempotencySource,
    /// TTL for idempotency records (default: 24 hours).
    pub ttl: Duration,
    /// Maximum time a webhook handler can run before the claim is
    /// considered stale and eligible for reclaim. Protects against
    /// process crashes leaving keys permanently locked.
    /// Defaults to 5 minutes.
    pub processing_timeout: Duration,
}

impl IdempotencyConfig {
    /// Create a new idempotency config with default TTL.
    pub fn new(source: IdempotencySource) -> Self {
        Self {
            source,
            ttl: Duration::from_secs(24 * 60 * 60),
            processing_timeout: Duration::from_secs(5 * 60),
        }
    }

    /// Set a custom TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Set a custom processing timeout for crash recovery.
    pub fn with_processing_timeout(mut self, timeout: Duration) -> Self {
        self.processing_timeout = timeout;
        self
    }
}

/// Helper for constructing signature configurations.
///
/// Use in webhook attributes like:
/// ```ignore
/// #[forge::webhook(
///     signature = WebhookSignature::hmac_sha256("X-Stripe-Signature", "STRIPE_SECRET"),
/// )]
/// ```
pub struct WebhookSignature;

impl WebhookSignature {
    /// Create HMAC-SHA256 signature config.
    ///
    /// # Arguments
    /// * `header` - The HTTP header containing the signature (e.g., "X-Hub-Signature-256")
    /// * `secret_env` - Environment variable containing the secret
    pub const fn hmac_sha256(header: &'static str, secret_env: &'static str) -> SignatureConfig {
        SignatureConfig {
            algorithm: SignatureAlgorithm::HmacSha256,
            header_name: header,
            secret_env,
            replay_window_secs: DEFAULT_REPLAY_WINDOW_SECS,
        }
    }

    /// Create a Stripe webhook signature config.
    ///
    /// Signs `{timestamp}.{body}` with HMAC-SHA256. The `Stripe-Signature` header
    /// carries both the timestamp (`t=`) and one or more signatures (`v1=`). Requests
    /// older than 5 minutes are rejected to guard against replay attacks.
    ///
    /// # Arguments
    /// * `secret_env` - Environment variable containing the Stripe webhook signing secret
    pub const fn stripe_webhooks(secret_env: &'static str) -> SignatureConfig {
        SignatureConfig {
            algorithm: SignatureAlgorithm::StripeWebhooks,
            header_name: "stripe-signature",
            secret_env,
            replay_window_secs: DEFAULT_REPLAY_WINDOW_SECS,
        }
    }

    /// Create a Shopify webhook signature config.
    ///
    /// HMAC-SHA256 over the raw body, base64-encoded. The signature arrives in the
    /// `X-Shopify-Hmac-Sha256` header.
    ///
    /// # Arguments
    /// * `secret_env` - Environment variable containing the Shopify webhook secret
    pub const fn shopify_webhooks(secret_env: &'static str) -> SignatureConfig {
        SignatureConfig {
            algorithm: SignatureAlgorithm::HmacSha256Base64,
            header_name: "x-shopify-hmac-sha256",
            secret_env,
            replay_window_secs: DEFAULT_REPLAY_WINDOW_SECS,
        }
    }

    /// Create an Ed25519 asymmetric signature config.
    ///
    /// The service signs the request body with an Ed25519 private key and publishes
    /// the matching public key for you to verify with. The `public_key_env` variable
    /// should hold the base64-encoded 32-byte Ed25519 public key.
    ///
    /// # Arguments
    /// * `header` - The HTTP header containing the base64-encoded signature
    /// * `public_key_env` - Environment variable containing the base64-encoded public key
    pub const fn ed25519(header: &'static str, public_key_env: &'static str) -> SignatureConfig {
        SignatureConfig {
            algorithm: SignatureAlgorithm::Ed25519,
            header_name: header,
            secret_env: public_key_env,
            replay_window_secs: DEFAULT_REPLAY_WINDOW_SECS,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_config_creation() {
        let config = WebhookSignature::hmac_sha256("X-Hub-Signature-256", "GITHUB_SECRET");
        assert_eq!(config.algorithm, SignatureAlgorithm::HmacSha256);
        assert_eq!(config.header_name, "X-Hub-Signature-256");
        assert_eq!(config.secret_env, "GITHUB_SECRET");
    }

    #[test]
    fn test_algorithm_prefix() {
        assert_eq!(SignatureAlgorithm::HmacSha256.prefix(), "sha256=");
        assert_eq!(SignatureAlgorithm::StripeWebhooks.prefix(), "");
        assert_eq!(SignatureAlgorithm::Ed25519.prefix(), "");
    }

    #[test]
    fn test_idempotency_source_parsing() {
        let header = IdempotencySource::parse("header:X-Request-Id");
        assert!(matches!(
            header,
            Some(IdempotencySource::Header("X-Request-Id"))
        ));

        let body = IdempotencySource::parse("body:$.id");
        assert!(matches!(body, Some(IdempotencySource::Body("$.id"))));

        let invalid = IdempotencySource::parse("invalid");
        assert!(invalid.is_none());
    }

    #[test]
    fn test_idempotency_config_default_ttl() {
        let config = IdempotencyConfig::new(IdempotencySource::Header("X-Id"));
        assert_eq!(config.ttl, Duration::from_secs(24 * 60 * 60));
    }
}

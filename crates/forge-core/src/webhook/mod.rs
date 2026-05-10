//! Webhook handling with signature verification.
//!
//! Forge provides a structured way to receive webhooks from external services
//! with automatic signature verification and idempotency handling.
//!
//! # Signature Verification
//!
//! Webhooks can verify signatures using HMAC-SHA256 or other algorithms:
//!
//! ```ignore
//! #[forge::webhook(
//!     path = "/webhooks/stripe",
//!     signature = "hmac_sha256",
//!     secret_env = "STRIPE_WEBHOOK_SECRET"
//! )]
//! async fn handle_stripe(ctx: &WebhookContext, payload: StripeEvent) -> WebhookResult {
//!     // Signature already verified
//! }
//! ```
//!
//! # Idempotency
//!
//! Webhooks can be configured to deduplicate based on a header or payload field:
//!
//! ```ignore
//! #[forge::webhook(
//!     path = "/webhooks/github",
//!     idempotency_key = "header:X-GitHub-Delivery"
//! )]
//! ```
//!
//! # Key Types
//!
//! - [`WebhookContext`] - Request context with headers and raw body
//! - [`SignatureConfig`] - Signature verification settings
//! - [`IdempotencyConfig`] - Deduplication settings

mod context;
mod signature;
mod traits;

pub use context::WebhookContext;
pub use signature::{
    DEFAULT_REPLAY_WINDOW_SECS, IdempotencyConfig, IdempotencySource, REPLAY_TIMESTAMP_HEADER,
    SignatureAlgorithm, SignatureConfig, WebhookSignature,
};
pub use traits::{ForgeWebhook, WebhookInfo, WebhookResult};

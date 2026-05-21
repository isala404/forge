//! Real-time subscription system.
//!
//! Forge provides automatic real-time updates via PostgreSQL LISTEN/NOTIFY.
//! When tables change, affected subscriptions are invalidated and re-executed.
//!
//! # Architecture
//!
//! ```text
//! Client                Gateway                  PostgreSQL
//!   │                      │                         │
//!   │─── Subscribe ───────>│                         │
//!   │                      │── Execute query ───────>│
//!   │<── Initial data ─────│<── Results ────────────│
//!   │                      │                         │
//!   │                      │<── NOTIFY on change ───│
//!   │                      │── Re-execute query ───>│
//!   │<── Delta update ─────│<── New results ────────│
//! ```
//!
//! # Key Types
//!
//! - [`ReadSet`] - Tables a subscription depends on
//! - [`QueryGroup`] - Coalesced subscriptions sharing query+args+auth_scope
//! - [`Subscriber`] - Lightweight subscriber within a query group
//! - [`Delta`] - Change payload sent to clients

mod readset;
mod session;
mod subscription;

pub use readset::{Change, ChangeOperation, ReadSet, TrackingMode};
pub use session::{SessionId, SessionInfo, SessionStatus};
pub use subscription::{
    AuthScope, Delta, QueryGroup, QueryGroupId, Subscriber, SubscriberId, SubscriptionId,
    SubscriptionKey, SubscriptionState,
};

//! Agent-facing trait contracts and their value types.
//!
//! Deliberately free of backend dependencies (no `sqlx`/vendor SDK/backend type);
//! backends in [`crate::backends`] implement these traits. Each trait transcribes
//! `docs/contracts/<name>.md`; the contract wins on any disagreement.

pub mod kv;
pub mod queue;
pub mod types;

pub use kv::{Kv, KvExt, SetMode, SetOpts};
pub use queue::{Backoff, DequeueOpts, EnqueueOpts, Job, JobId, NackOpts, Queue, QueueExt};
pub use types::Cursor;

//! The backend seam: provider lifecycle plus a per-primitive backend report.
//!
//! The primitive traits ([`crate::kv::Kv`], [`crate::queue::Queue`], …) are the operation
//! contracts; they say nothing about how a backend is initialized, health-checked, or
//! swept. [`BackendLifecycle`] is that layer: one value per primitive that knows which
//! provider powers it and how to maintain it. `forge::Forge` holds a
//! `Vec<Arc<dyn BackendLifecycle>>` and drives them from `forge::Forge::maintain` and
//! `forge::Forge::backend_report`.
//!
//! In v1 every primitive is Postgres, except that `blob` can store bytes on a local
//! filesystem instead of `BYTEA`. Adding a second backend for a primitive is a new
//! [`BackendLifecycle`] impl plus a new config variant, nothing more.
//!
//! The [`BackendLifecycle`] impls for the crate-local `Pg*`/`FsBlob` types live here
//! rather than in the per-primitive modules, so each primitive module stays focused on
//! its operation contract. Backends with nothing to sweep inherit the no-op `maintain`
//! default.

use crate::error::Result;
use async_trait::async_trait;
use std::fmt;

/// The eight primitives Forge provides.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    Kv,
    Queue,
    Blob,
    Auth,
    Config,
    RateLimit,
    Schedule,
    Pubsub,
}

impl Primitive {
    /// Stable lowercase identifier (`"kv"`, `"queue"`, …) for logs and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Primitive::Kv => "kv",
            Primitive::Queue => "queue",
            Primitive::Blob => "blob",
            Primitive::Auth => "auth",
            Primitive::Config => "config",
            Primitive::RateLimit => "ratelimit",
            Primitive::Schedule => "schedule",
            Primitive::Pubsub => "pubsub",
        }
    }
}

/// Per-primitive provider lifecycle, beside the operation traits.
///
/// One implementation per configured backend. The defaults let a backend with nothing to
/// sweep be a one-line impl; backends with real maintenance override
/// [`BackendLifecycle::maintain`].
#[async_trait]
pub trait BackendLifecycle: Send + Sync {
    /// Provider id, e.g. `"postgres"` or `"filesystem"`.
    fn name(&self) -> &'static str;

    /// Which primitive this provider powers.
    fn primitive(&self) -> Primitive;

    /// Whether data in this backend survives a restart. `true` for Postgres and the
    /// filesystem blob store.
    fn durable(&self) -> bool {
        true
    }

    /// Short, non-secret operational caveats for the backend report (`"none"` if none).
    fn caveats(&self) -> &'static str {
        "none"
    }

    /// Idempotent maintenance (expiry sweeps, lease reclaim, orphan cleanup). Defaults
    /// to a no-op for backends with nothing to sweep.
    async fn maintain(&self) -> Result<()> {
        Ok(())
    }
}

/// Injection marker: one trait per primitive bundling its operation contract with
/// [`BackendLifecycle`]. Implement both for a type and hand it to
/// [`Forge::builder`](crate::Forge::builder); Forge routes operations to it and drives its
/// maintenance/report like a built-in.
///
/// Each is a marker with a blanket impl, so any type implementing both halves qualifies
/// automatically; implementors never name these traits. A stored `Arc<dyn KvBackend>`
/// upcasts to both `Arc<dyn Kv>` (operations) and `Arc<dyn BackendLifecycle>`
/// (maintenance) with no extra glue.
pub trait KvBackend: crate::kv::Kv + BackendLifecycle {}
impl<T: crate::kv::Kv + BackendLifecycle> KvBackend for T {}

/// See [`KvBackend`]: the injection marker for the queue primitive.
pub trait QueueBackend: crate::queue::Queue + BackendLifecycle {}
impl<T: crate::queue::Queue + BackendLifecycle> QueueBackend for T {}

/// See [`KvBackend`]: the injection marker for the config-store primitive.
pub trait ConfigStoreBackend: crate::config_store::ConfigStore + BackendLifecycle {}
impl<T: crate::config_store::ConfigStore + BackendLifecycle> ConfigStoreBackend for T {}

/// See [`KvBackend`]: the injection marker for the ratelimit primitive.
pub trait RateLimitBackend: crate::ratelimit::RateLimit + BackendLifecycle {}
impl<T: crate::ratelimit::RateLimit + BackendLifecycle> RateLimitBackend for T {}

/// See [`KvBackend`]: the injection marker for the blob primitive. Distinct from the
/// [`BlobBackendConfig`](crate::config::BlobBackendConfig) enum, which only selects where a
/// built-in blob stores bytes.
pub trait BlobBackend: crate::blob::Blob + BackendLifecycle {}
impl<T: crate::blob::Blob + BackendLifecycle> BlobBackend for T {}

/// See [`KvBackend`]: the injection marker for the auth primitive.
pub trait AuthBackend: crate::auth::Auth + BackendLifecycle {}
impl<T: crate::auth::Auth + BackendLifecycle> AuthBackend for T {}

/// See [`KvBackend`]: the injection marker for the schedule primitive.
pub trait ScheduleBackend: crate::schedule::Schedule + BackendLifecycle {}
impl<T: crate::schedule::Schedule + BackendLifecycle> ScheduleBackend for T {}

/// See [`KvBackend`]: the injection marker for the pubsub primitive.
pub trait PubsubBackend: crate::pubsub::Pubsub + BackendLifecycle {}
impl<T: crate::pubsub::Pubsub + BackendLifecycle> PubsubBackend for T {}

/// One line of [`BackendReport`]: which provider powers a primitive and its properties.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub primitive: Primitive,
    pub provider: &'static str,
    pub durable: bool,
    pub caveats: &'static str,
}

impl BackendInfo {
    /// Construct one report line.
    pub fn new(
        primitive: Primitive,
        provider: &'static str,
        durable: bool,
        caveats: &'static str,
    ) -> Self {
        Self {
            primitive,
            provider,
            durable,
            caveats,
        }
    }
}

/// A snapshot of which backend powers each primitive, for logs, health pages, and
/// debugging. Not needed for request handling; the provider must not leak into app logic.
/// See `forge::Forge::backend_report`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackendReport {
    pub backends: Vec<BackendInfo>,
}

impl BackendReport {
    /// Assemble a report from its per-primitive lines.
    pub fn new(backends: Vec<BackendInfo>) -> Self {
        Self { backends }
    }
}

impl fmt::Display for BackendReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "forge backend report:")?;
        for b in &self.backends {
            writeln!(
                f,
                "  {:<10} {:<12} durable={:<3} caveats={}",
                b.primitive.as_str(),
                b.provider,
                if b.durable { "yes" } else { "no" },
                b.caveats,
            )?;
        }
        Ok(())
    }
}

#[async_trait]
impl BackendLifecycle for crate::kv::PgKv {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Kv
    }
    async fn maintain(&self) -> Result<()> {
        self.sweep().await.map(|_| ())
    }
}

#[async_trait]
impl BackendLifecycle for crate::kv::MemKv {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Kv
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process, not shared across replicas"
    }
    async fn maintain(&self) -> Result<()> {
        self.purge_expired();
        Ok(())
    }
}

#[async_trait]
impl BackendLifecycle for crate::queue::PgQueue {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Queue
    }
    async fn maintain(&self) -> Result<()> {
        self.maintenance().await
    }
}

#[async_trait]
impl BackendLifecycle for crate::ratelimit::PgRateLimit {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::RateLimit
    }
    async fn maintain(&self) -> Result<()> {
        self.sweep().await.map(|_| ())
    }
}

#[async_trait]
impl BackendLifecycle for crate::auth::PgAuth {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Auth
    }
    async fn maintain(&self) -> Result<()> {
        self.sweep().await.map(|_| ())
    }
}

#[async_trait]
impl BackendLifecycle for crate::config_store::PgConfig {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Config
    }
}

#[async_trait]
impl BackendLifecycle for crate::schedule::PgSchedule {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Schedule
    }
}

#[async_trait]
impl BackendLifecycle for crate::pubsub::PgPubsub {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Pubsub
    }
    fn caveats(&self) -> &'static str {
        "at-most-once, non-durable"
    }
}

#[async_trait]
impl BackendLifecycle for crate::blob::PgBlob {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Blob
    }
}

#[async_trait]
impl BackendLifecycle for crate::blob::FsBlob {
    fn name(&self) -> &'static str {
        "filesystem"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Blob
    }
    fn caveats(&self) -> &'static str {
        "local-dir, shared-mount-for-multi-replica, put-not-atomic-with-app-sql"
    }
    async fn maintain(&self) -> Result<()> {
        self.sweep_orphans().await.map(|_| ())
    }
}

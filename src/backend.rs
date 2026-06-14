//! The backend seam: provider lifecycle + a per-primitive backend report.
//!
//! The primitive traits ([`crate::Kv`], [`crate::Queue`], …) are the *operation*
//! contracts. They say nothing about how a backend is initialized, health-checked, or
//! swept. [`BackendLifecycle`] is that missing layer: one value per primitive that
//! knows which provider powers it and how to maintain it. [`crate::Forge`] holds a
//! `Vec<Arc<dyn BackendLifecycle>>` and drives them from [`crate::Forge::maintain`] and
//! [`crate::Forge::backend_report`].
//!
//! In v1 every primitive is Postgres, with the one exception that `blob` can store
//! bytes on a local filesystem instead of `BYTEA`. The seam is built so a second
//! backend for any single primitive is a later, app-code-invisible addition: a new
//! [`BackendLifecycle`] impl and a new config variant, nothing more.

use crate::error::Result;
use async_trait::async_trait;
use std::fmt;

/// The eight primitives Forge provides. Used by [`BackendLifecycle`] and the backend
/// report to name which slot a provider fills.
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

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The result of a backend health probe.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackendHealth {
    /// Whether the backend is currently usable.
    pub healthy: bool,
    /// Human-readable detail for logs/health pages (never secrets).
    pub detail: String,
}

impl BackendHealth {
    /// A healthy result with no extra detail.
    pub fn ok() -> Self {
        Self {
            healthy: true,
            detail: String::new(),
        }
    }
}

/// Per-primitive provider lifecycle, beside the operation traits.
///
/// One implementation exists per configured backend. The defaults make a no-op
/// backend (no init, healthy, nothing to sweep) a one-line impl; backends with real
/// maintenance (the Postgres sweeps, the filesystem orphan sweep) override
/// [`BackendLifecycle::maintain`].
#[async_trait]
pub trait BackendLifecycle: Send + Sync {
    /// Provider id, e.g. `"postgres"` or `"filesystem"`.
    fn name(&self) -> &'static str;

    /// Which primitive this provider powers.
    fn primitive(&self) -> Primitive;

    /// Whether data in this backend survives a restart. `true` for Postgres and the
    /// filesystem blob store; a future in-memory/Redis-without-persistence backend
    /// would say `false`.
    fn durable(&self) -> bool {
        true
    }

    /// Short, non-secret operational caveats for the backend report (`"none"` if none).
    fn caveats(&self) -> &'static str {
        "none"
    }

    /// One-time initialization. Postgres backends migrate via the shared runner, so
    /// this defaults to a no-op; external providers can use it.
    async fn init(&self) -> Result<()> {
        Ok(())
    }

    /// Liveness probe. Defaults to healthy; backends override to actually check.
    async fn health(&self) -> Result<BackendHealth> {
        Ok(BackendHealth::ok())
    }

    /// Idempotent maintenance (expiry sweeps, lease reclaim, orphan cleanup). Defaults
    /// to a no-op for backends with nothing to sweep.
    async fn maintain(&self) -> Result<()> {
        Ok(())
    }
}

/// One line of [`BackendReport`]: which provider powers a primitive and its properties.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub primitive: Primitive,
    pub provider: &'static str,
    pub durable: bool,
    pub caveats: &'static str,
}

/// A snapshot of which backend powers each primitive — for logs, health pages, and
/// debugging. Never needed for ordinary request handling (the provider must not leak
/// into app logic); see [`crate::Forge::backend_report`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackendReport {
    pub backends: Vec<BackendInfo>,
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

// --- Postgres + filesystem backend lifecycle impls -------------------------------
//
// Centralized here (both the trait and the Pg*/FsBlob types are crate-local, so the
// orphan rule allows it) to keep the per-primitive modules focused on their operation
// contracts. The maintenance arms call each backend's inherent sweep; backends with
// nothing to sweep inherit the no-op default.

#[cfg(feature = "postgres")]
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

#[cfg(feature = "postgres")]
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

#[cfg(feature = "postgres")]
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

#[cfg(feature = "postgres")]
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

#[cfg(feature = "postgres")]
#[async_trait]
impl BackendLifecycle for crate::config_store::PgConfig {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Config
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl BackendLifecycle for crate::schedule::PgSchedule {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Schedule
    }
}

#[cfg(feature = "postgres")]
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

#[cfg(feature = "postgres")]
#[async_trait]
impl BackendLifecycle for crate::blob::PgBlob {
    fn name(&self) -> &'static str {
        "postgres"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Blob
    }
}

#[cfg(feature = "postgres")]
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

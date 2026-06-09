//! `schedule` — lineage: cron + Unix `at` + Kubernetes CronJob. See
//! `docs/contracts/schedule.md`.
//!
//! A thin layer over [`crate::queue`]: a due tick enqueues a job, so all of the
//! queue's at-least-once / retry / DLQ semantics are inherited. A scheduled job is
//! delivered **at least once** — consumers must be idempotent.

use crate::error::Result;
use crate::queue::JobId;
use async_trait::async_trait;
use bytes::Bytes;
use std::time::SystemTime;

mod cron;

#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub(crate) use pg::PgSchedule;

/// Largest allowed schedule name, in bytes. Over => [`crate::ForgeError::Limit`].
pub const MAX_NAME_BYTES: usize = 256;

/// What a registered schedule is.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleKind {
    /// A recurring 5-field cron expression (UTC), carried verbatim.
    Cron(String),
    /// A one-shot enqueue at a fixed time.
    At,
}

/// A registered schedule, returned by [`Schedule::list`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ScheduleInfo {
    /// Unique name (caller-chosen for cron, generated for `at`).
    pub name: String,
    /// Whether this is a cron or a one-shot.
    pub kind: ScheduleKind,
    /// The target queue ticks are enqueued into.
    pub queue: String,
    /// The next time this schedule will fire.
    pub next_run: SystemTime,
    /// The last time it fired, if ever.
    pub last_run: Option<SystemTime>,
}

/// Recurring and one-shot future work. Lineage: cron + Unix `at` + k8s CronJob.
/// Object-safe; the facade hands out `Arc<dyn Schedule>`.
///
/// The ticker that fires due schedules runs via `forge.run_scheduler()` (managed
/// loop) or `forge.run_scheduler_once()` (one pass). Exactly one enqueue happens per
/// tick across all replicas. Exact semantics: `docs/contracts/schedule.md`.
#[async_trait]
pub trait Schedule: Send + Sync {
    /// Upsert a recurring cron schedule by `name` (re-registering replaces it). The
    /// 5-field `expr` (UTC) is validated now; an invalid one is
    /// [`crate::ForgeError::Invalid`].
    async fn cron(&self, name: &str, expr: &str, queue: &str, payload: Bytes) -> Result<()>;

    /// Schedule a one-shot enqueue at `when`. Returns the [`JobId`] the eventual
    /// queue job will carry.
    async fn at(&self, when: SystemTime, queue: &str, payload: Bytes) -> Result<JobId>;

    /// Cancel a schedule by name. `true` if one was removed, `false` if absent.
    async fn cancel(&self, name: &str) -> Result<bool>;

    /// List all registered schedules.
    async fn list(&self) -> Result<Vec<ScheduleInfo>>;
}

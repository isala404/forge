use crate::error::Result;
use crate::queue::JobId;
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use std::time::SystemTime;

/// Largest allowed schedule name, in bytes. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_NAME_BYTES: usize = 256;

/// Largest accepted `at` horizon: ~100 years from now, in days (365.25 × 100). A `when`
/// past this is [`crate::error::ForgeError::Limit`]: a time a century out is effectively
/// always a bug, and a fixed ceiling keeps backends in agreement (same rationale as the
/// kv TTL ceiling). A past/now `when` is not rejected: it fires on the next tick if
/// within the missed-tick grace, else is skipped and logged.
pub const MAX_AT_HORIZON_DAYS: i64 = 36525;

/// What a registered schedule is.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleKind {
    /// A recurring 5-field cron expression (UTC), carried verbatim.
    Cron(String),
    /// A one-shot enqueue at a fixed time.
    At,
}

/// Per-schedule delivery options applied to the queue job each tick enqueues. An unset
/// field inherits the queue's own default (`max_attempts = 5`), matching a plain
/// [`crate::queue::Queue::enqueue`].
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct ScheduleOpts {
    /// Max delivery attempts for the enqueued job before it dead-letters. `None` =
    /// the queue default (5).
    pub max_attempts: Option<u32>,
}

impl ScheduleOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }
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

impl ScheduleInfo {
    /// Construct a registered-schedule snapshot. For backend implementors; app code
    /// receives this from [`Schedule::list`].
    pub fn new(
        name: String,
        kind: ScheduleKind,
        queue: String,
        next_run: SystemTime,
        last_run: Option<SystemTime>,
    ) -> Self {
        Self {
            name,
            kind,
            queue,
            next_run,
            last_run,
        }
    }
}

/// Recurring and one-shot future work. Lineage: cron + Unix `at` + k8s CronJob.
/// Object-safe; the facade hands out `Arc<dyn Schedule>`.
///
/// The ticker that fires due schedules runs via `forge.run_scheduler()` (managed
/// loop) or `forge.run_scheduler_once()` (one pass). Built-in backends use stable queue
/// job ids so retrying a claimed tick is idempotent; custom queue backends must honor
/// `EnqueueOpts::job_id`. Exact semantics: <https://tryforge.dev/primitives/#schedule>.
#[async_trait]
pub trait Schedule: Send + Sync {
    /// Upsert a recurring cron schedule by `name` (re-registering replaces it). The
    /// 5-field `expr` (UTC) is validated now; an invalid one is
    /// [`crate::error::ForgeError::Invalid`]. `opts` controls the delivery options of the job
    /// each tick enqueues (pass [`ScheduleOpts::new`] for the queue defaults).
    async fn cron(
        &self,
        name: &str,
        expr: &str,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<()>;

    /// Schedule a one-shot enqueue at `when`. Returns the [`JobId`] the eventual
    /// queue job will carry. A `when` already in the past (or now) is accepted and
    /// fires on the next tick if within the missed-tick grace; a `when` more than
    /// [`MAX_AT_HORIZON_DAYS`] out is [`crate::error::ForgeError::Limit`]. `opts` controls the
    /// enqueued job's delivery options (pass [`ScheduleOpts::new`] for the defaults).
    async fn at(
        &self,
        when: SystemTime,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<JobId>;

    /// Cancel a schedule by name. `true` if one was removed, `false` if absent.
    async fn cancel(&self, name: &str) -> Result<bool>;

    /// Cancel a one-shot created by [`Schedule::at`], by the [`JobId`] it returned.
    /// `true` if it was still pending and removed, `false` if it already fired or
    /// never existed.
    async fn cancel_at(&self, job_id: JobId) -> Result<bool> {
        self.cancel(&format!("at:{job_id}")).await
    }

    /// List registered schedules, ordered by name, up to `limit` per page plus a next-page
    /// cursor (`None` when iteration is complete). Weakly consistent: tolerate duplicates
    /// across pages.
    async fn list(
        &self,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<ScheduleInfo>, Option<Cursor>)>;

    /// Run one scheduler pass: fire every due schedule once, returning how many jobs
    /// were enqueued. Drive it via `forgelib::Forge::run_scheduler` /
    /// `forgelib::Forge::run_scheduler_once`; safe to run concurrently across replicas,
    /// since each due row is claimed exactly once.
    async fn process_due(&self) -> Result<u64>;
}

mod cron;

mod memory;
mod postgres;
pub(crate) use memory::MemSchedule;
pub(crate) use postgres::PgSchedule;

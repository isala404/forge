use crate::error::Result;
use crate::queue::JobId;
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use std::time::{Duration, SystemTime};

/// Largest allowed schedule name, in bytes. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_NAME_BYTES: usize = 256;

/// Largest accepted `at` horizon: ~100 years from now, in days (365.25 × 100). A `when`
/// past this is [`crate::error::ForgeError::Limit`]: a time a century out is effectively
/// always a bug, and a fixed ceiling keeps backends in agreement (same rationale as the
/// kv TTL ceiling). A past/now `when` is not rejected: its explicit misfire policy
/// decides whether the next scheduler tick enqueues or skips it.
pub const MAX_AT_HORIZON_DAYS: i64 = 36525;

/// Hard ceiling for a catch-up policy. A restart can never enqueue more missed
/// occurrences than this for one schedule in one tick.
pub const MAX_CATCH_UP: u32 = 100;

/// How a late schedule handles occurrences missed while no scheduler was running.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MisfirePolicy {
    /// Drop every missed occurrence and continue with the next future occurrence.
    Skip,
    /// Enqueue only the most recent missed occurrence.
    #[default]
    RunOnce,
    /// Enqueue at most this many of the most recent missed occurrences.
    CatchUp(u32),
}

impl MisfirePolicy {
    pub(crate) fn validate(self) -> Result<Self> {
        match self {
            Self::CatchUp(0) => Err(crate::error::ForgeError::invalid(
                "catch-up count must be greater than zero",
            )),
            Self::CatchUp(count) if count > MAX_CATCH_UP => Err(crate::error::ForgeError::limit(
                format!("catch-up count is {count}; max is {MAX_CATCH_UP}"),
            )),
            value => Ok(value),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::RunOnce => "run_once",
            Self::CatchUp(_) => "catch_up",
        }
    }

    pub fn max_catch_up(self) -> u32 {
        match self {
            Self::CatchUp(count) => count,
            Self::Skip | Self::RunOnce => 0,
        }
    }
}

/// Select bounded, deterministic cron occurrences and the next future tick.
pub(crate) fn plan_cron_occurrences(
    cron: &cron::Cron,
    first_due: DateTime<Utc>,
    now: DateTime<Utc>,
    policy: MisfirePolicy,
) -> (Vec<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let next = cron.next_after(now);
    if first_due > now || policy == MisfirePolicy::Skip {
        return (Vec::new(), next);
    }
    let Some(latest) = cron.prev_or_at(now).filter(|latest| *latest >= first_due) else {
        return (Vec::new(), next);
    };
    let count = match policy {
        MisfirePolicy::Skip => 0,
        MisfirePolicy::RunOnce => 1,
        MisfirePolicy::CatchUp(count) => count,
    };
    let mut occurrences = Vec::with_capacity(count as usize);
    let mut current = latest;
    for _ in 0..count {
        if current < first_due {
            break;
        }
        occurrences.push(current);
        let Some(previous_probe) = current.checked_sub_signed(chrono::Duration::minutes(1)) else {
            break;
        };
        let Some(previous) = cron.prev_or_at(previous_probe) else {
            break;
        };
        current = previous;
    }
    occurrences.reverse();
    (occurrences, next)
}

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
    /// Explicit late-occurrence behavior. Defaults to [`MisfirePolicy::RunOnce`].
    pub misfire_policy: MisfirePolicy,
}

impl ScheduleOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }

    pub fn with_misfire_policy(mut self, policy: MisfirePolicy) -> Self {
        self.misfire_policy = policy;
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
    /// Paused schedules remain inspectable but are not considered due.
    pub paused: bool,
    /// Explicit missed-occurrence behavior.
    pub misfire_policy: MisfirePolicy,
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
        paused: bool,
        misfire_policy: MisfirePolicy,
    ) -> Self {
        Self {
            name,
            kind,
            queue,
            next_run,
            last_run,
            paused,
            misfire_policy,
        }
    }
}

/// Bounded operational state for one scheduler namespace.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct SchedulerDiagnostics {
    /// Age of the oldest unpaused due occurrence, or `None` when nothing is due.
    pub lag: Option<Duration>,
    /// Completion time of the most recent successful scheduler pass.
    pub last_successful_tick: Option<SystemTime>,
    /// Number of unpaused schedules currently due.
    pub due_count: u64,
    /// Cumulative queue-enqueue failures observed by scheduler passes.
    pub enqueue_failures: u64,
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
    /// handled by `opts.misfire_policy` on the next scheduler tick; a `when` more than
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

    /// Inspect one named schedule without pagination.
    async fn inspect(&self, name: &str) -> Result<Option<ScheduleInfo>>;

    /// Pause a named schedule. Returns `false` when it does not exist.
    async fn pause(&self, name: &str) -> Result<bool>;

    /// Resume a named schedule. Returns `false` when it does not exist.
    async fn resume(&self, name: &str) -> Result<bool>;

    /// Read bounded scheduler health and backlog data.
    async fn diagnostics(&self) -> Result<SchedulerDiagnostics>;

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

pub(crate) mod cron;

mod memory;
mod postgres;
pub(crate) use memory::MemSchedule;
pub(crate) use postgres::PgSchedule;

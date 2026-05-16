use std::future::Future;
use std::pin::Pin;

use super::context::CronContext;
use super::schedule::CronSchedule;
use crate::Result;
use crate::metadata::HandlerMetadata;

/// Trait for cron job handlers.
///
/// Crons are dispatched as `$cron:{name}` jobs for execution but retain a
/// separate trait for ergonomic configuration (schedule expression, timezone,
/// catch-up). The execution model is unified through the job queue: the cron
/// scheduler claims a run slot and enqueues a bridge job, which the worker
/// pool executes with retry and timeout semantics inherited from `JobInfo`.
pub trait ForgeCron: crate::__sealed::Sealed + Send + Sync + 'static {
    /// Reserved for future parameterized cron input.
    type Args: serde::de::DeserializeOwned + Send + Sync + 'static;

    /// Get cron metadata.
    fn info() -> CronInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    /// Execute the cron job.
    fn execute(ctx: &CronContext) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// Cron job metadata.
///
/// Constructed by the `#[cron]` macro. Adding a field is a breaking change for
/// hand-written `ForgeCron` impls; stage extensions through a builder or major
/// bump.
#[derive(Debug, Clone)]
pub struct CronInfo {
    /// Cron name (function name).
    pub name: &'static str,
    /// Cron schedule expression.
    pub schedule: CronSchedule,
    /// Timezone for the schedule.
    pub timezone: &'static str,
    /// Leadership group for sharded leader election.
    pub group: &'static str,
    /// Whether to catch up missed runs.
    pub catch_up: bool,
    /// Maximum number of missed runs to catch up.
    pub catch_up_limit: u32,
    /// Timeout for execution.
    pub timeout: std::time::Duration,
    /// Default timeout for outbound HTTP requests made by this cron.
    pub http_timeout: Option<std::time::Duration>,
}

impl Default for CronInfo {
    fn default() -> Self {
        Self {
            name: "",
            schedule: CronSchedule::default(),
            timezone: "UTC",
            group: "default",
            catch_up: false,
            catch_up_limit: 10,
            timeout: std::time::Duration::from_secs(3600),
            http_timeout: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_info_default() {
        let info = CronInfo::default();
        assert_eq!(info.name, "");
        assert_eq!(info.timezone, "UTC");
        assert!(!info.catch_up);
        assert_eq!(info.catch_up_limit, 10);
    }
}

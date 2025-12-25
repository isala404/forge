use std::future::Future;
use std::pin::Pin;

use super::context::CronContext;
use super::schedule::CronSchedule;
use crate::Result;

/// Trait for cron job handlers.
pub trait ForgeCron: Send + Sync + 'static {
    /// Get cron metadata.
    fn info() -> CronInfo;

    /// Execute the cron job.
    fn execute(ctx: &CronContext) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// Cron job metadata.
#[derive(Debug, Clone)]
pub struct CronInfo {
    /// Cron name (function name).
    pub name: &'static str,
    /// Cron schedule expression.
    pub schedule: CronSchedule,
    /// Timezone for the schedule.
    pub timezone: &'static str,
    /// Whether to catch up missed runs.
    pub catch_up: bool,
    /// Maximum number of missed runs to catch up.
    pub catch_up_limit: u32,
    /// Timeout for execution.
    pub timeout: std::time::Duration,
}

impl Default for CronInfo {
    fn default() -> Self {
        Self {
            name: "",
            schedule: CronSchedule::default(),
            timezone: "UTC",
            catch_up: false,
            catch_up_limit: 10,
            timeout: std::time::Duration::from_secs(3600),
        }
    }
}

#[cfg(test)]
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

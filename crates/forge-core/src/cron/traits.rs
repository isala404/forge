use std::future::Future;
use std::pin::Pin;

use super::context::CronContext;
use super::schedule::CronSchedule;
use crate::Result;

/// Trait for scheduled cron handlers.
pub trait ForgeCron: crate::__sealed::Sealed + Send + Sync + 'static {
    type Args: serde::de::DeserializeOwned + Send + Sync + 'static;

    fn info() -> CronInfo;

    fn execute(ctx: &CronContext) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// Metadata for a registered cron handler.
#[derive(Debug, Clone)]
pub struct CronInfo {
    pub name: &'static str,
    pub schedule: CronSchedule,
    pub timezone: &'static str,
    /// Leadership group for sharded leader election.
    pub group: &'static str,
    pub catch_up: bool,
    pub catch_up_limit: u32,
    pub timeout: std::time::Duration,
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

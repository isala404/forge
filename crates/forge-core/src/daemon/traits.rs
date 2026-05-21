use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use crate::error::Result;

use super::context::DaemonContext;

/// Trait for long-running daemon handlers.
pub trait ForgeDaemon: crate::__sealed::Sealed + Send + Sync + 'static {
    fn info() -> DaemonInfo;

    fn execute(ctx: &DaemonContext) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// Metadata for a registered daemon handler.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub name: &'static str,
    pub leader_elected: bool,
    pub restart_on_panic: bool,
    pub restart_delay: Duration,
    pub startup_delay: Duration,
    pub http_timeout: Option<Duration>,
    /// `None` means unlimited.
    pub max_restarts: Option<u32>,
}

impl Default for DaemonInfo {
    fn default() -> Self {
        Self {
            name: "",
            leader_elected: true,
            restart_on_panic: true,
            restart_delay: Duration::from_secs(5),
            startup_delay: Duration::from_secs(0),
            http_timeout: None,
            max_restarts: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DaemonStatus {
    Pending,
    Acquiring,
    Running,
    Stopped,
    Failed,
    Restarting,
}

impl DaemonStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acquiring => "acquiring",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Restarting => "restarting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDaemonStatusError(pub String);

impl std::fmt::Display for ParseDaemonStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown daemon status: {:?}", self.0)
    }
}

impl std::error::Error for ParseDaemonStatusError {}

impl FromStr for DaemonStatus {
    type Err = ParseDaemonStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "acquiring" => Ok(Self::Acquiring),
            "running" => Ok(Self::Running),
            "stopped" => Ok(Self::Stopped),
            "failed" => Ok(Self::Failed),
            "restarting" => Ok(Self::Restarting),
            _ => Err(ParseDaemonStatusError(s.to_string())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_default_daemon_info() {
        let info = DaemonInfo::default();
        assert!(info.leader_elected);
        assert!(info.restart_on_panic);
        assert_eq!(info.restart_delay, Duration::from_secs(5));
        assert_eq!(info.startup_delay, Duration::from_secs(0));
        assert_eq!(info.http_timeout, None);
        assert!(info.max_restarts.is_none());
    }

    #[test]
    fn test_status_conversion() {
        assert_eq!(DaemonStatus::Running.as_str(), "running");
        assert_eq!("running".parse::<DaemonStatus>(), Ok(DaemonStatus::Running));
        assert_eq!(DaemonStatus::Failed.as_str(), "failed");
        assert_eq!("failed".parse::<DaemonStatus>(), Ok(DaemonStatus::Failed));
    }
}

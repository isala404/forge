use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

use crate::error::Result;

use super::context::JobContext;

/// Trait for FORGE job handlers.
pub trait ForgeJob: Send + Sync + 'static {
    /// Input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// Output result type.
    type Output: Serialize + Send;

    /// Get job metadata.
    fn info() -> JobInfo;

    /// Execute the job.
    fn execute(
        ctx: &JobContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// Job metadata.
#[derive(Debug, Clone)]
pub struct JobInfo {
    /// Job name (used for routing).
    pub name: &'static str,
    /// Job timeout.
    pub timeout: Duration,
    /// Default priority.
    pub priority: JobPriority,
    /// Retry configuration.
    pub retry: RetryConfig,
    /// Required worker capability (e.g., "general", "media", "ml").
    pub worker_capability: Option<&'static str>,
    /// Whether to deduplicate by idempotency key.
    pub idempotent: bool,
    /// Idempotency key field path.
    pub idempotency_key: Option<&'static str>,
}

impl Default for JobInfo {
    fn default() -> Self {
        Self {
            name: "",
            timeout: Duration::from_secs(3600), // 1 hour default
            priority: JobPriority::Normal,
            retry: RetryConfig::default(),
            worker_capability: None,
            idempotent: false,
            idempotency_key: None,
        }
    }
}

/// Job priority levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobPriority {
    Background = 0,
    Low = 25,
    Normal = 50,
    High = 75,
    Critical = 100,
}

impl Default for JobPriority {
    fn default() -> Self {
        Self::Normal
    }
}

impl JobPriority {
    /// Get numeric value for database storage.
    pub fn as_i32(&self) -> i32 {
        *self as i32
    }

    /// Parse from numeric value.
    pub fn from_i32(value: i32) -> Self {
        match value {
            0..=12 => Self::Background,
            13..=37 => Self::Low,
            38..=62 => Self::Normal,
            63..=87 => Self::High,
            _ => Self::Critical,
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "background" => Self::Background,
            "low" => Self::Low,
            "normal" => Self::Normal,
            "high" => Self::High,
            "critical" => Self::Critical,
            _ => Self::Normal,
        }
    }
}

/// Job status in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Waiting to be claimed.
    Pending,
    /// Claimed by a worker.
    Claimed,
    /// Currently executing.
    Running,
    /// Successfully completed.
    Completed,
    /// Failed, will retry.
    Retry,
    /// Failed permanently.
    Failed,
    /// Moved to dead letter queue.
    DeadLetter,
}

impl JobStatus {
    /// Convert to database string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Retry => "retry",
            Self::Failed => "failed",
            Self::DeadLetter => "dead_letter",
        }
    }

    /// Parse from database string.
    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "claimed" => Self::Claimed,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "retry" => Self::Retry,
            "failed" => Self::Failed,
            "dead_letter" => Self::DeadLetter,
            _ => Self::Pending,
        }
    }
}

/// Retry configuration for jobs.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts.
    pub max_attempts: u32,
    /// Backoff strategy.
    pub backoff: BackoffStrategy,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
    /// Error types to retry on (empty = all errors).
    pub retry_on: Vec<String>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffStrategy::Exponential,
            max_backoff: Duration::from_secs(300), // 5 minutes
            retry_on: Vec::new(),                  // Retry on all errors
        }
    }
}

impl RetryConfig {
    /// Calculate backoff duration for a given attempt.
    pub fn calculate_backoff(&self, attempt: u32) -> Duration {
        let base = Duration::from_secs(1);
        let backoff = match self.backoff {
            BackoffStrategy::Fixed => base,
            BackoffStrategy::Linear => base * attempt,
            BackoffStrategy::Exponential => base * 2u32.pow(attempt.saturating_sub(1)),
        };
        backoff.min(self.max_backoff)
    }
}

/// Backoff strategy for retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffStrategy {
    /// Same delay each time.
    Fixed,
    /// Delay increases linearly.
    Linear,
    /// Delay doubles each time.
    Exponential,
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self::Exponential
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        assert!(JobPriority::Critical > JobPriority::High);
        assert!(JobPriority::High > JobPriority::Normal);
        assert!(JobPriority::Normal > JobPriority::Low);
        assert!(JobPriority::Low > JobPriority::Background);
    }

    #[test]
    fn test_priority_conversion() {
        assert_eq!(JobPriority::Critical.as_i32(), 100);
        assert_eq!(JobPriority::Normal.as_i32(), 50);
        assert_eq!(JobPriority::from_i32(100), JobPriority::Critical);
        assert_eq!(JobPriority::from_i32(50), JobPriority::Normal);
    }

    #[test]
    fn test_status_conversion() {
        assert_eq!(JobStatus::Pending.as_str(), "pending");
        assert_eq!(JobStatus::from_str("pending"), JobStatus::Pending);
        assert_eq!(JobStatus::DeadLetter.as_str(), "dead_letter");
        assert_eq!(JobStatus::from_str("dead_letter"), JobStatus::DeadLetter);
    }

    #[test]
    fn test_exponential_backoff() {
        let config = RetryConfig::default();
        assert_eq!(config.calculate_backoff(1), Duration::from_secs(1));
        assert_eq!(config.calculate_backoff(2), Duration::from_secs(2));
        assert_eq!(config.calculate_backoff(3), Duration::from_secs(4));
        assert_eq!(config.calculate_backoff(4), Duration::from_secs(8));
    }

    #[test]
    fn test_max_backoff_cap() {
        let config = RetryConfig {
            max_backoff: Duration::from_secs(10),
            ..Default::default()
        };
        assert_eq!(config.calculate_backoff(10), Duration::from_secs(10));
    }
}

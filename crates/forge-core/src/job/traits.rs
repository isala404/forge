use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use crate::error::Result;
use crate::metadata::HandlerMetadata;

use super::context::JobContext;

/// Trait for FORGE job handlers.
pub trait ForgeJob: crate::__sealed::Sealed + Send + Sync + 'static {
    /// Input arguments type.
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// Output result type.
    type Output: Serialize + Send;

    /// Get job metadata.
    fn info() -> JobInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    /// Execute the job.
    fn execute(
        ctx: &JobContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;

    /// Compensate a cancelled job.
    fn compensate<'a>(
        _ctx: &'a JobContext,
        _args: Self::Args,
        _reason: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// Job metadata.
///
/// Constructed by the `#[job]` macro. Adding a field is a breaking change for
/// hand-written `ForgeJob` impls; stage extensions through a builder or major
/// bump.
#[derive(Debug, Clone)]
pub struct JobInfo {
    /// Job name (used for routing).
    pub name: &'static str,
    /// Human-readable description of the job's purpose.
    pub description: Option<&'static str>,
    /// Job timeout.
    pub timeout: Duration,
    /// Default timeout for outbound HTTP requests made by this job.
    pub http_timeout: Option<Duration>,
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
    /// Whether the job is public (no auth required).
    pub is_public: bool,
    /// Required role for authorization (implies auth required).
    pub required_role: Option<&'static str>,
    /// Time-to-live after completion. Job records are cleaned up after this duration.
    /// None means records are kept indefinitely.
    pub ttl: Option<Duration>,
}

impl Default for JobInfo {
    fn default() -> Self {
        Self {
            name: "",
            description: None,
            timeout: Duration::from_secs(3600), // 1 hour default
            http_timeout: None,
            priority: JobPriority::Normal,
            retry: RetryConfig::default(),
            worker_capability: None,
            idempotent: false,
            idempotency_key: None,
            is_public: false,
            required_role: None,
            ttl: None,
        }
    }
}

/// Job priority levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[non_exhaustive]
pub enum JobPriority {
    Background = 0,
    Low = 25,
    #[default]
    Normal = 50,
    High = 75,
    Critical = 100,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseJobPriorityError(pub String);

impl std::fmt::Display for ParseJobPriorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid job priority: '{}'", self.0)
    }
}

impl std::error::Error for ParseJobPriorityError {}

impl FromStr for JobPriority {
    type Err = ParseJobPriorityError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "background" => Ok(Self::Background),
            "low" => Ok(Self::Low),
            "normal" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            _ => Err(ParseJobPriorityError(s.to_string())),
        }
    }
}

/// Job status in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// Cancellation requested for a running job.
    CancelRequested,
    /// Job cancelled.
    Cancelled,
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
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseJobStatusError(pub String);

impl std::fmt::Display for ParseJobStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid job status: '{}'", self.0)
    }
}

impl std::error::Error for ParseJobStatusError {}

impl FromStr for JobStatus {
    type Err = ParseJobStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "retry" => Ok(Self::Retry),
            "failed" => Ok(Self::Failed),
            "dead_letter" => Ok(Self::DeadLetter),
            "cancel_requested" => Ok(Self::CancelRequested),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(ParseJobStatusError(s.to_string())),
        }
    }
}

/// Retry configuration for jobs.
///
/// Constructed by the `#[job]` macro. Adding a field is a breaking change
/// for hand-written `ForgeJob` impls; stage extensions through a builder
/// or major bump.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BackoffStrategy {
    /// Same delay each time.
    Fixed,
    /// Delay increases linearly.
    Linear,
    /// Delay doubles each time.
    #[default]
    Exponential,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
        assert_eq!("pending".parse::<JobStatus>(), Ok(JobStatus::Pending));
        assert_eq!(JobStatus::DeadLetter.as_str(), "dead_letter");
        assert_eq!(
            "dead_letter".parse::<JobStatus>(),
            Ok(JobStatus::DeadLetter)
        );
        assert_eq!(JobStatus::CancelRequested.as_str(), "cancel_requested");
        assert_eq!(
            "cancel_requested".parse::<JobStatus>(),
            Ok(JobStatus::CancelRequested)
        );
        assert_eq!(JobStatus::Cancelled.as_str(), "cancelled");
        assert_eq!("cancelled".parse::<JobStatus>(), Ok(JobStatus::Cancelled));
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

    #[test]
    fn priority_from_i32_covers_all_buckets() {
        // Boundaries derived from the impl: 0..=12 Background, 13..=37 Low,
        // 38..=62 Normal, 63..=87 High, _ Critical (incl. negatives).
        assert_eq!(JobPriority::from_i32(0), JobPriority::Background);
        assert_eq!(JobPriority::from_i32(12), JobPriority::Background);
        assert_eq!(JobPriority::from_i32(13), JobPriority::Low);
        assert_eq!(JobPriority::from_i32(25), JobPriority::Low);
        assert_eq!(JobPriority::from_i32(37), JobPriority::Low);
        assert_eq!(JobPriority::from_i32(38), JobPriority::Normal);
        assert_eq!(JobPriority::from_i32(62), JobPriority::Normal);
        assert_eq!(JobPriority::from_i32(63), JobPriority::High);
        assert_eq!(JobPriority::from_i32(87), JobPriority::High);
        assert_eq!(JobPriority::from_i32(88), JobPriority::Critical);
        assert_eq!(JobPriority::from_i32(1_000_000), JobPriority::Critical);
        // Negatives fall through to Critical via the wildcard arm.
        assert_eq!(JobPriority::from_i32(-1), JobPriority::Critical);
    }

    #[test]
    fn priority_default_is_normal() {
        assert_eq!(JobPriority::default(), JobPriority::Normal);
    }

    #[test]
    fn priority_round_trips_through_i32_buckets() {
        // The constructed values are at bucket centers, so the round trip is
        // lossless for the canonical numeric encodings.
        for variant in [
            JobPriority::Background,
            JobPriority::Low,
            JobPriority::Normal,
            JobPriority::High,
            JobPriority::Critical,
        ] {
            assert_eq!(JobPriority::from_i32(variant.as_i32()), variant);
        }
    }

    #[test]
    fn priority_from_str_is_case_insensitive_for_all_variants() {
        assert_eq!(
            "background".parse::<JobPriority>(),
            Ok(JobPriority::Background)
        );
        assert_eq!("Low".parse::<JobPriority>(), Ok(JobPriority::Low));
        assert_eq!("NORMAL".parse::<JobPriority>(), Ok(JobPriority::Normal));
        assert_eq!("HiGh".parse::<JobPriority>(), Ok(JobPriority::High));
        assert_eq!("critical".parse::<JobPriority>(), Ok(JobPriority::Critical));
    }

    #[test]
    fn priority_from_str_reports_unknown_input_verbatim() {
        let err = "urgent".parse::<JobPriority>().unwrap_err();
        assert_eq!(err.0, "urgent");
        // Display surfaces the bad input.
        assert!(err.to_string().contains("urgent"));
    }

    #[test]
    fn status_from_str_rejects_unknown_input() {
        let err = "pending_review".parse::<JobStatus>().unwrap_err();
        assert_eq!(err.0, "pending_review");
        assert!(err.to_string().contains("pending_review"));
    }

    #[test]
    fn status_round_trips_for_every_variant() {
        for status in [
            JobStatus::Pending,
            JobStatus::Claimed,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Retry,
            JobStatus::Failed,
            JobStatus::DeadLetter,
            JobStatus::CancelRequested,
            JobStatus::Cancelled,
        ] {
            let s = status.as_str();
            assert_eq!(s.parse::<JobStatus>().unwrap(), status);
        }
    }

    #[test]
    fn parse_errors_are_error_trait_impls() {
        // Cheap guard against accidental removal of the Error impl, which
        // would break `?` propagation in user code that uses these parsers.
        fn assert_error<E: std::error::Error>() {}
        assert_error::<ParseJobPriorityError>();
        assert_error::<ParseJobStatusError>();
    }

    #[test]
    fn job_info_default_values_match_doctrine() {
        let info = JobInfo::default();
        assert_eq!(info.name, "");
        assert_eq!(info.timeout, Duration::from_secs(3600));
        assert_eq!(info.priority, JobPriority::Normal);
        assert!(!info.is_public);
        assert!(info.required_role.is_none());
        assert!(!info.idempotent);
        assert!(info.ttl.is_none());
    }

    #[test]
    fn retry_config_default_retries_on_all_errors() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.max_attempts, 3);
        assert_eq!(cfg.backoff, BackoffStrategy::Exponential);
        assert_eq!(cfg.max_backoff, Duration::from_secs(300));
        assert!(cfg.retry_on.is_empty(), "empty list ⇒ retry on every error");
    }

    #[test]
    fn backoff_fixed_returns_base_for_any_attempt() {
        let cfg = RetryConfig {
            backoff: BackoffStrategy::Fixed,
            ..Default::default()
        };
        for attempt in [1u32, 2, 5, 100] {
            assert_eq!(cfg.calculate_backoff(attempt), Duration::from_secs(1));
        }
    }

    #[test]
    fn backoff_linear_multiplies_base_by_attempt() {
        let cfg = RetryConfig {
            backoff: BackoffStrategy::Linear,
            ..Default::default()
        };
        assert_eq!(cfg.calculate_backoff(1), Duration::from_secs(1));
        assert_eq!(cfg.calculate_backoff(5), Duration::from_secs(5));
        assert_eq!(cfg.calculate_backoff(50), Duration::from_secs(50));
    }

    #[test]
    fn backoff_exponential_handles_attempt_zero_without_underflow() {
        // attempt = 0 ⇒ saturating_sub keeps exponent at 0 ⇒ 2^0 = 1 ⇒ base.
        let cfg = RetryConfig::default();
        assert_eq!(cfg.calculate_backoff(0), Duration::from_secs(1));
    }

    #[test]
    fn backoff_exponential_caps_at_max_backoff_for_large_attempt() {
        // attempt = 20 ⇒ 2^19 seconds = ~6 days, must cap to default 5 min.
        let cfg = RetryConfig::default();
        assert_eq!(cfg.calculate_backoff(20), Duration::from_secs(300));
    }

    #[test]
    fn backoff_strategy_default_is_exponential() {
        assert_eq!(BackoffStrategy::default(), BackoffStrategy::Exponential);
    }
}

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

use crate::Result;

/// Step execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Step not yet started.
    Pending,
    /// Step currently running.
    Running,
    /// Step completed successfully.
    Completed,
    /// Step failed.
    Failed,
    /// Step compensation ran.
    Compensated,
    /// Step was skipped.
    Skipped,
}

impl StepStatus {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Compensated => "compensated",
            Self::Skipped => "skipped",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "compensated" => Self::Compensated,
            "skipped" => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

/// Result of a step execution.
#[derive(Debug, Clone)]
pub struct StepResult<T> {
    /// Step name.
    pub name: String,
    /// Step status.
    pub status: StepStatus,
    /// Step result (if completed).
    pub value: Option<T>,
    /// Error message (if failed).
    pub error: Option<String>,
}

/// A workflow step definition.
pub struct Step<T> {
    /// Step name.
    pub name: String,
    /// Step result type.
    _marker: std::marker::PhantomData<T>,
}

impl<T> Step<T> {
    /// Create a new step.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            _marker: std::marker::PhantomData,
        }
    }
}

/// Builder for configuring and executing a step.
pub struct StepBuilder<'a, T, F, C>
where
    T: Serialize + DeserializeOwned + Send + 'static,
    F: Future<Output = Result<T>> + Send + 'a,
    C: Future<Output = Result<()>> + Send + 'a,
{
    name: String,
    run_fn: Option<Pin<Box<dyn FnOnce() -> F + Send + 'a>>>,
    compensate_fn: Option<Arc<dyn Fn(T) -> Pin<Box<C>> + Send + Sync + 'a>>,
    timeout: Option<Duration>,
    retry_count: u32,
    retry_delay: Duration,
    optional: bool,
    _marker: std::marker::PhantomData<(T, F, C)>,
}

impl<'a, T, F, C> StepBuilder<'a, T, F, C>
where
    T: Serialize + DeserializeOwned + Send + Clone + 'static,
    F: Future<Output = Result<T>> + Send + 'a,
    C: Future<Output = Result<()>> + Send + 'a,
{
    /// Create a new step builder.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            run_fn: None,
            compensate_fn: None,
            timeout: None,
            retry_count: 0,
            retry_delay: Duration::from_secs(1),
            optional: false,
            _marker: std::marker::PhantomData,
        }
    }

    /// Set the step execution function.
    pub fn run<RF>(mut self, f: RF) -> Self
    where
        RF: FnOnce() -> F + Send + 'a,
    {
        self.run_fn = Some(Box::pin(f));
        self
    }

    /// Set the compensation function.
    pub fn compensate<CF>(mut self, f: CF) -> Self
    where
        CF: Fn(T) -> Pin<Box<C>> + Send + Sync + 'a,
    {
        self.compensate_fn = Some(Arc::new(f));
        self
    }

    /// Set step timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Configure retry behavior.
    pub fn retry(mut self, count: u32, delay: Duration) -> Self {
        self.retry_count = count;
        self.retry_delay = delay;
        self
    }

    /// Mark the step as optional (failure won't trigger compensation).
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Get step name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if step is optional.
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// Get retry count.
    pub fn retry_count(&self) -> u32 {
        self.retry_count
    }

    /// Get retry delay.
    pub fn retry_delay(&self) -> Duration {
        self.retry_delay
    }

    /// Get timeout.
    pub fn get_timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

/// Configuration for a step (without closures, for storage).
#[derive(Debug, Clone)]
pub struct StepConfig {
    /// Step name.
    pub name: String,
    /// Step timeout.
    pub timeout: Option<Duration>,
    /// Retry count.
    pub retry_count: u32,
    /// Retry delay.
    pub retry_delay: Duration,
    /// Whether the step is optional.
    pub optional: bool,
    /// Whether the step has a compensation function.
    pub has_compensation: bool,
}

impl Default for StepConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            timeout: None,
            retry_count: 0,
            retry_delay: Duration::from_secs(1),
            optional: false,
            has_compensation: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_status_conversion() {
        assert_eq!(StepStatus::Pending.as_str(), "pending");
        assert_eq!(StepStatus::Running.as_str(), "running");
        assert_eq!(StepStatus::Completed.as_str(), "completed");
        assert_eq!(StepStatus::Failed.as_str(), "failed");
        assert_eq!(StepStatus::Compensated.as_str(), "compensated");

        assert_eq!(StepStatus::from_str("pending"), StepStatus::Pending);
        assert_eq!(StepStatus::from_str("completed"), StepStatus::Completed);
    }

    #[test]
    fn test_step_config_default() {
        let config = StepConfig::default();
        assert!(config.name.is_empty());
        assert!(!config.optional);
        assert_eq!(config.retry_count, 0);
    }
}

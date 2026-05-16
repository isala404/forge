//! Fluent step runner API for workflows.
//!
//! Provides a chainable API for defining and executing workflow steps:
//!
//! ```ignore
//! // Simple step - returns Some(result) on success
//! let result = ctx.step("fetch_data", || async { fetch_data().await })
//!     .run()
//!     .await?
//!     .expect("required step always succeeds");
//!
//! // Step with timeout
//! ctx.step("slow_op", || async { slow_operation().await })
//!     .timeout(Duration::from_secs(30))
//!     .run()
//!     .await?;
//!
//! // Step with retry
//! ctx.step("flaky_api", || async { call_external_api().await })
//!     .retry(3, Duration::from_secs(5))
//!     .run()
//!     .await?;
//!
//! // Step with compensation (rollback on later failure)
//! ctx.step("charge_card", || async { charge(&card).await })
//!     .compensate(|result| async move { refund(&result.charge_id).await })
//!     .run()
//!     .await?;
//!
//! // Optional step - returns None on failure instead of propagating error
//! let notification = ctx.step("send_notification", || async { notify_slack().await })
//!     .optional()
//!     .run()
//!     .await?; // Returns Ok(None) if notification fails
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use super::context::WorkflowContext;
use crate::Result;

/// Type alias for the step function (Fn to support retries).
type StepFn<T> = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<T>> + Send>> + Send + Sync>;

/// Type alias for the compensation function.
type CompensateFn<T> =
    Arc<dyn Fn(T) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;

/// A fluent builder for executing workflow steps.
///
/// Created via `WorkflowContext::step()`.
pub struct StepRunner<'a, T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    ctx: &'a WorkflowContext,
    name: String,
    step_fn: Option<StepFn<T>>,
    compensate_fn: Option<CompensateFn<T>>,
    timeout: Option<Duration>,
    retry_count: u32,
    retry_delay: Duration,
    optional: bool,
}

impl<'a, T> StepRunner<'a, T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// Create a new step runner.
    ///
    /// The closure must implement `Fn` (not just `FnOnce`) to support retries.
    /// For closures that capture moved values, wrap them in `Arc` or use `Clone`.
    pub(crate) fn new<F, Fut>(ctx: &'a WorkflowContext, name: impl Into<String>, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let name = name.into();
        let step_fn: StepFn<T> = Arc::new(move || Box::pin(f()));

        Self {
            ctx,
            name,
            step_fn: Some(step_fn),
            compensate_fn: None,
            timeout: None,
            retry_count: 0,
            retry_delay: Duration::from_secs(0),
            optional: false,
        }
    }

    /// Set a compensation function (rollback handler).
    ///
    /// If a later step fails, this compensation function will be called
    /// with the step's result to undo its effects (saga pattern).
    ///
    /// ```ignore
    /// ctx.step("charge_card", || async { charge(&card).await })
    ///     .compensate(|charge_result| async move {
    ///         refund(&charge_result.charge_id).await
    ///     })
    ///     .run()
    ///     .await?;
    /// ```
    pub fn compensate<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.compensate_fn = Some(Arc::new(move |result| Box::pin(f(result))));
        self
    }

    /// Set a timeout for this step.
    ///
    /// ```ignore
    /// ctx.step("slow_operation", || async { slow_op().await })
    ///     .timeout(Duration::from_secs(30))
    ///     .run()
    ///     .await?;
    /// ```
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Configure retry behavior for this step.
    ///
    /// If the step fails, it will be retried up to `count` times with a fixed
    /// `delay` between attempts. The step succeeds if any attempt succeeds.
    ///
    /// ```ignore
    /// ctx.step("call_flaky_api", || async { external_api_call().await })
    ///     .retry(3, Duration::from_secs(5))  // 3 retries, 5 second delay
    ///     .run()
    ///     .await?;
    /// ```
    pub fn retry(mut self, count: u32, delay: Duration) -> Self {
        self.retry_count = count;
        self.retry_delay = delay;
        self
    }

    /// Mark step as optional.
    ///
    /// If an optional step fails, the workflow continues without triggering
    /// compensation of previous steps. The step returns `Ok(None)` on failure
    /// instead of propagating the error.
    ///
    /// ```ignore
    /// let result = ctx.step("send_notification", || async { notify_slack().await })
    ///     .optional()
    ///     .run()
    ///     .await?; // Returns Ok(None) if notification fails
    ///
    /// if let Some(notification_id) = result {
    ///     println!("Notification sent: {}", notification_id);
    /// }
    /// ```
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Execute the step.
    ///
    /// This runs the step with configured timeout, retry, and compensation settings.
    /// Returns `Ok(Some(result))` on success. For optional steps, returns `Ok(None)`
    /// on failure instead of propagating the error.
    pub async fn run(mut self) -> Result<Option<T>> {
        let step_fn = self
            .step_fn
            .take()
            .expect("StepRunner::run called without step function");

        // Check if step already completed (for workflow resumption)
        if self.ctx.is_step_completed(&self.name)
            && let Some(result) = self.ctx.get_step_result::<T>(&self.name)
        {
            tracing::debug!(step = %self.name, "Step already completed, returning cached result");
            return Ok(Some(result));
        }

        // Record step start
        self.ctx.record_step_start(&self.name).await;

        // Execute with retry logic
        let total_attempts = self.retry_count + 1; // Initial attempt + retries
        let mut last_error = None;

        for attempt in 1..=total_attempts {
            let result = self.execute_with_timeout(&step_fn).await;

            match result {
                Ok(value) => {
                    // Success - record completion
                    let json_value =
                        serde_json::to_value(&value).unwrap_or(serde_json::Value::Null);
                    self.ctx.record_step_complete(&self.name, json_value).await;

                    // Register compensation handler if provided
                    if let Some(compensate_fn) = self.compensate_fn.take() {
                        let value_clone = value.clone();
                        self.ctx.register_compensation(
                            &self.name,
                            Arc::new(move |_| compensate_fn(value_clone.clone())),
                        );
                    }

                    return Ok(Some(value));
                }
                Err(e) => {
                    last_error = Some(e);

                    // Check if we should retry
                    if attempt < total_attempts {
                        if let Some(ref err) = last_error {
                            tracing::warn!(
                                step = %self.name,
                                attempt = attempt,
                                max_attempts = total_attempts,
                                delay_ms = self.retry_delay.as_millis() as u64,
                                error = %err,
                                "Step failed, retrying"
                            );
                        }
                        tokio::time::sleep(self.retry_delay).await;
                    }
                }
            }
        }

        // All attempts failed (last_error always Some after the loop runs at least once)
        let error = last_error.expect("loop ran at least one attempt");
        let error_msg = error.to_string();
        self.ctx.record_step_failure(&self.name, &error_msg).await;

        if self.optional {
            tracing::warn!(
                step = %self.name,
                error = %error_msg,
                attempts = total_attempts,
                "Optional step failed after all retries, continuing workflow"
            );
            return Ok(None);
        }

        Err(error)
    }

    /// Execute step function with optional timeout.
    async fn execute_with_timeout(&self, step_fn: &StepFn<T>) -> Result<T> {
        let fut = step_fn();

        if let Some(timeout_duration) = self.timeout {
            match tokio::time::timeout(timeout_duration, fut).await {
                Ok(result) => result,
                Err(_) => Err(crate::ForgeError::Timeout(format!(
                    "Step '{}' timed out after {:?}",
                    self.name, timeout_duration
                ))),
            }
        } else {
            fut.await
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_step_runner_builder_pattern() {
        // Just test that the builder pattern compiles
        // Actual execution tests would need a full WorkflowContext
    }
}

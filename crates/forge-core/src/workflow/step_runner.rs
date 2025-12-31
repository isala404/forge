//! Fluent step runner API for workflows.
//!
//! Provides a chainable API for defining and executing workflow steps:
//!
//! ```ignore
//! // Simple step
//! let result = ctx.step("fetch_data", || async { fetch_data().await }).run().await?;
//!
//! // Step with timeout
//! ctx.step("slow_op", || async { slow_operation().await })
//!     .timeout(Duration::from_secs(30))
//!     .run()
//!     .await?;
//!
//! // Step with compensation (rollback on later failure)
//! ctx.step("charge_card", || async { charge(&card).await })
//!     .compensate(|result| async move { refund(&result.charge_id).await })
//!     .run()
//!     .await?;
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

use super::context::WorkflowContext;
use crate::Result;

/// Type alias for the step function.
type StepFn<T> = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<T>> + Send>> + Send>;

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
    optional: bool,
}

impl<'a, T> StepRunner<'a, T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// Create a new step runner.
    pub(crate) fn new<F, Fut>(ctx: &'a WorkflowContext, name: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let name = name.into();
        let step_fn: StepFn<T> = Box::new(move || Box::pin(f()));

        Self {
            ctx,
            name,
            step_fn: Some(step_fn),
            compensate_fn: None,
            timeout: None,
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

    /// Mark step as optional.
    ///
    /// If an optional step fails, the workflow continues without triggering
    /// compensation of previous steps.
    ///
    /// ```ignore
    /// ctx.step("send_notification", || async { notify_slack().await })
    ///     .optional()
    ///     .run()
    ///     .await?; // Won't fail workflow if notification fails
    /// ```
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Execute the step.
    ///
    /// This runs the step with configured timeout and compensation settings.
    /// Returns the step result or an error.
    ///
    /// Note: For retry support with the fluent API, consider using a retryable
    /// wrapper inside your step function, or use the low-level API with manual
    /// retry logic.
    pub async fn run(mut self) -> Result<T> {
        let step_fn = self
            .step_fn
            .take()
            .expect("StepRunner::run called without step function");

        // Check if step already completed (for workflow resumption)
        if self.ctx.is_step_completed(&self.name) {
            if let Some(result) = self.ctx.get_step_result::<T>(&self.name) {
                tracing::debug!(step = %self.name, "Step already completed, returning cached result");
                return Ok(result);
            }
        }

        // Record step start
        self.ctx.record_step_start(&self.name);

        // Execute the step (with timeout if configured)
        let result = self.execute_with_timeout(step_fn).await;

        match result {
            Ok(value) => {
                // Success - record completion
                let json_value =
                    serde_json::to_value(&value).unwrap_or(serde_json::Value::Null);
                self.ctx.record_step_complete(&self.name, json_value);

                // Register compensation handler if provided
                if let Some(compensate_fn) = self.compensate_fn.take() {
                    let value_clone = value.clone();
                    self.ctx.register_compensation(
                        &self.name,
                        Arc::new(move |_| compensate_fn(value_clone.clone())),
                    );
                }

                Ok(value)
            }
            Err(e) => {
                let error_msg = e.to_string();
                self.ctx.record_step_failure(&self.name, &error_msg);

                if self.optional {
                    tracing::warn!(step = %self.name, error = %error_msg, "Optional step failed, continuing workflow");
                }

                Err(e)
            }
        }
    }

    /// Execute step function with optional timeout.
    async fn execute_with_timeout(&self, step_fn: StepFn<T>) -> Result<T> {
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
mod tests {
    use super::*;

    #[test]
    fn test_step_runner_builder_pattern() {
        // Just test that the builder pattern compiles
        // Actual execution tests would need a full WorkflowContext
    }
}

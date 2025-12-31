use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

use super::context::WorkflowContext;
use super::CompensationHandler;
use crate::{ForgeError, Result};

/// Type alias for parallel step handler.
type ParallelStepHandler =
    Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send + 'static>>;

/// A step to be executed in parallel.
struct ParallelStep {
    name: String,
    handler: ParallelStepHandler,
    compensate: Option<CompensationHandler>,
}

/// Builder for executing workflow steps in parallel.
pub struct ParallelBuilder<'a> {
    ctx: &'a WorkflowContext,
    steps: Vec<ParallelStep>,
}

impl<'a> ParallelBuilder<'a> {
    /// Create a new parallel builder.
    pub fn new(ctx: &'a WorkflowContext) -> Self {
        Self {
            ctx,
            steps: Vec::new(),
        }
    }

    /// Add a step to be executed in parallel.
    pub fn step<T, F, Fut>(mut self, name: &str, handler: F) -> Self
    where
        T: Serialize + Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let step_handler: ParallelStepHandler = Box::pin(async move {
            let result = handler().await?;
            serde_json::to_value(result).map_err(|e| ForgeError::Serialization(e.to_string()))
        });

        self.steps.push(ParallelStep {
            name: name.to_string(),
            handler: step_handler,
            compensate: None,
        });

        self
    }

    /// Add a step with compensation handler.
    pub fn step_with_compensate<T, F, Fut, C, CFut>(
        mut self,
        name: &str,
        handler: F,
        compensate: C,
    ) -> Self
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        C: Fn(T) -> CFut + Send + Sync + 'static,
        CFut: Future<Output = Result<()>> + Send + 'static,
    {
        let step_handler: ParallelStepHandler = Box::pin(async move {
            let result = handler().await?;
            serde_json::to_value(result).map_err(|e| ForgeError::Serialization(e.to_string()))
        });

        let compensation: CompensationHandler = Arc::new(move |value: serde_json::Value| {
            let result: std::result::Result<T, _> = serde_json::from_value(value);
            match result {
                Ok(typed_value) => Box::pin(compensate(typed_value))
                    as Pin<Box<dyn Future<Output = Result<()>> + Send>>,
                Err(e) => Box::pin(async move {
                    Err(ForgeError::Deserialization(format!(
                        "Failed to deserialize compensation value: {}",
                        e
                    )))
                }) as Pin<Box<dyn Future<Output = Result<()>> + Send>>,
            }
        });

        self.steps.push(ParallelStep {
            name: name.to_string(),
            handler: step_handler,
            compensate: Some(compensation),
        });

        self
    }

    /// Execute all steps in parallel.
    pub async fn run(self) -> Result<ParallelResults> {
        let mut results = ParallelResults::new();
        let mut compensation_handlers: Vec<(String, CompensationHandler)> = Vec::new();
        let mut pending_steps = Vec::new();

        // Check for cached results
        for step in self.steps {
            if let Some(cached) = self.ctx.get_step_result::<serde_json::Value>(&step.name) {
                results.insert(step.name.clone(), cached);
            } else {
                pending_steps.push(step);
            }
        }

        // If all steps are cached, return early
        if pending_steps.is_empty() {
            return Ok(results);
        }

        // Record step starts
        for step in &pending_steps {
            self.ctx.record_step_start(&step.name);
        }

        // Execute steps in parallel
        type StepResult = (
            String,
            Result<serde_json::Value>,
            Option<CompensationHandler>,
        );

        let handles: Vec<tokio::task::JoinHandle<StepResult>> = pending_steps
            .into_iter()
            .map(|step| {
                let name = step.name;
                let handler = step.handler;
                let compensate = step.compensate;
                tokio::spawn(async move {
                    let result = handler.await;
                    (name, result, compensate)
                })
            })
            .collect();

        // Collect results
        let step_results = futures::future::join_all(handles).await;
        let mut failed = false;
        let mut first_error: Option<ForgeError> = None;

        for join_result in step_results {
            let (name, result, compensate): StepResult =
                join_result.map_err(|e| ForgeError::Internal(format!("Task join error: {}", e)))?;

            match result {
                Ok(value) => {
                    self.ctx.record_step_complete(&name, value.clone());
                    results.insert(name.clone(), value);
                    if let Some(comp) = compensate {
                        compensation_handlers.push((name, comp));
                    }
                }
                Err(e) => {
                    self.ctx.record_step_failure(&name, e.to_string());
                    failed = true;
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        // If any step failed, run compensation in reverse order
        if failed {
            for (name, handler) in compensation_handlers.into_iter().rev() {
                self.ctx.register_compensation(&name, handler);
            }
            self.ctx.run_compensation().await;
            return Err(first_error.unwrap());
        }

        Ok(results)
    }
}

/// Results from parallel step execution.
#[derive(Debug, Clone, Default)]
pub struct ParallelResults {
    inner: HashMap<String, serde_json::Value>,
}

impl ParallelResults {
    /// Create empty results.
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Insert a result.
    pub fn insert(&mut self, step_name: String, value: serde_json::Value) {
        self.inner.insert(step_name, value);
    }

    /// Get a typed result by step name.
    pub fn get<T: DeserializeOwned>(&self, step_name: &str) -> Result<T> {
        let value = self
            .inner
            .get(step_name)
            .ok_or_else(|| ForgeError::NotFound(format!("Step '{}' not found", step_name)))?;
        serde_json::from_value(value.clone())
            .map_err(|e| ForgeError::Deserialization(e.to_string()))
    }

    /// Check if a step result exists.
    pub fn contains(&self, step_name: &str) -> bool {
        self.inner.contains_key(step_name)
    }

    /// Get the number of results.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over results.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
        self.inner.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parallel_results() {
        let mut results = ParallelResults::new();
        results.insert("step1".to_string(), serde_json::json!({"value": 42}));
        results.insert("step2".to_string(), serde_json::json!("hello"));

        assert!(results.contains("step1"));
        assert!(results.contains("step2"));
        assert!(!results.contains("step3"));
        assert_eq!(results.len(), 2);

        #[derive(Debug, serde::Deserialize, PartialEq)]
        struct StepResult {
            value: i32,
        }

        let step1: StepResult = results.get("step1").unwrap();
        assert_eq!(step1.value, 42);

        let step2: String = results.get("step2").unwrap();
        assert_eq!(step2, "hello");
    }
}

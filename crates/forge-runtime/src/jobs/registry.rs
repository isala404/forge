use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::job::{ForgeJob, JobContext, JobInfo};
use forge_core::Result;
use serde_json::Value;

/// Type alias for boxed job handler function.
pub type BoxedJobHandler = Arc<
    dyn Fn(&JobContext, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>
        + Send
        + Sync,
>;

/// Entry in the job registry.
pub struct JobEntry {
    /// Job metadata.
    pub info: JobInfo,
    /// Job handler function.
    pub handler: BoxedJobHandler,
}

/// Registry of all FORGE jobs.
#[derive(Clone, Default)]
pub struct JobRegistry {
    jobs: HashMap<String, Arc<JobEntry>>,
}

impl JobRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
        }
    }

    /// Register a job type.
    pub fn register<J: ForgeJob>(&mut self)
    where
        J::Args: serde::de::DeserializeOwned + Send + 'static,
        J::Output: serde::Serialize + Send + 'static,
    {
        let info = J::info();
        let name = info.name.to_string();

        let handler: BoxedJobHandler = Arc::new(move |ctx, args| {
            Box::pin(async move {
                let parsed_args: J::Args = serde_json::from_value(args)
                    .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                let result = J::execute(ctx, parsed_args).await?;
                serde_json::to_value(result)
                    .map_err(|e| forge_core::ForgeError::Internal(e.to_string()))
            })
        });

        self.jobs.insert(name, Arc::new(JobEntry { info, handler }));
    }

    /// Get a job entry by name.
    pub fn get(&self, name: &str) -> Option<Arc<JobEntry>> {
        self.jobs.get(name).cloned()
    }

    /// Get job info by name.
    pub fn info(&self, name: &str) -> Option<&JobInfo> {
        self.jobs.get(name).map(|e| &e.info)
    }

    /// Check if a job exists.
    pub fn exists(&self, name: &str) -> bool {
        self.jobs.contains_key(name)
    }

    /// Get all job names.
    pub fn job_names(&self) -> impl Iterator<Item = &str> {
        self.jobs.keys().map(|s| s.as_str())
    }

    /// Get all jobs.
    pub fn jobs(&self) -> impl Iterator<Item = (&str, &Arc<JobEntry>)> {
        self.jobs.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Get the number of registered jobs.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry() {
        let registry = JobRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.get("nonexistent").is_none());
    }
}

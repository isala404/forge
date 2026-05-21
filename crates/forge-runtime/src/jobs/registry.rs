use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::Result;
use forge_core::job::{ForgeJob, JobContext, JobInfo};
use serde_json::Value;

/// Normalize args for deserialization.
/// - Converts `null` to `{}` so both unit `()` and empty structs deserialize correctly.
/// - Unwraps `{"args": ...}` or `{"input": ...}` wrapper if present (callers may use either format).
fn normalize_args(args: Value) -> Value {
    let unwrapped = match &args {
        Value::Object(map) if map.len() == 1 => {
            if map.contains_key("args") {
                map.get("args").cloned().unwrap_or(Value::Null)
            } else if map.contains_key("input") {
                map.get("input").cloned().unwrap_or(Value::Null)
            } else {
                args
            }
        }
        _ => args,
    };

    match &unwrapped {
        Value::Null => Value::Object(serde_json::Map::new()),
        _ => unwrapped,
    }
}

/// Type alias for boxed job handler function.
pub type BoxedJobHandler = Arc<
    dyn Fn(&JobContext, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>
        + Send
        + Sync,
>;

pub type BoxedJobCompensation = Arc<
    dyn for<'a> Fn(
            &'a JobContext,
            Value,
            &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        + Send
        + Sync,
>;

/// Entry in the job registry.
pub struct JobEntry {
    /// Job metadata.
    pub info: JobInfo,
    /// Job handler function.
    pub handler: BoxedJobHandler,
    /// Job compensation handler.
    pub compensation: BoxedJobCompensation,
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
                let parsed_args: J::Args = serde_json::from_value(normalize_args(args))
                    .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                let result = J::execute(ctx, parsed_args).await?;
                serde_json::to_value(result).map_err(|e| {
                    forge_core::ForgeError::internal_with("Failed to serialize job result", e)
                })
            })
        });

        let compensation: BoxedJobCompensation = Arc::new(move |ctx, args, reason| {
            Box::pin(async move {
                let parsed_args: J::Args = serde_json::from_value(normalize_args(args))
                    .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                J::compensate(ctx, parsed_args, reason).await
            })
        });

        self.jobs.insert(
            name,
            Arc::new(JobEntry {
                info,
                handler,
                compensation,
            }),
        );
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

    /// Register a system job handler with a raw name and handler function.
    /// Used for internal bridge handlers (`$cron:*`, `$daemon:*`, `$workflow_resume`).
    pub fn register_system(
        &mut self,
        name: impl Into<String>,
        info: JobInfo,
        handler: BoxedJobHandler,
    ) {
        let noop_compensation: BoxedJobCompensation =
            Arc::new(|_ctx, _args, _reason| Box::pin(async { Ok(()) }));
        self.jobs.insert(
            name.into(),
            Arc::new(JobEntry {
                info,
                handler,
                compensation: noop_compensation,
            }),
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- normalize_args: jobs/registry collapses null to {} so derive(Default)
    // empty-struct args deserialize correctly. function/registry deliberately
    // keeps null as-is for unit `()` — this divergence is the contract.

    #[test]
    fn normalize_args_converts_null_to_empty_object() {
        assert_eq!(normalize_args(json!(null)), json!({}));
    }

    #[test]
    fn normalize_args_keeps_empty_object_intact() {
        // `{}` (len 0) skips the envelope unwrap and the null branch.
        assert_eq!(normalize_args(json!({})), json!({}));
    }

    #[test]
    fn normalize_args_unwraps_args_envelope() {
        assert_eq!(normalize_args(json!({"args": {"id": 7}})), json!({"id": 7}));
        // The trailing null-to-{} step still applies after unwrap.
        assert_eq!(normalize_args(json!({"args": null})), json!({}));
    }

    #[test]
    fn normalize_args_unwraps_input_envelope() {
        assert_eq!(normalize_args(json!({"input": [1,2]})), json!([1, 2]));
    }

    #[test]
    fn normalize_args_keeps_other_single_key_objects_intact() {
        // A handler with `struct Args { id: u32 }` must receive {"id":...}
        // as-is — envelope stripping only fires for `args`/`input`.
        assert_eq!(normalize_args(json!({"id": 7})), json!({"id": 7}));
    }

    #[test]
    fn normalize_args_keeps_multi_key_objects_intact() {
        let v = json!({"a": 1, "b": 2});
        assert_eq!(normalize_args(v.clone()), v);
    }

    #[test]
    fn normalize_args_keeps_non_null_non_object_values_intact() {
        assert_eq!(normalize_args(json!(42)), json!(42));
        assert_eq!(normalize_args(json!("x")), json!("x"));
        assert_eq!(normalize_args(json!([1])), json!([1]));
        assert_eq!(normalize_args(json!(true)), json!(true));
    }

    // --- registry construction + lookup ---

    fn sample_info(name: &'static str) -> JobInfo {
        JobInfo {
            name,
            ..Default::default()
        }
    }

    fn noop_handler() -> BoxedJobHandler {
        Arc::new(|_ctx, _args| Box::pin(async { Ok(Value::Null) }))
    }

    #[tokio::test]
    async fn new_registry_is_empty() {
        let reg = JobRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("anything").is_none());
        assert!(reg.info("anything").is_none());
        assert!(!reg.exists("anything"));
        assert_eq!(reg.job_names().count(), 0);
    }

    #[tokio::test]
    async fn register_system_inserts_and_lookups_succeed() {
        let mut reg = JobRegistry::new();
        reg.register_system(
            "$cron:nightly",
            sample_info("$cron:nightly"),
            noop_handler(),
        );

        assert!(reg.exists("$cron:nightly"));
        assert!(!reg.exists("$cron:hourly"));
        assert_eq!(reg.len(), 1);
        assert_eq!(
            reg.info("$cron:nightly").expect("info").name,
            "$cron:nightly"
        );
        assert!(reg.get("$cron:nightly").is_some());

        let names: Vec<&str> = reg.job_names().collect();
        assert_eq!(names, vec!["$cron:nightly"]);
    }

    #[tokio::test]
    async fn register_system_last_writer_wins_for_duplicate_name() {
        // System bridge handlers (`$cron:*`, etc.) are re-registered on every
        // startup. Re-registering the same name must overwrite, not duplicate.
        let mut reg = JobRegistry::new();
        let mut first = sample_info("$cron:x");
        first.description = Some("original");
        reg.register_system("$cron:x", first, noop_handler());

        let mut second = sample_info("$cron:x");
        second.description = Some("replaced");
        reg.register_system("$cron:x", second, noop_handler());

        assert_eq!(reg.len(), 1);
        assert_eq!(
            reg.info("$cron:x").expect("info").description,
            Some("replaced")
        );
    }

    #[tokio::test]
    async fn jobs_iterator_returns_all_registered_entries() {
        let mut reg = JobRegistry::new();
        reg.register_system("a", sample_info("a"), noop_handler());
        reg.register_system("b", sample_info("b"), noop_handler());

        let mut names: Vec<&str> = reg.jobs().map(|(n, _)| n).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn get_returns_owned_arc_outliving_registry_drop() {
        // `get` returns Arc<JobEntry>. Holding the Arc must keep the entry
        // alive after the registry is dropped — important when handlers are
        // looked up once and shared with long-lived worker tasks.
        let entry_arc = {
            let mut reg = JobRegistry::new();
            reg.register_system("$noop", sample_info("$noop"), noop_handler());
            reg.get("$noop").expect("entry")
        };
        assert_eq!(entry_arc.info.name, "$noop");
    }
}

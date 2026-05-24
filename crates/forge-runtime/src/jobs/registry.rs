use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::Result;
use forge_core::job::{ForgeJob, JobContext, JobInfo};
use forge_core::util::normalize_handler_args as normalize_args;
use serde_json::Value;

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

pub struct JobEntry {
    pub info: JobInfo,
    pub handler: BoxedJobHandler,
    pub compensation: BoxedJobCompensation,
}

#[derive(Clone, Default)]
pub struct JobRegistry {
    jobs: HashMap<String, Arc<JobEntry>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
        }
    }

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

    pub fn get(&self, name: &str) -> Option<Arc<JobEntry>> {
        self.jobs.get(name).cloned()
    }

    pub fn info(&self, name: &str) -> Option<&JobInfo> {
        self.jobs.get(name).map(|e| &e.info)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.jobs.contains_key(name)
    }

    pub fn job_names(&self) -> impl Iterator<Item = &str> {
        self.jobs.keys().map(|s| s.as_str())
    }

    pub fn jobs(&self) -> impl Iterator<Item = (&str, &Arc<JobEntry>)> {
        self.jobs.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Register internal bridge handlers (`$cron:*`, `$workflow_resume`, etc.).
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

    // normalize_args is exercised via `forge_core::util` tests; jobs/registry
    // now delegates to that shared helper.

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

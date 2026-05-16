use std::sync::Arc;

use forge_core::cron::CronContext;
use forge_core::job::{JobContext, JobInfo};
use serde_json::Value;

use super::registry::CronRegistry;
use crate::jobs::registry::{BoxedJobHandler, JobRegistry};

/// Register bridge job handlers for each cron in the registry.
///
/// For each registered cron named `foo`, this creates a `$cron:foo` job
/// handler that the worker pool can execute. The cron scheduler enqueues
/// these jobs instead of calling handlers directly.
pub fn register_cron_bridges(cron_registry: &Arc<CronRegistry>, job_registry: &mut JobRegistry) {
    for entry in cron_registry.list() {
        let cron_name = entry.info.name.to_string();
        let job_name = format!("$cron:{cron_name}");
        let handler_clone = entry.handler.clone();
        let timeout = entry.info.timeout;

        let handler: BoxedJobHandler = Arc::new(move |ctx: &JobContext, args: Value| {
            let handler = handler_clone.clone();
            let cron_name = cron_name.clone();
            Box::pin(async move {
                let run_id: uuid::Uuid = args
                    .get("run_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .unwrap_or_else(uuid::Uuid::new_v4);

                let scheduled_time: chrono::DateTime<chrono::Utc> = args
                    .get("scheduled_time")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(chrono::Utc::now);

                let timezone: String = args
                    .get("timezone")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("UTC")
                    .to_string();

                let is_catch_up: bool = args
                    .get("is_catch_up")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                let cron_ctx = CronContext::new(
                    run_id,
                    cron_name,
                    scheduled_time,
                    timezone,
                    is_catch_up,
                    ctx.pool().clone(),
                    ctx.circuit_breaker_client().clone(),
                );

                handler(&cron_ctx).await?;
                Ok(serde_json::Value::Null)
            })
        });

        // info.name is left empty because the HashMap key (job_name) is the
        // source of truth for routing. Bridge jobs are never dispatched through
        // the standard JobDispatch path which reads info.name. This avoids
        // leaking a heap-allocated String into &'static str via Box::leak.
        let info = JobInfo {
            name: "",
            timeout,
            ..JobInfo::default()
        };

        job_registry.register_system(job_name, info, handler);
    }
}

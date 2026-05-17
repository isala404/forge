use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use forge_core::function::JobDispatch;
use forge_core::job::{ForgeJob, JobInfo, JobPriority};
use uuid::Uuid;

use super::queue::{JobQueue, JobRecord};
use super::registry::JobRegistry;

/// Dispatches jobs to the queue.
#[derive(Clone)]
pub struct JobDispatcher {
    queue: JobQueue,
    registry: JobRegistry,
}

impl JobDispatcher {
    /// Create a new job dispatcher.
    pub fn new(queue: JobQueue, registry: JobRegistry) -> Self {
        Self { queue, registry }
    }

    /// Dispatch a job immediately.
    pub async fn dispatch<J: ForgeJob>(&self, args: J::Args) -> Result<Uuid, forge_core::ForgeError>
    where
        J::Args: serde::Serialize,
    {
        let info = J::info();
        self.dispatch_with_info(&info, serde_json::to_value(args)?, None)
            .await
    }

    /// Dispatch a job with a delay.
    pub async fn dispatch_in<J: ForgeJob>(
        &self,
        delay: Duration,
        args: J::Args,
    ) -> Result<Uuid, forge_core::ForgeError>
    where
        J::Args: serde::Serialize,
    {
        let info = J::info();
        let scheduled_at = Utc::now()
            + chrono::Duration::from_std(delay)
                .map_err(|_| forge_core::ForgeError::InvalidArgument("delay too large".into()))?;
        self.dispatch_at_with_info(&info, serde_json::to_value(args)?, scheduled_at)
            .await
    }

    /// Dispatch a job at a specific time.
    pub async fn dispatch_at<J: ForgeJob>(
        &self,
        at: DateTime<Utc>,
        args: J::Args,
    ) -> Result<Uuid, forge_core::ForgeError>
    where
        J::Args: serde::Serialize,
    {
        let info = J::info();
        self.dispatch_at_with_info(&info, serde_json::to_value(args)?, at)
            .await
    }

    /// Dispatch job by name (dynamic).
    pub async fn dispatch_by_name(
        &self,
        job_type: &str,
        args: serde_json::Value,
        owner_subject: Option<String>,
    ) -> Result<Uuid, forge_core::ForgeError> {
        let entry = self.registry.get(job_type).ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!("Job type '{}' not found", job_type))
        })?;

        self.dispatch_with_info(&entry.info, args, owner_subject)
            .await
    }

    /// Dispatch job with explicit info.
    async fn dispatch_with_info(
        &self,
        info: &JobInfo,
        args: serde_json::Value,
        owner_subject: Option<String>,
    ) -> Result<Uuid, forge_core::ForgeError> {
        let mut job = JobRecord::new(
            info.name,
            args,
            info.priority,
            info.retry.max_attempts as i32,
        )
        .with_owner_subject(owner_subject);

        if let Some(cap) = info.worker_capability {
            job = job.with_capability(cap);
        }

        self.queue
            .enqueue(job)
            .await
            .map_err(forge_core::ForgeError::Database)
    }

    /// Request cancellation for a job.
    ///
    /// If `caller_subject` is provided, the cancellation will only succeed if
    /// the caller owns the job or the job has no owner.
    pub async fn cancel(
        &self,
        job_id: Uuid,
        reason: Option<&str>,
        caller_subject: Option<&str>,
    ) -> Result<bool, forge_core::ForgeError> {
        self.queue
            .request_cancel(job_id, reason, caller_subject)
            .await
            .map_err(forge_core::ForgeError::Database)
    }

    /// Dispatch job at specific time with explicit info.
    async fn dispatch_at_with_info(
        &self,
        info: &JobInfo,
        args: serde_json::Value,
        scheduled_at: DateTime<Utc>,
    ) -> Result<Uuid, forge_core::ForgeError> {
        let mut job = JobRecord::new(
            info.name,
            args,
            info.priority,
            info.retry.max_attempts as i32,
        )
        .with_scheduled_at(scheduled_at);

        if let Some(cap) = info.worker_capability {
            job = job.with_capability(cap);
        }

        self.queue
            .enqueue(job)
            .await
            .map_err(forge_core::ForgeError::Database)
    }

    /// Dispatch job with idempotency key.
    pub async fn dispatch_idempotent<J: ForgeJob>(
        &self,
        idempotency_key: impl Into<String>,
        args: J::Args,
    ) -> Result<Uuid, forge_core::ForgeError>
    where
        J::Args: serde::Serialize,
    {
        let info = J::info();
        let mut job = JobRecord::new(
            info.name,
            serde_json::to_value(args)?,
            info.priority,
            info.retry.max_attempts as i32,
        )
        .with_idempotency_key(idempotency_key);

        if let Some(cap) = info.worker_capability {
            job = job.with_capability(cap);
        }

        self.queue
            .enqueue(job)
            .await
            .map_err(forge_core::ForgeError::Database)
    }

    /// Dispatch job with custom priority.
    pub async fn dispatch_with_priority<J: ForgeJob>(
        &self,
        priority: JobPriority,
        args: J::Args,
    ) -> Result<Uuid, forge_core::ForgeError>
    where
        J::Args: serde::Serialize,
    {
        let info = J::info();
        let mut job = JobRecord::new(
            info.name,
            serde_json::to_value(args)?,
            priority,
            info.retry.max_attempts as i32,
        );

        if let Some(cap) = info.worker_capability {
            job = job.with_capability(cap);
        }

        self.queue
            .enqueue(job)
            .await
            .map_err(forge_core::ForgeError::Database)
    }
}

impl JobDispatch for JobDispatcher {
    fn get_info(&self, job_type: &str) -> Option<JobInfo> {
        self.registry.get(job_type).map(|e| e.info.clone())
    }

    fn dispatch_by_name(
        &self,
        job_type: &str,
        args: serde_json::Value,
        owner_subject: Option<String>,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Uuid>> + Send + '_>> {
        let job_type = job_type.to_string();
        Box::pin(async move { self.dispatch_by_name(&job_type, args, owner_subject).await })
    }

    fn dispatch_in_conn<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        job_type: &'a str,
        args: serde_json::Value,
        owner_subject: Option<String>,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Uuid>> + Send + 'a>> {
        Box::pin(async move {
            let entry = self.registry.get(job_type).ok_or_else(|| {
                forge_core::ForgeError::NotFound(format!("Job type '{}' not found", job_type))
            })?;

            let mut record = JobRecord::new(
                entry.info.name,
                args,
                entry.info.priority,
                entry.info.retry.max_attempts as i32,
            )
            .with_owner_subject(owner_subject);
            if let Some(cap) = entry.info.worker_capability {
                record = record.with_capability(cap);
            }

            self.queue
                .enqueue_in_conn(conn, record)
                .await
                .map_err(forge_core::ForgeError::Database)
        })
    }

    fn cancel(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<bool>> + Send + '_>> {
        Box::pin(async move { self.cancel(job_id, reason.as_deref(), None).await })
    }
}

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use crate::jobs::registry::BoxedJobHandler;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};
    use std::sync::Arc;

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        let db = base
            .isolated(test_name)
            .await
            .expect("Failed to create isolated db");
        let system_sql = crate::pg::migration::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    fn noop_handler() -> BoxedJobHandler {
        Arc::new(|_ctx, _args| Box::pin(async { Ok(serde_json::Value::Null) }))
    }

    fn dispatcher_with_registry(
        pool: sqlx::PgPool,
        seed: impl FnOnce(&mut JobRegistry),
    ) -> JobDispatcher {
        let queue = JobQueue::new(pool);
        let mut registry = JobRegistry::new();
        seed(&mut registry);
        JobDispatcher::new(queue, registry)
    }

    fn info_with(name: &'static str, capability: Option<&'static str>) -> JobInfo {
        JobInfo {
            name,
            worker_capability: capability,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn dispatch_by_name_returns_not_found_for_unregistered_job() {
        // The whole point of the registry-aware dispatcher: callers can't
        // accidentally enqueue jobs no worker can run. NotFound here keeps
        // typos from silently parking work in the queue.
        let db = setup_db("dispatch_unknown").await;
        let dispatcher = dispatcher_with_registry(db.pool().clone(), |_| {});

        let err = dispatcher
            .dispatch_by_name("ghost", serde_json::json!({}), None)
            .await
            .expect_err("unknown job must error");

        match err {
            forge_core::ForgeError::NotFound(msg) => {
                assert!(msg.contains("ghost"), "error must name the missing job");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_by_name_enqueues_with_registered_metadata() {
        // The dispatcher must pull priority and capability from the registry's
        // JobInfo, not from the caller — that's the contract that lets the
        // `#[job]` macro own job behavior at the call site.
        let db = setup_db("dispatch_capability").await;
        let pool = db.pool().clone();
        let dispatcher = dispatcher_with_registry(pool.clone(), |reg| {
            reg.register_system("ship", info_with("ship", Some("media")), noop_handler());
        });

        let job_id = dispatcher
            .dispatch_by_name(
                "ship",
                serde_json::json!({"to": "warehouse"}),
                Some("u-1".into()),
            )
            .await
            .unwrap();

        let row: (String, Option<String>, Option<String>, serde_json::Value) = sqlx::query_as(
            "SELECT job_type, worker_capability, owner_subject, input
             FROM forge_jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.0, "ship");
        assert_eq!(row.1.as_deref(), Some("media"));
        assert_eq!(row.2.as_deref(), Some("u-1"));
        assert_eq!(row.3, serde_json::json!({"to": "warehouse"}));
    }

    #[tokio::test]
    async fn dispatch_in_conn_only_commits_with_surrounding_tx() {
        // This is the JobDispatch trait method used inside mutation handlers.
        // It MUST honor the transaction passed in — if the outer tx rolls
        // back, the dispatched job must vanish too. Otherwise a partially-
        // failed mutation would still trigger background work.
        let db = setup_db("dispatch_in_conn_rollback").await;
        let pool = db.pool().clone();
        let dispatcher = dispatcher_with_registry(pool.clone(), |reg| {
            reg.register_system("ship", info_with("ship", None), noop_handler());
        });

        let mut tx = pool.begin().await.unwrap();
        let id = JobDispatch::dispatch_in_conn(
            &dispatcher,
            &mut tx,
            "ship",
            serde_json::json!({}),
            None,
        )
        .await
        .unwrap();
        // Rollback before commit — the job must disappear.
        tx.rollback().await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM forge_jobs WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "rolled-back tx must not leave job behind");
    }

    #[tokio::test]
    async fn dispatch_in_conn_commits_with_surrounding_tx() {
        // Inverse: when the surrounding tx commits, the job becomes visible
        // to workers exactly once.
        let db = setup_db("dispatch_in_conn_commit").await;
        let pool = db.pool().clone();
        let dispatcher = dispatcher_with_registry(pool.clone(), |reg| {
            reg.register_system("ship", info_with("ship", None), noop_handler());
        });

        let mut tx = pool.begin().await.unwrap();
        let id = JobDispatch::dispatch_in_conn(
            &dispatcher,
            &mut tx,
            "ship",
            serde_json::json!({}),
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM forge_jobs WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn dispatch_in_conn_returns_not_found_for_unregistered_job() {
        let db = setup_db("dispatch_in_conn_unknown").await;
        let pool = db.pool().clone();
        let dispatcher = dispatcher_with_registry(pool.clone(), |_| {});

        let mut tx = pool.begin().await.unwrap();
        let err = JobDispatch::dispatch_in_conn(
            &dispatcher,
            &mut tx,
            "missing",
            serde_json::json!({}),
            None,
        )
        .await
        .expect_err("unknown job must error");
        tx.rollback().await.unwrap();

        assert!(matches!(err, forge_core::ForgeError::NotFound(_)));
    }

    #[tokio::test]
    async fn get_info_returns_registered_info_or_none() {
        let db = setup_db("dispatcher_get_info").await;
        let dispatcher = dispatcher_with_registry(db.pool().clone(), |reg| {
            reg.register_system("ship", info_with("ship", Some("media")), noop_handler());
        });

        let info = JobDispatch::get_info(&dispatcher, "ship").expect("registered");
        assert_eq!(info.name, "ship");
        assert_eq!(info.worker_capability, Some("media"));
        assert!(JobDispatch::get_info(&dispatcher, "absent").is_none());
    }

    #[tokio::test]
    async fn cancel_returns_false_for_unknown_job() {
        // cancel propagates the queue's response — there's nothing to cancel,
        // so it must report false rather than error. The caller decides
        // whether that's a problem.
        let db = setup_db("dispatcher_cancel_missing").await;
        let dispatcher = dispatcher_with_registry(db.pool().clone(), |_| {});

        let ok = dispatcher
            .cancel(Uuid::new_v4(), Some("test"), None)
            .await
            .unwrap();
        assert!(!ok);
    }

    #[tokio::test]
    async fn cancel_marks_owned_job_when_caller_matches() {
        // Owned-job cancel: the queue checks `owner_subject` before flagging
        // for the worker. A matching subject must succeed.
        let db = setup_db("dispatcher_cancel_owner_ok").await;
        let pool = db.pool().clone();
        let dispatcher = dispatcher_with_registry(pool.clone(), |reg| {
            reg.register_system("ship", info_with("ship", None), noop_handler());
        });

        let job_id = dispatcher
            .dispatch_by_name("ship", serde_json::json!({}), Some("alice".into()))
            .await
            .unwrap();

        let ok = dispatcher
            .cancel(job_id, Some("user requested"), Some("alice"))
            .await
            .unwrap();
        assert!(ok, "owner must be able to cancel their own job");
    }

    #[tokio::test]
    async fn cancel_rejects_owned_job_when_caller_differs() {
        // Cross-tenant cancel attempt: must report false, must not flag the
        // job. This is the tenancy guardrail.
        let db = setup_db("dispatcher_cancel_owner_mismatch").await;
        let pool = db.pool().clone();
        let dispatcher = dispatcher_with_registry(pool.clone(), |reg| {
            reg.register_system("ship", info_with("ship", None), noop_handler());
        });

        let job_id = dispatcher
            .dispatch_by_name("ship", serde_json::json!({}), Some("alice".into()))
            .await
            .unwrap();

        let ok = dispatcher
            .cancel(job_id, Some("malicious"), Some("mallory"))
            .await
            .unwrap();
        assert!(!ok, "non-owner must not be able to cancel");
    }
}

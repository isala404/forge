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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatcher_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");
        let queue = JobQueue::new(pool);
        let registry = JobRegistry::new();
        let _dispatcher = JobDispatcher::new(queue, registry);
    }
}

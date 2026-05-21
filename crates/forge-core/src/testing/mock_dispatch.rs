//! Mock dispatchers for testing job and workflow dispatch.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::sync::RwLock;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::{ForgeError, Result};
use crate::job::JobStatus;
use crate::workflow::WorkflowStatus;

/// Record of a dispatched job.
#[derive(Debug, Clone)]
pub struct DispatchedJob {
    pub id: Uuid,
    pub job_type: String,
    pub args: serde_json::Value,
    pub owner_subject: Option<String>,
    /// `true` when dispatched via `dispatch_in_conn`.
    pub in_connection: bool,
    pub dispatched_at: DateTime<Utc>,
    /// `None` for immediately-runnable jobs.
    pub scheduled_at: Option<DateTime<Utc>>,
    pub status: JobStatus,
    pub cancel_reason: Option<String>,
}

/// Record of a started workflow.
#[derive(Debug, Clone)]
pub struct StartedWorkflow {
    pub run_id: Uuid,
    pub workflow_name: String,
    pub input: serde_json::Value,
    pub started_at: DateTime<Utc>,
    pub status: WorkflowStatus,
}

/// Records dispatched jobs for later verification.
pub struct MockJobDispatch {
    jobs: RwLock<Vec<DispatchedJob>>,
}

impl MockJobDispatch {
    pub fn new() -> Self {
        Self {
            jobs: RwLock::new(Vec::new()),
        }
    }

    pub async fn dispatch<T: serde::Serialize>(&self, job_type: &str, args: T) -> Result<Uuid> {
        self.dispatch_inner(job_type, args, None, false, None).await
    }

    pub async fn dispatch_at<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        scheduled_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        self.dispatch_inner(job_type, args, None, false, Some(scheduled_at))
            .await
    }

    async fn dispatch_inner<T: serde::Serialize>(
        &self,
        job_type: &str,
        args: T,
        owner_subject: Option<String>,
        in_connection: bool,
        scheduled_at: Option<DateTime<Utc>>,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let args_json =
            serde_json::to_value(args).map_err(|e| ForgeError::Serialization(e.to_string()))?;

        let job = DispatchedJob {
            id,
            job_type: job_type.to_string(),
            args: args_json,
            owner_subject,
            in_connection,
            dispatched_at: Utc::now(),
            scheduled_at,
            status: JobStatus::Pending,
            cancel_reason: None,
        };

        self.jobs.write().expect("jobs lock poisoned").push(job);
        Ok(id)
    }

    pub fn dispatched_jobs(&self) -> Vec<DispatchedJob> {
        self.jobs.read().expect("jobs lock poisoned").clone()
    }

    pub fn jobs_of_type(&self, job_type: &str) -> Vec<DispatchedJob> {
        self.jobs
            .read()
            .expect("jobs lock poisoned")
            .iter()
            .filter(|j| j.job_type == job_type)
            .cloned()
            .collect()
    }

    pub fn assert_dispatched(&self, job_type: &str) {
        let jobs = self.jobs.read().expect("jobs lock poisoned");
        let found = jobs.iter().any(|j| j.job_type == job_type);
        assert!(
            found,
            "Expected job '{}' to be dispatched, but it wasn't. Dispatched jobs: {:?}",
            job_type,
            jobs.iter().map(|j| &j.job_type).collect::<Vec<_>>()
        );
    }

    pub fn assert_dispatched_with<F>(&self, job_type: &str, predicate: F)
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let jobs = self.jobs.read().expect("jobs lock poisoned");
        let found = jobs
            .iter()
            .any(|j| j.job_type == job_type && predicate(&j.args));
        assert!(
            found,
            "Expected job '{}' with matching args to be dispatched",
            job_type
        );
    }

    pub fn assert_not_dispatched(&self, job_type: &str) {
        let jobs = self.jobs.read().expect("jobs lock poisoned");
        let found = jobs.iter().any(|j| j.job_type == job_type);
        assert!(
            !found,
            "Expected job '{}' NOT to be dispatched, but it was",
            job_type
        );
    }

    pub fn assert_dispatch_count(&self, job_type: &str, expected: usize) {
        let jobs = self.jobs.read().expect("jobs lock poisoned");
        let count = jobs.iter().filter(|j| j.job_type == job_type).count();
        assert_eq!(
            count, expected,
            "Expected {} dispatches of '{}', but found {}",
            expected, job_type, count
        );
    }

    pub fn clear(&self) {
        self.jobs.write().expect("jobs lock poisoned").clear();
    }

    pub fn complete_job(&self, job_id: Uuid) {
        let mut jobs = self.jobs.write().expect("jobs lock poisoned");
        if let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) {
            job.status = JobStatus::Completed;
        }
    }

    pub fn fail_job(&self, job_id: Uuid) {
        let mut jobs = self.jobs.write().expect("jobs lock poisoned");
        if let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) {
            job.status = JobStatus::Failed;
        }
    }

    pub fn cancel_job(&self, job_id: Uuid, reason: Option<String>) {
        let mut jobs = self.jobs.write().expect("jobs lock poisoned");
        if let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) {
            job.status = JobStatus::Cancelled;
            job.cancel_reason = reason;
        }
    }
}

impl Default for MockJobDispatch {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::function::JobDispatch for MockJobDispatch {
    fn get_info(&self, _job_type: &str) -> Option<crate::job::JobInfo> {
        None
    }

    fn dispatch_by_name(
        &self,
        job_type: &str,
        args: serde_json::Value,
        owner_subject: Option<String>,
        _tenant_id: Option<Uuid>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
        let job_type = job_type.to_string();
        Box::pin(async move {
            self.dispatch_inner(&job_type, args, owner_subject, false, None)
                .await
        })
    }

    fn dispatch_by_name_at(
        &self,
        job_type: &str,
        args: serde_json::Value,
        scheduled_at: DateTime<Utc>,
        owner_subject: Option<String>,
        _tenant_id: Option<Uuid>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
        let job_type = job_type.to_string();
        Box::pin(async move {
            self.dispatch_inner(&job_type, args, owner_subject, false, Some(scheduled_at))
                .await
        })
    }

    fn dispatch_in_conn<'a>(
        &'a self,
        _conn: &'a mut sqlx::PgConnection,
        job_type: &'a str,
        args: serde_json::Value,
        owner_subject: Option<String>,
        _tenant_id: Option<Uuid>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + 'a>> {
        Box::pin(async move {
            self.dispatch_inner(job_type, args, owner_subject, true, None)
                .await
        })
    }

    fn dispatch_in_conn_at<'a>(
        &'a self,
        _conn: &'a mut sqlx::PgConnection,
        job_type: &'a str,
        args: serde_json::Value,
        scheduled_at: DateTime<Utc>,
        owner_subject: Option<String>,
        _tenant_id: Option<Uuid>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + 'a>> {
        Box::pin(async move {
            self.dispatch_inner(job_type, args, owner_subject, true, Some(scheduled_at))
                .await
        })
    }

    fn cancel(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        Box::pin(async move {
            self.cancel_job(job_id, reason);
            Ok(true)
        })
    }
}

/// Records started workflows for later verification.
pub struct MockWorkflowDispatch {
    workflows: RwLock<Vec<StartedWorkflow>>,
}

impl MockWorkflowDispatch {
    pub fn new() -> Self {
        Self {
            workflows: RwLock::new(Vec::new()),
        }
    }

    pub async fn start<T: serde::Serialize>(&self, workflow_name: &str, input: T) -> Result<Uuid> {
        let run_id = Uuid::new_v4();
        let input_json =
            serde_json::to_value(input).map_err(|e| ForgeError::Serialization(e.to_string()))?;

        let workflow = StartedWorkflow {
            run_id,
            workflow_name: workflow_name.to_string(),
            input: input_json,
            started_at: Utc::now(),
            status: WorkflowStatus::Pending,
        };

        self.workflows
            .write()
            .expect("workflows lock poisoned")
            .push(workflow);
        Ok(run_id)
    }

    pub fn started_workflows(&self) -> Vec<StartedWorkflow> {
        self.workflows
            .read()
            .expect("workflows lock poisoned")
            .clone()
    }

    pub fn workflows_named(&self, name: &str) -> Vec<StartedWorkflow> {
        self.workflows
            .read()
            .expect("workflows lock poisoned")
            .iter()
            .filter(|w| w.workflow_name == name)
            .cloned()
            .collect()
    }

    pub fn assert_started(&self, workflow_name: &str) {
        let workflows = self.workflows.read().expect("workflows lock poisoned");
        let found = workflows.iter().any(|w| w.workflow_name == workflow_name);
        assert!(
            found,
            "Expected workflow '{}' to be started, but it wasn't. Started workflows: {:?}",
            workflow_name,
            workflows
                .iter()
                .map(|w| &w.workflow_name)
                .collect::<Vec<_>>()
        );
    }

    pub fn assert_started_with<F>(&self, workflow_name: &str, predicate: F)
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let workflows = self.workflows.read().expect("workflows lock poisoned");
        let found = workflows
            .iter()
            .any(|w| w.workflow_name == workflow_name && predicate(&w.input));
        assert!(
            found,
            "Expected workflow '{}' with matching input to be started",
            workflow_name
        );
    }

    pub fn assert_not_started(&self, workflow_name: &str) {
        let workflows = self.workflows.read().expect("workflows lock poisoned");
        let found = workflows.iter().any(|w| w.workflow_name == workflow_name);
        assert!(
            !found,
            "Expected workflow '{}' NOT to be started, but it was",
            workflow_name
        );
    }

    pub fn assert_start_count(&self, workflow_name: &str, expected: usize) {
        let workflows = self.workflows.read().expect("workflows lock poisoned");
        let count = workflows
            .iter()
            .filter(|w| w.workflow_name == workflow_name)
            .count();
        assert_eq!(
            count, expected,
            "Expected {} starts of '{}', but found {}",
            expected, workflow_name, count
        );
    }

    pub fn clear(&self) {
        self.workflows
            .write()
            .expect("workflows lock poisoned")
            .clear();
    }

    pub fn complete_workflow(&self, run_id: Uuid) {
        let mut workflows = self.workflows.write().expect("workflows lock poisoned");
        if let Some(workflow) = workflows.iter_mut().find(|w| w.run_id == run_id) {
            workflow.status = WorkflowStatus::Completed;
        }
    }

    pub fn fail_workflow(&self, run_id: Uuid) {
        let mut workflows = self.workflows.write().expect("workflows lock poisoned");
        if let Some(workflow) = workflows.iter_mut().find(|w| w.run_id == run_id) {
            workflow.status = WorkflowStatus::Failed;
        }
    }
}

impl Default for MockWorkflowDispatch {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::function::WorkflowDispatch for MockWorkflowDispatch {
    fn get_info(&self, _workflow_name: &str) -> Option<crate::workflow::WorkflowInfo> {
        None
    }

    fn start_by_name(
        &self,
        workflow_name: &str,
        input: serde_json::Value,
        _owner_subject: Option<String>,
        _trace_id: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
        let name = workflow_name.to_string();
        Box::pin(async move { self.start(&name, input).await })
    }

    fn start_in_conn<'a>(
        &'a self,
        _conn: &'a mut sqlx::PgConnection,
        workflow_name: &'a str,
        input: serde_json::Value,
        _owner_subject: Option<String>,
        _trace_id: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + 'a>> {
        Box::pin(async move { self.start(workflow_name, input).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_job_dispatch() {
        let dispatch = MockJobDispatch::new();

        let job_id = dispatch
            .dispatch("send_email", serde_json::json!({"to": "test@example.com"}))
            .await
            .unwrap();

        assert!(!job_id.is_nil());
        dispatch.assert_dispatched("send_email");
        dispatch.assert_not_dispatched("other_job");
    }

    #[tokio::test]
    async fn test_job_dispatch_with_args() {
        let dispatch = MockJobDispatch::new();

        dispatch
            .dispatch("send_email", serde_json::json!({"to": "test@example.com"}))
            .await
            .unwrap();

        dispatch.assert_dispatched_with("send_email", |args| args["to"] == "test@example.com");
    }

    #[tokio::test]
    async fn test_job_dispatch_count() {
        let dispatch = MockJobDispatch::new();

        dispatch
            .dispatch("job_a", serde_json::json!({}))
            .await
            .unwrap();
        dispatch
            .dispatch("job_b", serde_json::json!({}))
            .await
            .unwrap();
        dispatch
            .dispatch("job_a", serde_json::json!({}))
            .await
            .unwrap();

        dispatch.assert_dispatch_count("job_a", 2);
        dispatch.assert_dispatch_count("job_b", 1);
    }

    #[tokio::test]
    async fn test_mock_workflow_dispatch() {
        let dispatch = MockWorkflowDispatch::new();

        let run_id = dispatch
            .start("onboarding", serde_json::json!({"user_id": "123"}))
            .await
            .unwrap();

        assert!(!run_id.is_nil());
        dispatch.assert_started("onboarding");
        dispatch.assert_not_started("other_workflow");
    }

    #[tokio::test]
    async fn test_workflow_dispatch_with_input() {
        let dispatch = MockWorkflowDispatch::new();

        dispatch
            .start("onboarding", serde_json::json!({"user_id": "123"}))
            .await
            .unwrap();

        dispatch.assert_started_with("onboarding", |input| input["user_id"] == "123");
    }

    #[tokio::test]
    async fn test_clear() {
        let dispatch = MockJobDispatch::new();
        dispatch
            .dispatch("test", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(dispatch.dispatched_jobs().len(), 1);
        dispatch.clear();
        assert_eq!(dispatch.dispatched_jobs().len(), 0);
    }

    #[tokio::test]
    async fn test_job_status_simulation() {
        let dispatch = MockJobDispatch::new();
        let job_id = dispatch
            .dispatch("test", serde_json::json!({}))
            .await
            .unwrap();

        let jobs = dispatch.dispatched_jobs();
        assert_eq!(jobs[0].status, JobStatus::Pending);

        dispatch.complete_job(job_id);

        let jobs = dispatch.dispatched_jobs();
        assert_eq!(jobs[0].status, JobStatus::Completed);
    }

    #[tokio::test]
    async fn test_job_cancel_simulation() {
        let dispatch = MockJobDispatch::new();
        let job_id = dispatch
            .dispatch("test", serde_json::json!({}))
            .await
            .unwrap();

        dispatch.cancel_job(job_id, Some("user request".to_string()));

        let jobs = dispatch.dispatched_jobs();
        assert_eq!(jobs[0].status, JobStatus::Cancelled);
        assert_eq!(jobs[0].cancel_reason.as_deref(), Some("user request"));
    }
}

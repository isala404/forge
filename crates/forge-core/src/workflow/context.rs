use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::step::StepStatus;
use crate::function::AuthContext;

/// Step state stored during execution.
#[derive(Debug, Clone)]
pub struct StepState {
    /// Step name.
    pub name: String,
    /// Step status.
    pub status: StepStatus,
    /// Serialized result (if completed).
    pub result: Option<serde_json::Value>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// When the step started.
    pub started_at: Option<DateTime<Utc>>,
    /// When the step completed.
    pub completed_at: Option<DateTime<Utc>>,
}

impl StepState {
    /// Create a new pending step state.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Pending,
            result: None,
            error: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// Mark step as running.
    pub fn start(&mut self) {
        self.status = StepStatus::Running;
        self.started_at = Some(Utc::now());
    }

    /// Mark step as completed with result.
    pub fn complete(&mut self, result: serde_json::Value) {
        self.status = StepStatus::Completed;
        self.result = Some(result);
        self.completed_at = Some(Utc::now());
    }

    /// Mark step as failed with error.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = StepStatus::Failed;
        self.error = Some(error.into());
        self.completed_at = Some(Utc::now());
    }

    /// Mark step as compensated.
    pub fn compensate(&mut self) {
        self.status = StepStatus::Compensated;
    }
}

/// Context available to workflow handlers.
pub struct WorkflowContext {
    /// Workflow run ID.
    pub run_id: Uuid,
    /// Workflow name.
    pub workflow_name: String,
    /// Workflow version.
    pub version: u32,
    /// When the workflow started.
    pub started_at: DateTime<Utc>,
    /// Deterministic workflow time (consistent across replays).
    workflow_time: DateTime<Utc>,
    /// Authentication context.
    pub auth: AuthContext,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client.
    http_client: reqwest::Client,
    /// Step states (for resumption).
    step_states: Arc<RwLock<HashMap<String, StepState>>>,
    /// Completed steps in order (for compensation).
    completed_steps: Arc<RwLock<Vec<String>>>,
}

impl WorkflowContext {
    /// Create a new workflow context.
    pub fn new(
        run_id: Uuid,
        workflow_name: String,
        version: u32,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        let now = Utc::now();
        Self {
            run_id,
            workflow_name,
            version,
            started_at: now,
            workflow_time: now,
            auth: AuthContext::unauthenticated(),
            db_pool,
            http_client,
            step_states: Arc::new(RwLock::new(HashMap::new())),
            completed_steps: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get the deterministic workflow time.
    pub fn workflow_time(&self) -> DateTime<Utc> {
        self.workflow_time
    }

    /// Get the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Set authentication context.
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }

    /// Restore step states from persisted data.
    pub fn with_step_states(self, states: HashMap<String, StepState>) -> Self {
        let completed: Vec<String> = states
            .iter()
            .filter(|(_, s)| s.status == StepStatus::Completed)
            .map(|(name, _)| name.clone())
            .collect();

        *self.step_states.write().unwrap() = states;
        *self.completed_steps.write().unwrap() = completed;
        self
    }

    /// Get step state by name.
    pub fn get_step_state(&self, name: &str) -> Option<StepState> {
        self.step_states.read().unwrap().get(name).cloned()
    }

    /// Check if a step is already completed.
    pub fn is_step_completed(&self, name: &str) -> bool {
        self.step_states
            .read()
            .unwrap()
            .get(name)
            .map(|s| s.status == StepStatus::Completed)
            .unwrap_or(false)
    }

    /// Get the result of a completed step.
    pub fn get_step_result<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.step_states
            .read()
            .unwrap()
            .get(name)
            .and_then(|s| s.result.as_ref())
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Record step start.
    pub fn record_step_start(&self, name: &str) {
        let mut states = self.step_states.write().unwrap();
        let state = states
            .entry(name.to_string())
            .or_insert_with(|| StepState::new(name));
        state.start();
    }

    /// Record step completion.
    pub fn record_step_complete(&self, name: &str, result: serde_json::Value) {
        let mut states = self.step_states.write().unwrap();
        if let Some(state) = states.get_mut(name) {
            state.complete(result);
        }
        drop(states);

        let mut completed = self.completed_steps.write().unwrap();
        if !completed.contains(&name.to_string()) {
            completed.push(name.to_string());
        }
    }

    /// Record step failure.
    pub fn record_step_failure(&self, name: &str, error: impl Into<String>) {
        let mut states = self.step_states.write().unwrap();
        if let Some(state) = states.get_mut(name) {
            state.fail(error);
        }
    }

    /// Record step compensation.
    pub fn record_step_compensated(&self, name: &str) {
        let mut states = self.step_states.write().unwrap();
        if let Some(state) = states.get_mut(name) {
            state.compensate();
        }
    }

    /// Get completed steps in reverse order (for compensation).
    pub fn completed_steps_reversed(&self) -> Vec<String> {
        let completed = self.completed_steps.read().unwrap();
        completed.iter().rev().cloned().collect()
    }

    /// Get all step states.
    pub fn all_step_states(&self) -> HashMap<String, StepState> {
        self.step_states.read().unwrap().clone()
    }

    /// Get elapsed time since workflow started.
    pub fn elapsed(&self) -> chrono::Duration {
        Utc::now() - self.started_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_workflow_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let run_id = Uuid::new_v4();
        let ctx = WorkflowContext::new(
            run_id,
            "test_workflow".to_string(),
            1,
            pool,
            reqwest::Client::new(),
        );

        assert_eq!(ctx.run_id, run_id);
        assert_eq!(ctx.workflow_name, "test_workflow");
        assert_eq!(ctx.version, 1);
    }

    #[tokio::test]
    async fn test_step_state_tracking() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = WorkflowContext::new(
            Uuid::new_v4(),
            "test".to_string(),
            1,
            pool,
            reqwest::Client::new(),
        );

        ctx.record_step_start("step1");
        assert!(!ctx.is_step_completed("step1"));

        ctx.record_step_complete("step1", serde_json::json!({"result": "ok"}));
        assert!(ctx.is_step_completed("step1"));

        let result: Option<serde_json::Value> = ctx.get_step_result("step1");
        assert!(result.is_some());
    }

    #[test]
    fn test_step_state_transitions() {
        let mut state = StepState::new("test");
        assert_eq!(state.status, StepStatus::Pending);

        state.start();
        assert_eq!(state.status, StepStatus::Running);
        assert!(state.started_at.is_some());

        state.complete(serde_json::json!({}));
        assert_eq!(state.status, StepStatus::Completed);
        assert!(state.completed_at.is_some());
    }
}

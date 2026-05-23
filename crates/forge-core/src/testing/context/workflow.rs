//! Test context for workflow functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use super::build_test_auth;
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::AuthContext;

/// Step state stored during testing.
#[derive(Debug, Clone)]
pub struct TestStepState {
    pub completed: bool,
    pub result: Option<serde_json::Value>,
}

/// Test context for workflow functions.
pub struct TestWorkflowContext {
    pub run_id: Uuid,
    pub workflow_name: String,
    pub started_at: DateTime<Utc>,
    workflow_time: DateTime<Utc>,
    is_resumed: bool,
    tenant_id: Option<Uuid>,
    pub auth: AuthContext,
    pool: Option<PgPool>,
    http: Arc<MockHttp>,
    step_states: Arc<RwLock<HashMap<String, TestStepState>>>,
    /// Ordered for compensation.
    completed_steps: Arc<RwLock<Vec<String>>>,
    sleep_called: Arc<RwLock<bool>>,
    env_provider: Arc<MockEnvProvider>,
    saved_state: Arc<RwLock<HashMap<String, serde_json::Value>>>,
}

impl TestWorkflowContext {
    /// Create a new builder.
    pub fn builder(workflow_name: impl Into<String>) -> TestWorkflowContextBuilder {
        TestWorkflowContextBuilder::new(workflow_name)
    }

    /// Get the database pool (if available).
    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Get the mock HTTP client.
    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    /// Check if this is a resumed execution.
    pub fn is_resumed(&self) -> bool {
        self.is_resumed
    }

    /// Get the deterministic workflow time.
    pub fn workflow_time(&self) -> DateTime<Utc> {
        self.workflow_time
    }

    /// Get the tenant ID.
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id
    }

    /// Check if a step is completed.
    pub fn is_step_completed(&self, name: &str) -> bool {
        self.step_states
            .read()
            .unwrap()
            .get(name)
            .map(|s| s.completed)
            .unwrap_or(false)
    }

    /// Check if a step has been started (exists in step states).
    pub fn is_step_started(&self, name: &str) -> bool {
        self.step_states.read().unwrap().contains_key(name)
    }

    /// Get the result of a completed step.
    pub fn get_step_result<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.step_states
            .read()
            .unwrap()
            .get(name)
            .and_then(|s| s.result.clone())
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Record step start.
    ///
    /// Mirrors `WorkflowContext::record_step_start` so workflow code that
    /// uses `?` on the call compiles unchanged in tests. The in-memory test
    /// implementation can never fail, but the signature must match.
    pub async fn record_step_start(&self, name: &str) -> Result<()> {
        let mut states = self.step_states.write().unwrap();
        states
            .entry(name.to_string())
            .or_insert_with(|| TestStepState {
                completed: false,
                result: None,
            });
        Ok(())
    }

    /// Record step completion.
    pub async fn record_step_complete(&self, name: &str, result: serde_json::Value) -> Result<()> {
        let mut states = self.step_states.write().unwrap();
        let state = states
            .entry(name.to_string())
            .or_insert_with(|| TestStepState {
                completed: false,
                result: None,
            });
        state.completed = true;
        state.result = Some(result);
        drop(states);

        let mut completed = self.completed_steps.write().unwrap();
        if !completed.contains(&name.to_string()) {
            completed.push(name.to_string());
        }
        Ok(())
    }

    /// Record step failure.
    pub async fn record_step_failure(&self, name: &str, _error: impl Into<String>) -> Result<()> {
        let mut states = self.step_states.write().unwrap();
        states
            .entry(name.to_string())
            .or_insert_with(|| TestStepState {
                completed: false,
                result: None,
            });
        Ok(())
    }

    /// Get completed step names in order.
    pub fn completed_step_names(&self) -> Vec<String> {
        self.completed_steps.read().unwrap().clone()
    }

    /// Durable sleep (no-op in tests, but records the intent).
    pub async fn sleep(&self, _duration: Duration) -> Result<()> {
        *self.sleep_called.write().unwrap() = true;
        Ok(())
    }

    /// Check if sleep was called.
    pub fn sleep_called(&self) -> bool {
        *self.sleep_called.read().unwrap()
    }

    /// Get elapsed time since workflow started.
    pub fn elapsed(&self) -> chrono::Duration {
        Utc::now() - self.started_at
    }

    /// Get the mock env provider for verification.
    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }

    /// Save arbitrary state that persists across suspension points.
    pub fn save_state(&self, key: &str, value: impl serde::Serialize) -> Result<()> {
        let json = serde_json::to_value(value)
            .map_err(|e| crate::ForgeError::Serialization(e.to_string()))?;
        self.saved_state
            .write()
            .unwrap()
            .insert(key.to_string(), json);
        Ok(())
    }

    /// Load previously saved state. Returns `None` if the key doesn't exist.
    pub fn load_state<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let guard = self.saved_state.read().unwrap();
        match guard.get(key) {
            Some(value) => {
                let result = serde_json::from_value(value.clone())
                    .map_err(|e| crate::ForgeError::Deserialization(e.to_string()))?;
                Ok(Some(result))
            }
            None => Ok(None),
        }
    }

    /// Get a snapshot of all saved state.
    pub fn take_saved_state(&self) -> HashMap<String, serde_json::Value> {
        self.saved_state.read().unwrap().clone()
    }
}

impl EnvAccess for TestWorkflowContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Builder for TestWorkflowContext.
pub struct TestWorkflowContextBuilder {
    run_id: Option<Uuid>,
    workflow_name: String,
    started_at: DateTime<Utc>,
    workflow_time: Option<DateTime<Utc>>,
    is_resumed: bool,
    tenant_id: Option<Uuid>,
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
    http: MockHttp,
    completed_steps: HashMap<String, serde_json::Value>,
    env_vars: HashMap<String, String>,
}

impl TestWorkflowContextBuilder {
    /// Create a new builder.
    pub fn new(workflow_name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            run_id: None,
            workflow_name: workflow_name.into(),
            started_at: now,
            workflow_time: None,
            is_resumed: false,
            tenant_id: None,
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            pool: None,
            http: MockHttp::new(),
            completed_steps: HashMap::new(),
            env_vars: HashMap::new(),
        }
    }

    /// Set a specific run ID.
    pub fn with_run_id(mut self, id: Uuid) -> Self {
        self.run_id = Some(id);
        self
    }

    /// Set the workflow time (for deterministic testing).
    pub fn with_workflow_time(mut self, time: DateTime<Utc>) -> Self {
        self.workflow_time = Some(time);
        self
    }

    /// Mark as a resumed execution.
    pub fn as_resumed(mut self) -> Self {
        self.is_resumed = true;
        self
    }

    /// Add a completed step (for resume testing).
    pub fn with_completed_step(
        mut self,
        name: impl Into<String>,
        result: serde_json::Value,
    ) -> Self {
        self.completed_steps.insert(name.into(), result);
        self
    }

    /// Set the tenant ID.
    ///
    /// Production reads the tenant from `auth.claims["tenant_id"]`, so this
    /// writes the same value into the claims map.
    pub fn with_tenant(mut self, tenant_id: Uuid) -> Self {
        self.tenant_id = Some(tenant_id);
        self.claims.insert(
            "tenant_id".to_string(),
            serde_json::Value::String(tenant_id.to_string()),
        );
        self
    }

    /// Set the authenticated user with a UUID.
    pub fn as_user(mut self, id: Uuid) -> Self {
        self.user_id = Some(id);
        self
    }

    /// For non-UUID auth providers (Firebase, Clerk, etc.).
    pub fn as_subject(mut self, subject: impl Into<String>) -> Self {
        self.claims
            .insert("sub".to_string(), serde_json::json!(subject.into()));
        self
    }

    /// Add a role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Add multiple roles.
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles.extend(roles);
        self
    }

    /// Add a custom claim.
    pub fn with_claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(key.into(), value);
        self
    }

    /// Set the database pool.
    pub fn with_pool(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Add an HTTP mock.
    pub fn mock_http<F>(self, pattern: &str, handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.http.add_mock_sync(pattern, handler);
        self
    }

    /// Add an HTTP mock that returns a JSON response.
    pub fn mock_http_json<T: serde::Serialize>(self, pattern: &str, response: T) -> Self {
        let json = serde_json::to_value(response).unwrap_or(serde_json::Value::Null);
        self.mock_http(pattern, move |_| MockResponse::json(json.clone()))
    }

    /// Set a single environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Set multiple environment variables.
    pub fn with_envs(mut self, vars: HashMap<String, String>) -> Self {
        self.env_vars.extend(vars);
        self
    }

    /// Build the test context.
    pub fn build(self) -> TestWorkflowContext {
        let step_states: HashMap<String, TestStepState> = self
            .completed_steps
            .iter()
            .map(|(name, result)| {
                (
                    name.clone(),
                    TestStepState {
                        completed: true,
                        result: Some(result.clone()),
                    },
                )
            })
            .collect();

        let completed_steps: Vec<String> = self.completed_steps.keys().cloned().collect();

        TestWorkflowContext {
            run_id: self.run_id.unwrap_or_else(Uuid::new_v4),
            workflow_name: self.workflow_name,
            started_at: self.started_at,
            workflow_time: self.workflow_time.unwrap_or(self.started_at),
            is_resumed: self.is_resumed,
            tenant_id: self.tenant_id,
            auth: build_test_auth(self.user_id, self.roles, self.claims),
            pool: self.pool,
            http: Arc::new(self.http),
            step_states: Arc::new(RwLock::new(step_states)),
            completed_steps: Arc::new(RwLock::new(completed_steps)),
            sleep_called: Arc::new(RwLock::new(false)),
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
            saved_state: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_context_creation() {
        let ctx = TestWorkflowContext::builder("test_workflow").build();

        assert_eq!(ctx.workflow_name, "test_workflow");
        assert!(!ctx.is_resumed());
    }

    #[tokio::test]
    async fn test_step_tracking() {
        let ctx = TestWorkflowContext::builder("test").build();

        assert!(!ctx.is_step_completed("step1"));

        ctx.record_step_start("step1").await.unwrap();
        ctx.record_step_complete("step1", serde_json::json!({"result": "ok"}))
            .await
            .unwrap();

        assert!(ctx.is_step_completed("step1"));

        let result: Option<serde_json::Value> = ctx.get_step_result("step1");
        assert!(result.is_some());
    }

    #[test]
    fn test_resumed_with_completed_steps() {
        let ctx = TestWorkflowContext::builder("test")
            .as_resumed()
            .with_completed_step("step1", serde_json::json!({"id": 123}))
            .with_completed_step("step2", serde_json::json!({"status": "ok"}))
            .build();

        assert!(ctx.is_resumed());
        assert!(ctx.is_step_completed("step1"));
        assert!(ctx.is_step_completed("step2"));
        assert!(!ctx.is_step_completed("step3"));
    }

    #[tokio::test]
    async fn test_step_order() {
        let ctx = TestWorkflowContext::builder("test").build();

        ctx.record_step_complete("step1", serde_json::json!({}))
            .await
            .unwrap();
        ctx.record_step_complete("step2", serde_json::json!({}))
            .await
            .unwrap();
        ctx.record_step_complete("step3", serde_json::json!({}))
            .await
            .unwrap();

        let completed = ctx.completed_step_names();
        assert_eq!(completed, vec!["step1", "step2", "step3"]);
    }

    #[tokio::test]
    async fn test_durable_sleep() {
        let ctx = TestWorkflowContext::builder("test").build();

        assert!(!ctx.sleep_called());
        ctx.sleep(Duration::from_secs(3600)).await.unwrap();
        assert!(ctx.sleep_called());
    }

    #[test]
    fn test_deterministic_time() {
        let fixed_time = Utc::now();
        let ctx = TestWorkflowContext::builder("test")
            .with_workflow_time(fixed_time)
            .build();

        assert_eq!(ctx.workflow_time(), fixed_time);
    }

    #[test]
    fn test_tenant() {
        let tenant_id = Uuid::new_v4();
        let ctx = TestWorkflowContext::builder("test")
            .with_tenant(tenant_id)
            .build();

        assert_eq!(ctx.tenant_id(), Some(tenant_id));
    }
}

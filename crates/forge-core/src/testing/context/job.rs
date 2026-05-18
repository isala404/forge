//! Test context for job functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use uuid::Uuid;

use serde::Serialize;

use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use super::build_test_auth;
use crate::Result;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::AuthContext;

/// Progress update recorded during testing.
#[derive(Debug, Clone)]
pub struct TestProgressUpdate {
    /// Progress percentage (0-100).
    pub percent: u8,
    /// Progress message.
    pub message: String,
}

/// Test context for job functions.
///
/// Provides an isolated testing environment for jobs with progress tracking,
/// retry simulation, cancellation testing, and HTTP mocking.
///
/// # Example
///
/// ```ignore
/// let ctx = TestJobContext::builder("export_users")
///     .with_job_id(Uuid::new_v4())
///     .build();
///
/// // Simulate progress
/// ctx.progress(50, "Halfway there")?;
///
/// // Verify progress was recorded
/// assert_eq!(ctx.progress_updates().len(), 1);
///
/// // Test cancellation
/// ctx.request_cancellation();
/// assert!(ctx.is_cancel_requested().unwrap());
/// ```
pub struct TestJobContext {
    /// Job ID.
    pub job_id: Uuid,
    /// Job type name.
    pub job_type: String,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Maximum attempts allowed.
    pub max_attempts: u32,
    /// Authentication context.
    pub auth: AuthContext,
    /// Optional database pool.
    pool: Option<PgPool>,
    /// Mock HTTP client.
    http: Arc<MockHttp>,
    /// Progress updates recorded during execution.
    progress_updates: Arc<RwLock<Vec<TestProgressUpdate>>>,
    /// Mock environment provider.
    env_provider: Arc<MockEnvProvider>,
    /// Persisted saved data (in-memory).
    saved_data: Arc<RwLock<serde_json::Value>>,
    /// Whether cancellation has been requested.
    cancel_requested: Arc<AtomicBool>,
    /// Dispatched sub-jobs (for assertion).
    dispatched_jobs: Arc<RwLock<Vec<(String, serde_json::Value, Uuid)>>>,
    /// Started workflows (for assertion).
    started_workflows: Arc<RwLock<Vec<(String, serde_json::Value, Uuid)>>>,
}

impl TestJobContext {
    /// Create a new builder.
    pub fn builder(job_type: impl Into<String>) -> TestJobContextBuilder {
        TestJobContextBuilder::new(job_type)
    }

    /// Get the database pool (if available).
    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Get the mock HTTP client.
    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    /// Report job progress.
    pub fn progress(&self, percent: u8, message: impl Into<String>) -> Result<()> {
        let update = TestProgressUpdate {
            percent: percent.min(100),
            message: message.into(),
        };
        self.progress_updates.write().unwrap().push(update);
        Ok(())
    }

    /// Get all progress updates.
    pub fn progress_updates(&self) -> Vec<TestProgressUpdate> {
        self.progress_updates.read().unwrap().clone()
    }

    /// Get all saved job data.
    ///
    /// Returns the in-memory data that was written via [`save()`](Self::save).
    pub fn saved(&self) -> serde_json::Value {
        self.saved_data.read().unwrap().clone()
    }

    /// Save a key-value pair to job data.
    ///
    /// Merges `key` into the saved data object. Use [`saved()`](Self::saved)
    /// to read it back in assertions.
    pub fn save(&self, key: &str, value: serde_json::Value) -> Result<()> {
        let mut guard = self.saved_data.write().unwrap();
        if let Some(map) = guard.as_object_mut() {
            map.insert(key.to_string(), value);
        } else {
            let mut map = serde_json::Map::new();
            map.insert(key.to_string(), value);
            *guard = serde_json::Value::Object(map);
        }
        Ok(())
    }

    /// Check if this is a retry attempt.
    pub fn is_retry(&self) -> bool {
        self.attempt > 1
    }

    /// Check if this is the last attempt.
    pub fn is_last_attempt(&self) -> bool {
        self.attempt >= self.max_attempts
    }

    /// Simulate heartbeat (no-op in tests, but records the intent).
    pub async fn heartbeat(&self) -> Result<()> {
        Ok(())
    }

    /// Check if cancellation has been requested.
    pub fn is_cancel_requested(&self) -> Result<bool> {
        Ok(self.cancel_requested.load(Ordering::SeqCst))
    }

    /// Return an error if cancellation has been requested.
    ///
    /// Use this in job handlers to check for cancellation and exit early.
    pub fn check_cancelled(&self) -> Result<()> {
        if self.cancel_requested.load(Ordering::SeqCst) {
            Err(crate::ForgeError::JobCancelled(
                "Job cancellation requested".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    /// Request cancellation (for testing cancellation flows).
    ///
    /// After calling this, `is_cancel_requested()` returns `true` and
    /// `check_cancelled()` returns an error.
    pub fn request_cancellation(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// Buffer a sub-job dispatch (mirrors `JobContext::dispatch_job`).
    pub fn dispatch_job<T: Serialize>(&self, job_type: &str, args: &T) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let json = serde_json::to_value(args)
            .map_err(|e| crate::ForgeError::Serialization(e.to_string()))?;
        self.dispatched_jobs
            .write()
            .unwrap()
            .push((job_type.to_string(), json, id));
        Ok(id)
    }

    /// Buffer a workflow start (mirrors `JobContext::start_workflow`).
    pub fn start_workflow<T: Serialize>(&self, workflow_name: &str, args: &T) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let json = serde_json::to_value(args)
            .map_err(|e| crate::ForgeError::Serialization(e.to_string()))?;
        self.started_workflows
            .write()
            .unwrap()
            .push((workflow_name.to_string(), json, id));
        Ok(id)
    }

    /// Get all dispatched sub-jobs for assertions.
    pub fn dispatched_jobs(&self) -> Vec<(String, serde_json::Value, Uuid)> {
        self.dispatched_jobs.read().unwrap().clone()
    }

    /// Get all started workflows for assertions.
    pub fn started_workflows(&self) -> Vec<(String, serde_json::Value, Uuid)> {
        self.started_workflows.read().unwrap().clone()
    }

    /// Get the mock env provider for verification.
    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }
}

impl EnvAccess for TestJobContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

/// Builder for TestJobContext.
pub struct TestJobContextBuilder {
    job_id: Option<Uuid>,
    job_type: String,
    attempt: u32,
    max_attempts: u32,
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
    http: MockHttp,
    env_vars: HashMap<String, String>,
    cancel_requested: bool,
}

impl TestJobContextBuilder {
    /// Create a new builder with job type.
    pub fn new(job_type: impl Into<String>) -> Self {
        Self {
            job_id: None,
            job_type: job_type.into(),
            attempt: 1,
            max_attempts: 1,
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            pool: None,
            http: MockHttp::new(),
            env_vars: HashMap::new(),
            cancel_requested: false,
        }
    }

    /// Set a specific job ID.
    pub fn with_job_id(mut self, id: Uuid) -> Self {
        self.job_id = Some(id);
        self
    }

    /// Set as a retry (attempt > 1).
    pub fn as_retry(mut self, attempt: u32) -> Self {
        self.attempt = attempt.max(1);
        self
    }

    /// Set the maximum attempts.
    pub fn with_max_attempts(mut self, max: u32) -> Self {
        self.max_attempts = max.max(1);
        self
    }

    /// Set as the last attempt.
    pub fn as_last_attempt(mut self) -> Self {
        self.attempt = 3;
        self.max_attempts = 3;
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

    /// Add an HTTP mock with a custom handler.
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

    /// Start with cancellation already requested.
    ///
    /// Use this to test how jobs handle cancellation from the start.
    pub fn with_cancellation_requested(mut self) -> Self {
        self.cancel_requested = true;
        self
    }

    /// Build the test context.
    pub fn build(self) -> TestJobContext {
        TestJobContext {
            job_id: self.job_id.unwrap_or_else(Uuid::new_v4),
            job_type: self.job_type,
            attempt: self.attempt,
            max_attempts: self.max_attempts,
            auth: build_test_auth(self.user_id, self.roles, self.claims),
            pool: self.pool,
            http: Arc::new(self.http),
            progress_updates: Arc::new(RwLock::new(Vec::new())),
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
            saved_data: Arc::new(RwLock::new(crate::job::empty_saved_data())),
            cancel_requested: Arc::new(AtomicBool::new(self.cancel_requested)),
            dispatched_jobs: Arc::new(RwLock::new(Vec::new())),
            started_workflows: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_context_creation() {
        let ctx = TestJobContext::builder("export_users").build();

        assert_eq!(ctx.job_type, "export_users");
        assert_eq!(ctx.attempt, 1);
        assert!(!ctx.is_retry());
        assert!(ctx.is_last_attempt()); // 1 of 1
    }

    #[test]
    fn test_retry_detection() {
        let ctx = TestJobContext::builder("test")
            .as_retry(3)
            .with_max_attempts(5)
            .build();

        assert!(ctx.is_retry());
        assert!(!ctx.is_last_attempt());
    }

    #[test]
    fn test_last_attempt() {
        let ctx = TestJobContext::builder("test").as_last_attempt().build();

        assert!(ctx.is_retry());
        assert!(ctx.is_last_attempt());
    }

    #[test]
    fn test_progress_tracking() {
        let ctx = TestJobContext::builder("test").build();

        ctx.progress(25, "Step 1 complete").unwrap();
        ctx.progress(50, "Step 2 complete").unwrap();
        ctx.progress(100, "Done").unwrap();

        let updates = ctx.progress_updates();
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].percent, 25);
        assert_eq!(updates[2].percent, 100);
    }

    #[test]
    fn test_save_and_saved() {
        let ctx = TestJobContext::builder("test").build();
        ctx.save("charge_id", serde_json::json!("ch_123")).unwrap();
        ctx.save("amount", serde_json::json!(100)).unwrap();

        let saved = ctx.saved();
        assert_eq!(saved["charge_id"], "ch_123");
        assert_eq!(saved["amount"], 100);
    }

    #[test]
    fn test_cancellation_not_requested() {
        let ctx = TestJobContext::builder("test").build();

        assert!(!ctx.is_cancel_requested().unwrap());
        assert!(ctx.check_cancelled().is_ok());
    }

    #[test]
    fn test_cancellation_requested_at_build() {
        let ctx = TestJobContext::builder("test")
            .with_cancellation_requested()
            .build();

        assert!(ctx.is_cancel_requested().unwrap());
        assert!(ctx.check_cancelled().is_err());
    }

    #[test]
    fn test_request_cancellation_mid_test() {
        let ctx = TestJobContext::builder("test").build();

        assert!(!ctx.is_cancel_requested().unwrap());
        ctx.request_cancellation();
        assert!(ctx.is_cancel_requested().unwrap());
        assert!(ctx.check_cancelled().is_err());
    }
}

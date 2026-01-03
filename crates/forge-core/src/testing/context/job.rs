//! Test context for job functions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use crate::function::AuthContext;
use crate::Result;

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
/// retry simulation, and HTTP mocking.
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

    /// Set the authenticated user.
    pub fn as_user(mut self, id: Uuid) -> Self {
        self.user_id = Some(id);
        self
    }

    /// Add a role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
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

    /// Build the test context.
    pub fn build(self) -> TestJobContext {
        let auth = if let Some(user_id) = self.user_id {
            AuthContext::authenticated(user_id, self.roles, self.claims)
        } else {
            AuthContext::unauthenticated()
        };

        TestJobContext {
            job_id: self.job_id.unwrap_or_else(Uuid::new_v4),
            job_type: self.job_type,
            attempt: self.attempt,
            max_attempts: self.max_attempts,
            auth,
            pool: self.pool,
            http: Arc::new(self.http),
            progress_updates: Arc::new(RwLock::new(Vec::new())),
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
}

//! Test context for cron functions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use crate::function::AuthContext;

/// Log entry recorded during testing.
#[derive(Debug, Clone)]
pub struct TestLogEntry {
    /// Log level.
    pub level: String,
    /// Log message.
    pub message: String,
    /// Associated data.
    pub data: serde_json::Value,
}

/// Test log for cron context.
#[derive(Clone)]
pub struct TestCronLog {
    cron_name: String,
    entries: Arc<RwLock<Vec<TestLogEntry>>>,
}

impl TestCronLog {
    /// Create a new test cron log.
    pub fn new(cron_name: impl Into<String>) -> Self {
        Self {
            cron_name: cron_name.into(),
            entries: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Log an info message.
    pub fn info(&self, message: &str) {
        self.log("info", message, serde_json::Value::Null);
    }

    /// Log an info message with data.
    pub fn info_with(&self, message: &str, data: serde_json::Value) {
        self.log("info", message, data);
    }

    /// Log a warning message.
    pub fn warn(&self, message: &str) {
        self.log("warn", message, serde_json::Value::Null);
    }

    /// Log a warning message with data.
    pub fn warn_with(&self, message: &str, data: serde_json::Value) {
        self.log("warn", message, data);
    }

    /// Log an error message.
    pub fn error(&self, message: &str) {
        self.log("error", message, serde_json::Value::Null);
    }

    /// Log an error message with data.
    pub fn error_with(&self, message: &str, data: serde_json::Value) {
        self.log("error", message, data);
    }

    /// Log a debug message.
    pub fn debug(&self, message: &str) {
        self.log("debug", message, serde_json::Value::Null);
    }

    fn log(&self, level: &str, message: &str, data: serde_json::Value) {
        let entry = TestLogEntry {
            level: level.to_string(),
            message: message.to_string(),
            data,
        };
        self.entries.write().unwrap().push(entry);
    }

    /// Get all log entries.
    pub fn entries(&self) -> Vec<TestLogEntry> {
        self.entries.read().unwrap().clone()
    }

    /// Get the cron name.
    pub fn cron_name(&self) -> &str {
        &self.cron_name
    }
}

/// Test context for cron functions.
///
/// Provides an isolated testing environment for crons with delay detection,
/// catch-up simulation, and structured logging.
///
/// # Example
///
/// ```ignore
/// let ctx = TestCronContext::builder("daily_cleanup")
///     .scheduled_at(Utc::now() - Duration::minutes(5))
///     .build();
///
/// assert!(ctx.is_late());
///
/// ctx.log.info("Starting cleanup");
/// assert_eq!(ctx.log.entries().len(), 1);
/// ```
pub struct TestCronContext {
    /// Cron run ID.
    pub run_id: Uuid,
    /// Cron name.
    pub cron_name: String,
    /// Scheduled time.
    pub scheduled_time: DateTime<Utc>,
    /// Execution time.
    pub execution_time: DateTime<Utc>,
    /// Timezone.
    pub timezone: String,
    /// Whether this is a catch-up run.
    pub is_catch_up: bool,
    /// Authentication context.
    pub auth: AuthContext,
    /// Structured logger.
    pub log: TestCronLog,
    /// Optional database pool.
    pool: Option<PgPool>,
    /// Mock HTTP client.
    http: Arc<MockHttp>,
}

impl TestCronContext {
    /// Create a new builder.
    pub fn builder(cron_name: impl Into<String>) -> TestCronContextBuilder {
        TestCronContextBuilder::new(cron_name)
    }

    /// Get the database pool (if available).
    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Get the mock HTTP client.
    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    /// Get the delay between scheduled and actual execution time.
    pub fn delay(&self) -> Duration {
        self.execution_time - self.scheduled_time
    }

    /// Check if the cron is running late (more than 1 minute delay).
    pub fn is_late(&self) -> bool {
        self.delay() > Duration::minutes(1)
    }
}

/// Builder for TestCronContext.
pub struct TestCronContextBuilder {
    run_id: Option<Uuid>,
    cron_name: String,
    scheduled_time: DateTime<Utc>,
    execution_time: DateTime<Utc>,
    timezone: String,
    is_catch_up: bool,
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    pool: Option<PgPool>,
    http: MockHttp,
}

impl TestCronContextBuilder {
    /// Create a new builder.
    pub fn new(cron_name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            run_id: None,
            cron_name: cron_name.into(),
            scheduled_time: now,
            execution_time: now,
            timezone: "UTC".to_string(),
            is_catch_up: false,
            user_id: None,
            roles: Vec::new(),
            claims: HashMap::new(),
            pool: None,
            http: MockHttp::new(),
        }
    }

    /// Set a specific run ID.
    pub fn with_run_id(mut self, id: Uuid) -> Self {
        self.run_id = Some(id);
        self
    }

    /// Set the scheduled time.
    pub fn scheduled_at(mut self, time: DateTime<Utc>) -> Self {
        self.scheduled_time = time;
        self
    }

    /// Set the execution time.
    pub fn executed_at(mut self, time: DateTime<Utc>) -> Self {
        self.execution_time = time;
        self
    }

    /// Set the timezone.
    pub fn with_timezone(mut self, tz: impl Into<String>) -> Self {
        self.timezone = tz.into();
        self
    }

    /// Mark as a catch-up run.
    pub fn as_catch_up(mut self) -> Self {
        self.is_catch_up = true;
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

    /// Build the test context.
    pub fn build(self) -> TestCronContext {
        let auth = if let Some(user_id) = self.user_id {
            AuthContext::authenticated(user_id, self.roles, self.claims)
        } else {
            AuthContext::unauthenticated()
        };

        TestCronContext {
            run_id: self.run_id.unwrap_or_else(Uuid::new_v4),
            cron_name: self.cron_name.clone(),
            scheduled_time: self.scheduled_time,
            execution_time: self.execution_time,
            timezone: self.timezone,
            is_catch_up: self.is_catch_up,
            auth,
            log: TestCronLog::new(self.cron_name),
            pool: self.pool,
            http: Arc::new(self.http),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_context_creation() {
        let ctx = TestCronContext::builder("daily_cleanup").build();

        assert_eq!(ctx.cron_name, "daily_cleanup");
        assert!(!ctx.is_catch_up);
        assert!(!ctx.is_late());
    }

    #[test]
    fn test_catch_up_run() {
        let ctx = TestCronContext::builder("hourly_sync")
            .as_catch_up()
            .build();

        assert!(ctx.is_catch_up);
    }

    #[test]
    fn test_late_detection() {
        let scheduled = Utc::now() - Duration::minutes(5);
        let ctx = TestCronContext::builder("quick_task")
            .scheduled_at(scheduled)
            .build();

        assert!(ctx.is_late());
        assert!(ctx.delay() >= Duration::minutes(4));
    }

    #[test]
    fn test_logging() {
        let ctx = TestCronContext::builder("test_cron").build();

        ctx.log.info("Starting");
        ctx.log.warn("Warning message");
        ctx.log.error("Error occurred");

        let entries = ctx.log.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].level, "info");
        assert_eq!(entries[1].level, "warn");
        assert_eq!(entries[2].level, "error");
    }

    #[test]
    fn test_timezone() {
        let ctx = TestCronContext::builder("tz_test")
            .with_timezone("America/New_York")
            .build();

        assert_eq!(ctx.timezone, "America/New_York");
    }
}

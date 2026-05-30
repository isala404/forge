//! Test context for cron functions.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::super::mock_http::{MockHttp, MockRequest, MockResponse};
use super::build_test_auth;
use crate::env::{EnvAccess, EnvProvider, MockEnvProvider};
use crate::function::AuthContext;

/// Log entry recorded during testing.
#[derive(Debug, Clone)]
pub struct TestLogEntry {
    pub level: String,
    pub message: String,
    pub data: serde_json::Value,
}

/// Test log for cron context.
#[derive(Clone)]
pub struct TestCronLog {
    cron_name: String,
    entries: Arc<RwLock<Vec<TestLogEntry>>>,
}

impl TestCronLog {
    pub fn new(cron_name: impl Into<String>) -> Self {
        Self {
            cron_name: cron_name.into(),
            entries: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn info(&self, message: &str) {
        self.log("info", message, serde_json::Value::Null);
    }

    pub fn info_with(&self, message: &str, data: serde_json::Value) {
        self.log("info", message, data);
    }

    pub fn warn(&self, message: &str) {
        self.log("warn", message, serde_json::Value::Null);
    }

    pub fn warn_with(&self, message: &str, data: serde_json::Value) {
        self.log("warn", message, data);
    }

    pub fn error(&self, message: &str) {
        self.log("error", message, serde_json::Value::Null);
    }

    pub fn error_with(&self, message: &str, data: serde_json::Value) {
        self.log("error", message, data);
    }

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

    pub fn entries(&self) -> Vec<TestLogEntry> {
        self.entries.read().unwrap().clone()
    }

    pub fn cron_name(&self) -> &str {
        &self.cron_name
    }
}

/// Test context for cron functions.
pub struct TestCronContext {
    pub run_id: Uuid,
    pub cron_name: String,
    pub scheduled_time: DateTime<Utc>,
    pub execution_time: DateTime<Utc>,
    pub timezone: String,
    pub is_catch_up: bool,
    pub auth: AuthContext,
    pub log: TestCronLog,
    pool: Option<PgPool>,
    http: Arc<MockHttp>,
    env_provider: Arc<MockEnvProvider>,
}

impl TestCronContext {
    pub fn builder(cron_name: impl Into<String>) -> TestCronContextBuilder {
        TestCronContextBuilder::new(cron_name)
    }

    pub fn db(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    pub fn http(&self) -> &MockHttp {
        &self.http
    }

    pub fn delay(&self) -> Duration {
        self.execution_time - self.scheduled_time
    }

    /// More than 1 minute behind schedule.
    pub fn is_late(&self) -> bool {
        self.delay() > Duration::minutes(1)
    }

    pub fn env_mock(&self) -> &MockEnvProvider {
        &self.env_provider
    }
}

impl EnvAccess for TestCronContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
    }
}

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
    env_vars: HashMap<String, String>,
}

impl_test_auth_builder!(TestCronContextBuilder);
impl_test_env_builder!(TestCronContextBuilder);

impl TestCronContextBuilder {
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
            env_vars: HashMap::new(),
        }
    }

    pub fn with_run_id(mut self, id: Uuid) -> Self {
        self.run_id = Some(id);
        self
    }

    pub fn scheduled_at(mut self, time: DateTime<Utc>) -> Self {
        self.scheduled_time = time;
        self
    }

    pub fn executed_at(mut self, time: DateTime<Utc>) -> Self {
        self.execution_time = time;
        self
    }

    pub fn with_timezone(mut self, tz: impl Into<String>) -> Self {
        self.timezone = tz.into();
        self
    }

    pub fn as_catch_up(mut self) -> Self {
        self.is_catch_up = true;
        self
    }
    pub fn mock_http<F>(self, pattern: &str, handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.http.add_mock_sync(pattern, handler);
        self
    }

    pub fn mock_http_json<T: serde::Serialize>(self, pattern: &str, response: T) -> Self {
        let json = serde_json::to_value(response).unwrap_or(serde_json::Value::Null);
        self.mock_http(pattern, move |_| MockResponse::json(json.clone()))
    }
    pub fn build(self) -> TestCronContext {
        TestCronContext {
            run_id: self.run_id.unwrap_or_else(Uuid::new_v4),
            cron_name: self.cron_name.clone(),
            scheduled_time: self.scheduled_time,
            execution_time: self.execution_time,
            timezone: self.timezone,
            is_catch_up: self.is_catch_up,
            auth: build_test_auth(self.user_id, self.roles, self.claims),
            log: TestCronLog::new(self.cron_name),
            pool: self.pool,
            http: Arc::new(self.http),
            env_provider: Arc::new(MockEnvProvider::with_vars(self.env_vars)),
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

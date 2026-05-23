//! HTTP mocking utilities for testing.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

/// Mock HTTP client for testing.
///
/// Mocks are matched **first-registered-wins**: the first handler whose
/// pattern matches the request URL (or path) is used. Register specific
/// patterns before broad wildcards.
///
/// Patterns are matched against both the full URL and the path component,
/// so `"/health"` matches a request to `https://internal:8080/health`.
#[derive(Clone)]
pub struct MockHttp {
    mocks: Arc<RwLock<Vec<MockHandler>>>,
    requests: Arc<RwLock<Vec<RecordedRequest>>>,
}

pub type BoxedHandler = Box<dyn Fn(&MockRequest) -> MockResponse + Send + Sync>;

struct MockHandler {
    pattern: String,
    handler: Arc<dyn Fn(&MockRequest) -> MockResponse + Send + Sync>,
}

/// A recorded request for verification.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: serde_json::Value,
}

/// Mock HTTP request.
#[derive(Debug, Clone)]
pub struct MockRequest {
    pub method: String,
    pub path: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: serde_json::Value,
}

/// Mock HTTP response.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: serde_json::Value,
}

impl MockResponse {
    pub fn json<T: Serialize>(body: T) -> Self {
        Self {
            status: 200,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: serde_json::to_value(body).unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: serde_json::json!({ "error": message }),
        }
    }

    pub fn internal_error(message: &str) -> Self {
        Self::error(500, message)
    }

    pub fn not_found(message: &str) -> Self {
        Self::error(404, message)
    }

    pub fn unauthorized(message: &str) -> Self {
        Self::error(401, message)
    }

    pub fn ok() -> Self {
        Self::json(serde_json::json!({}))
    }
}

impl MockHttp {
    pub fn new() -> Self {
        Self {
            mocks: Arc::new(RwLock::new(Vec::new())),
            requests: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn builder() -> MockHttpBuilder {
        MockHttpBuilder::new()
    }

    /// Add a mock handler (sync version for use in builders).
    ///
    /// The pattern supports `*` as a glob wildcard. Mocks are matched
    /// first-registered-wins against both the full URL and the path.
    pub fn add_mock_sync<F>(&self, pattern: &str, handler: F)
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        let mut mocks = self.mocks.write().unwrap();
        mocks.push(MockHandler {
            pattern: pattern.to_string(),
            handler: Arc::new(handler),
        });
    }

    /// No wildcards; use `mock_glob` for patterns.
    pub fn mock_exact<F>(&self, url: &str, handler: F)
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.add_mock_sync(url, handler);
    }

    /// Add a mock handler with glob pattern (`*` matches any substring).
    ///
    /// Register specific patterns before broad ones — first match wins.
    pub fn mock_glob<F>(&self, pattern: &str, handler: F)
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.add_mock_sync(pattern, handler);
    }

    pub fn add_mock_boxed(&mut self, pattern: &str, handler: BoxedHandler) {
        let mut mocks = self.mocks.write().unwrap();
        mocks.push(MockHandler {
            pattern: pattern.to_string(),
            handler: Arc::from(handler),
        });
    }

    pub async fn execute(&self, request: MockRequest) -> MockResponse {
        {
            let mut requests = self.requests.write().unwrap();
            requests.push(RecordedRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            });
        }

        let mocks = self.mocks.read().unwrap();
        for mock in mocks.iter() {
            if self.matches_pattern(&request.url, &mock.pattern)
                || self.matches_pattern(&request.path, &mock.pattern)
            {
                return (mock.handler)(&request);
            }
        }

        MockResponse::error(500, &format!("No mock found for {}", request.url))
    }

    fn matches_pattern(&self, url: &str, pattern: &str) -> bool {
        let pattern_parts: Vec<&str> = pattern.split('*').collect();
        if pattern_parts.len() == 1 {
            return url == pattern;
        }

        let mut remaining = url;
        for (i, part) in pattern_parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }

            if i == 0 {
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if i == pattern_parts.len() - 1 {
                if !remaining.ends_with(part) {
                    return false;
                }
            } else if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }

        true
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.read().unwrap().clone()
    }

    pub fn requests_blocking(&self) -> Vec<RecordedRequest> {
        self.requests.read().unwrap().clone()
    }

    pub fn requests_to(&self, pattern: &str) -> Vec<RecordedRequest> {
        self.requests
            .read()
            .unwrap()
            .iter()
            .filter(|r| self.matches_pattern(&r.url, pattern))
            .cloned()
            .collect()
    }

    pub fn clear_requests(&self) {
        self.requests.write().unwrap().clear();
    }

    pub fn clear_mocks(&self) {
        self.mocks.write().unwrap().clear();
    }

    /// Assert that a URL pattern was called.
    pub fn assert_called(&self, pattern: &str) {
        let requests = self.requests_blocking();
        let matching = requests
            .iter()
            .filter(|r| self.matches_pattern(&r.url, pattern))
            .count();
        assert!(
            matching > 0,
            "Expected HTTP call matching '{}', but none found. Recorded requests: {:?}",
            pattern,
            requests.iter().map(|r| &r.url).collect::<Vec<_>>()
        );
    }

    /// Assert that a URL pattern was called a specific number of times.
    pub fn assert_called_times(&self, pattern: &str, expected: usize) {
        let requests = self.requests_blocking();
        let matching = requests
            .iter()
            .filter(|r| self.matches_pattern(&r.url, pattern))
            .count();
        assert_eq!(
            matching, expected,
            "Expected {} HTTP calls matching '{}', but found {}",
            expected, pattern, matching
        );
    }

    /// Assert that a URL pattern was not called.
    pub fn assert_not_called(&self, pattern: &str) {
        let requests = self.requests_blocking();
        let matching = requests
            .iter()
            .filter(|r| self.matches_pattern(&r.url, pattern))
            .count();
        assert_eq!(
            matching, 0,
            "Expected no HTTP calls matching '{}', but found {}",
            pattern, matching
        );
    }

    /// Assert that a request was made with specific body content.
    pub fn assert_called_with_body<F>(&self, pattern: &str, predicate: F)
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let requests = self.requests_blocking();
        let matching = requests
            .iter()
            .filter(|r| self.matches_pattern(&r.url, pattern) && predicate(&r.body));
        assert!(
            matching.count() > 0,
            "Expected HTTP call matching '{}' with matching body, but none found",
            pattern
        );
    }
}

impl Default for MockHttp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MockHttpBuilder {
    mocks: Vec<(String, BoxedHandler)>,
}

impl MockHttpBuilder {
    pub fn new() -> Self {
        Self { mocks: Vec::new() }
    }

    pub fn mock<F>(mut self, pattern: &str, handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.mocks.push((pattern.to_string(), Box::new(handler)));
        self
    }

    pub fn mock_json<T: Serialize + Clone + Send + Sync + 'static>(
        self,
        pattern: &str,
        response: T,
    ) -> Self {
        self.mock(pattern, move |_| MockResponse::json(response.clone()))
    }

    pub fn build(self) -> MockHttp {
        let mut mock = MockHttp::new();
        for (pattern, handler) in self.mocks {
            mock.add_mock_boxed(&pattern, handler);
        }
        mock
    }
}

impl Default for MockHttpBuilder {
    fn default() -> Self {
        Self::new()
    }
}

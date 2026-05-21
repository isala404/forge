//! HTTP mocking utilities for testing.
//!
//! Provides a mock HTTP client that intercepts requests and returns
//! predefined responses. Supports pattern matching and request recording
//! for verification.

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
///
/// # Example
///
/// ```ignore
/// let mock = MockHttp::new();
/// mock.mock_exact("https://api.example.com/users", |_| {
///     MockResponse::json(json!({"users": []}))
/// });
/// mock.mock_glob("https://api.example.com/*", |_| {
///     MockResponse::json(json!({"status": "ok"}))
/// });
///
/// let response = mock.execute(request).await;
/// mock.assert_called("https://api.example.com/users");
/// ```
#[derive(Clone)]
pub struct MockHttp {
    mocks: Arc<RwLock<Vec<MockHandler>>>,
    requests: Arc<RwLock<Vec<RecordedRequest>>>,
}

/// Type alias for mock handler closure.
pub type BoxedHandler = Box<dyn Fn(&MockRequest) -> MockResponse + Send + Sync>;

/// A mock handler.
struct MockHandler {
    pattern: String,
    handler: Arc<dyn Fn(&MockRequest) -> MockResponse + Send + Sync>,
}

/// A recorded request for verification.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Request method.
    pub method: String,
    /// Request URL.
    pub url: String,
    /// Request headers.
    pub headers: HashMap<String, String>,
    /// Request body.
    pub body: serde_json::Value,
}

/// Mock HTTP request.
#[derive(Debug, Clone)]
pub struct MockRequest {
    /// Request method.
    pub method: String,
    /// Request path.
    pub path: String,
    /// Request URL.
    pub url: String,
    /// Request headers.
    pub headers: HashMap<String, String>,
    /// Request body.
    pub body: serde_json::Value,
}

/// Mock HTTP response.
#[derive(Debug, Clone)]
pub struct MockResponse {
    /// Status code.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Response body.
    pub body: serde_json::Value,
}

impl MockResponse {
    /// Create a successful JSON response.
    pub fn json<T: Serialize>(body: T) -> Self {
        Self {
            status: 200,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: serde_json::to_value(body).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Create an error response.
    pub fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: serde_json::json!({ "error": message }),
        }
    }

    /// Create a 500 internal error.
    pub fn internal_error(message: &str) -> Self {
        Self::error(500, message)
    }

    /// Create a 404 not found.
    pub fn not_found(message: &str) -> Self {
        Self::error(404, message)
    }

    /// Create a 401 unauthorized.
    pub fn unauthorized(message: &str) -> Self {
        Self::error(401, message)
    }

    /// Create an empty 200 OK response.
    pub fn ok() -> Self {
        Self::json(serde_json::json!({}))
    }
}

impl MockHttp {
    /// Create a new mock HTTP client.
    pub fn new() -> Self {
        Self {
            mocks: Arc::new(RwLock::new(Vec::new())),
            requests: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create a builder.
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

    /// Add a mock handler for an exact URL or path (no wildcards).
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

    /// Add a mock handler from a boxed closure.
    pub fn add_mock_boxed(&mut self, pattern: &str, handler: BoxedHandler) {
        let mut mocks = self.mocks.write().unwrap();
        mocks.push(MockHandler {
            pattern: pattern.to_string(),
            handler: Arc::from(handler),
        });
    }

    /// Execute a mock request.
    pub async fn execute(&self, request: MockRequest) -> MockResponse {
        // Record the request
        {
            let mut requests = self.requests.write().unwrap();
            requests.push(RecordedRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            });
        }

        // Find matching mock
        let mocks = self.mocks.read().unwrap();
        for mock in mocks.iter() {
            if self.matches_pattern(&request.url, &mock.pattern)
                || self.matches_pattern(&request.path, &mock.pattern)
            {
                return (mock.handler)(&request);
            }
        }

        // No mock found
        MockResponse::error(500, &format!("No mock found for {}", request.url))
    }

    /// Check if a URL matches a pattern.
    fn matches_pattern(&self, url: &str, pattern: &str) -> bool {
        // Convert glob pattern to simple matching
        let pattern_parts: Vec<&str> = pattern.split('*').collect();
        if pattern_parts.len() == 1 {
            // No wildcards - exact match
            return url == pattern;
        }

        let mut remaining = url;
        for (i, part) in pattern_parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }

            if i == 0 {
                // First part must match at start
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if i == pattern_parts.len() - 1 {
                // Last part must match at end
                if !remaining.ends_with(part) {
                    return false;
                }
            } else {
                // Middle parts can match anywhere
                if let Some(pos) = remaining.find(part) {
                    remaining = &remaining[pos + part.len()..];
                } else {
                    return false;
                }
            }
        }

        true
    }

    /// Get recorded requests.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.read().unwrap().clone()
    }

    /// Get recorded requests (blocking version for use in sync contexts).
    pub fn requests_blocking(&self) -> Vec<RecordedRequest> {
        self.requests.read().unwrap().clone()
    }

    /// Get requests matching a pattern.
    pub fn requests_to(&self, pattern: &str) -> Vec<RecordedRequest> {
        self.requests
            .read()
            .unwrap()
            .iter()
            .filter(|r| self.matches_pattern(&r.url, pattern))
            .cloned()
            .collect()
    }

    /// Clear recorded requests.
    pub fn clear_requests(&self) {
        self.requests.write().unwrap().clear();
    }

    /// Clear all mocks.
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

/// Builder for MockHttp.
pub struct MockHttpBuilder {
    mocks: Vec<(String, BoxedHandler)>,
}

impl MockHttpBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self { mocks: Vec::new() }
    }

    /// Add a mock with a custom handler.
    pub fn mock<F>(mut self, pattern: &str, handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        self.mocks.push((pattern.to_string(), Box::new(handler)));
        self
    }

    /// Add a mock that returns a JSON response.
    pub fn mock_json<T: Serialize + Clone + Send + Sync + 'static>(
        self,
        pattern: &str,
        response: T,
    ) -> Self {
        self.mock(pattern, move |_| MockResponse::json(response.clone()))
    }

    /// Build the MockHttp.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_response_json() {
        let response = MockResponse::json(serde_json::json!({"id": 123}));
        assert_eq!(response.status, 200);
        assert_eq!(response.body["id"], 123);
    }

    #[test]
    fn test_mock_response_error() {
        let response = MockResponse::error(404, "Not found");
        assert_eq!(response.status, 404);
        assert_eq!(response.body["error"], "Not found");
    }

    #[test]
    fn test_pattern_matching() {
        let mock = MockHttp::new();

        // Exact match
        assert!(mock.matches_pattern(
            "https://api.example.com/users",
            "https://api.example.com/users"
        ));

        // Wildcard at end
        assert!(mock.matches_pattern(
            "https://api.example.com/users/123",
            "https://api.example.com/*"
        ));

        // Wildcard in middle
        assert!(mock.matches_pattern(
            "https://api.example.com/v2/users",
            "https://api.example.com/*/users"
        ));

        // No match
        assert!(!mock.matches_pattern("https://other.com/users", "https://api.example.com/*"));
    }

    #[tokio::test]
    async fn test_mock_execution() {
        let mock = MockHttp::new();
        mock.add_mock_sync("https://api.example.com/*", |_| {
            MockResponse::json(serde_json::json!({"status": "ok"}))
        });

        let request = MockRequest {
            method: "GET".to_string(),
            path: "/users".to_string(),
            url: "https://api.example.com/users".to_string(),
            headers: HashMap::new(),
            body: serde_json::Value::Null,
        };

        let response = mock.execute(request).await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body["status"], "ok");
    }

    #[tokio::test]
    async fn test_request_recording() {
        let mock = MockHttp::new();
        mock.add_mock_sync("*", |_| MockResponse::ok());

        let request = MockRequest {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            url: "https://api.example.com/users".to_string(),
            headers: HashMap::from([("authorization".to_string(), "Bearer token".to_string())]),
            body: serde_json::json!({"name": "Test"}),
        };

        let _ = mock.execute(request).await;

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].body["name"], "Test");
    }

    #[tokio::test]
    async fn test_assert_called() {
        let mock = MockHttp::new();
        mock.add_mock_sync("*", |_| MockResponse::ok());

        let request = MockRequest {
            method: "GET".to_string(),
            path: "/test".to_string(),
            url: "https://api.example.com/test".to_string(),
            headers: HashMap::new(),
            body: serde_json::Value::Null,
        };

        let _ = mock.execute(request).await;

        mock.assert_called("https://api.example.com/*");
        mock.assert_called_times("https://api.example.com/*", 1);
        mock.assert_not_called("https://other.com/*");
    }

    #[test]
    fn test_builder() {
        let mock = MockHttpBuilder::new()
            .mock("https://api.example.com/*", |_| MockResponse::ok())
            .mock_json("https://other.com/*", serde_json::json!({"id": 1}))
            .build();

        assert_eq!(mock.mocks.read().unwrap().len(), 2);
    }

    fn req(method: &str, url: &str, path: &str) -> MockRequest {
        MockRequest {
            method: method.to_string(),
            path: path.to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            body: serde_json::Value::Null,
        }
    }

    #[test]
    fn response_status_helpers_use_documented_codes() {
        assert_eq!(MockResponse::internal_error("boom").status, 500);
        assert_eq!(MockResponse::not_found("nope").status, 404);
        assert_eq!(MockResponse::unauthorized("nope").status, 401);
        assert_eq!(MockResponse::ok().status, 200);

        // ok() returns an empty JSON object — handlers that key off body shape
        // (e.g., serde to () or empty struct) rely on this.
        assert_eq!(MockResponse::ok().body, serde_json::json!({}));
    }

    #[test]
    fn response_json_sets_content_type_header() {
        let r = MockResponse::json(serde_json::json!({"ok": true}));
        assert_eq!(
            r.headers.get("content-type"),
            Some(&"application/json".to_string())
        );
    }

    #[test]
    fn pattern_matcher_handles_leading_and_double_wildcards() {
        let m = MockHttp::new();
        // Leading wildcard (pattern_parts[0] is empty).
        assert!(m.matches_pattern("https://api.example.com/v1/users", "*/users"));
        assert!(!m.matches_pattern("https://api.example.com/v1/posts", "*/users"));

        // Bare `*` matches anything (both pattern parts are empty strings).
        assert!(m.matches_pattern("anything", "*"));
        assert!(m.matches_pattern("", "*"));
    }

    #[test]
    fn pattern_matcher_rejects_exact_pattern_with_extra_suffix() {
        let m = MockHttp::new();
        assert!(!m.matches_pattern(
            "https://api.example.com/users/extra",
            "https://api.example.com/users"
        ));
    }

    #[tokio::test]
    async fn execute_falls_back_to_500_when_no_mock_matches() {
        let mock = MockHttp::new();
        let r = mock.execute(req("GET", "https://nowhere/", "/")).await;
        assert_eq!(r.status, 500);
        assert!(
            r.body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("No mock found"),
            "fallback should explain the failure, got {:?}",
            r.body
        );
    }

    #[tokio::test]
    async fn execute_records_request_even_when_no_mock_matches() {
        // The recording happens before the lookup so failed-match calls still
        // show up in requests() — important for diagnosing "why didn't my mock fire".
        let mock = MockHttp::new();
        let _ = mock.execute(req("DELETE", "https://nowhere/x", "/x")).await;
        let recorded = mock.requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].method, "DELETE");
        assert_eq!(recorded[0].url, "https://nowhere/x");
    }

    #[tokio::test]
    async fn execute_matches_against_path_when_url_misses() {
        // Pattern only matches the path, not the full URL.
        let mock = MockHttp::new();
        mock.add_mock_sync("/health", |_| MockResponse::ok());
        let r = mock
            .execute(req("GET", "https://internal.svc:8080/health", "/health"))
            .await;
        assert_eq!(r.status, 200);
    }

    #[tokio::test]
    async fn execute_uses_first_registered_mock_on_overlapping_patterns() {
        let mock = MockHttp::new();
        mock.add_mock_sync("https://api.example.com/*", |_| {
            MockResponse::json(serde_json::json!({"hit": "first"}))
        });
        mock.add_mock_sync("https://api.example.com/users", |_| {
            MockResponse::json(serde_json::json!({"hit": "second"}))
        });

        let r = mock
            .execute(req("GET", "https://api.example.com/users", "/users"))
            .await;
        assert_eq!(r.body["hit"], "first");
    }

    #[tokio::test]
    async fn requests_to_filters_by_pattern() {
        let mock = MockHttp::new();
        mock.add_mock_sync("*", |_| MockResponse::ok());

        let _ = mock
            .execute(req("GET", "https://api.example.com/a", "/a"))
            .await;
        let _ = mock.execute(req("GET", "https://other.com/b", "/b")).await;
        let _ = mock
            .execute(req("GET", "https://api.example.com/c", "/c"))
            .await;

        let api_calls = mock.requests_to("https://api.example.com/*");
        assert_eq!(api_calls.len(), 2);
        assert!(api_calls.iter().all(|r| r.url.contains("api.example.com")));
    }

    #[tokio::test]
    async fn clear_requests_and_clear_mocks_independently_reset_state() {
        let mock = MockHttp::new();
        mock.add_mock_sync("*", |_| MockResponse::ok());
        let _ = mock.execute(req("GET", "https://x/", "/")).await;
        assert_eq!(mock.requests().len(), 1);

        mock.clear_requests();
        assert!(mock.requests().is_empty());
        // Mocks survive a requests-only clear; the next call should still match.
        let r = mock.execute(req("GET", "https://x/", "/")).await;
        assert_eq!(r.status, 200);

        mock.clear_mocks();
        let r = mock.execute(req("GET", "https://x/", "/")).await;
        assert_eq!(r.status, 500, "after clear_mocks, fallback should hit");
    }

    #[tokio::test]
    async fn assert_called_with_body_runs_predicate_against_recorded_body() {
        let mock = MockHttp::new();
        mock.add_mock_sync("*", |_| MockResponse::ok());

        let mut request = req("POST", "https://api/upload", "/upload");
        request.body = serde_json::json!({"size": 42});
        let _ = mock.execute(request).await;

        // Predicate matches — should not panic.
        mock.assert_called_with_body("https://api/*", |body| body["size"] == 42);
    }

    #[test]
    fn defaults_match_new() {
        // Default impls are wrappers around new(); just exercise them so the
        // Default path doesn't silently rot.
        let m1 = MockHttp::default();
        assert!(m1.requests().is_empty());
        let b1 = MockHttpBuilder::default();
        let m2 = b1.build();
        assert!(m2.requests().is_empty());
    }
}

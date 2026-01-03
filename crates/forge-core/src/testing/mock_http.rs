//! HTTP mocking utilities for testing.
//!
//! Provides a mock HTTP client that intercepts requests and returns
//! predefined responses. Supports pattern matching and request recording
//! for verification.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

/// Mock HTTP client for testing.
///
/// # Example
///
/// ```ignore
/// let mut mock = MockHttp::new();
/// mock.add_mock_sync("https://api.example.com/*", |req| {
///     MockResponse::json(json!({"status": "ok"}))
/// });
///
/// let response = mock.execute(request).await;
/// mock.assert_called("https://api.example.com/*");
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

    // =========================================================================
    // VERIFICATION METHODS
    // =========================================================================

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
}

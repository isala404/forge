//! HTTP client with circuit breaker pattern.
//!
//! Wraps `reqwest::Client` with automatic failure tracking per host.
//! After repeated failures, requests fail fast to prevent cascade failures.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use reqwest::{IntoUrl, Method, Request, RequestBuilder, Response};
use std::net::{IpAddr, SocketAddr};

/// Circuit breaker state for a single host.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CircuitState {
    /// Current state of the circuit.
    pub state: CircuitStatus,
    /// Number of consecutive failures.
    pub failure_count: u32,
    /// Number of consecutive successes (used in half-open state).
    pub success_count: u32,
    /// When the circuit was opened (for timeout calculation).
    pub opened_at: Option<Instant>,
    /// Current backoff duration.
    pub current_backoff: Duration,
}

/// Circuit breaker status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CircuitStatus {
    /// Normal operation, requests pass through.
    Closed,
    /// Circuit tripped, requests fail fast.
    Open,
    /// Testing if service recovered, limited requests allowed.
    HalfOpen,
}

impl Default for CircuitState {
    fn default() -> Self {
        Self {
            state: CircuitStatus::Closed,
            failure_count: 0,
            success_count: 0,
            opened_at: None,
            current_backoff: Duration::from_secs(30),
        }
    }
}

/// Configuration for the circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening the circuit.
    pub failure_threshold: u32,
    /// Number of successes in half-open state before closing.
    pub success_threshold: u32,
    /// Initial timeout before trying half-open.
    pub base_timeout: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
    /// Backoff multiplier for exponential backoff.
    pub backoff_multiplier: f64,
    /// Whether the circuit breaker is enabled.
    pub enabled: bool,
    /// Allow requests to private/loopback/link-local IPs.
    /// Defaults to `false` to block SSRF targets like `127.0.0.1`
    /// and the AWS/GCE metadata endpoint `169.254.169.254`. Flip to `true`
    /// in development or when calling internal services on private CIDRs.
    /// Clients built via `build_ssrf_safe_client` also block hostnames
    /// that resolve to private IPs at the DNS layer.
    pub allow_private: bool,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            base_timeout: Duration::from_secs(30),
            max_backoff: Duration::from_secs(600), // 10 minutes
            backoff_multiplier: 1.5,
            enabled: true,
            allow_private: false,
        }
    }
}

/// Error returned when circuit breaker is open.
#[derive(Debug, Clone)]
pub struct CircuitBreakerOpen {
    /// The host that is being blocked.
    pub host: String,
    /// Time until the circuit may try again.
    pub retry_after: Duration,
}

impl std::fmt::Display for CircuitBreakerOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Circuit breaker open for {}: retry after {:?}",
            self.host, self.retry_after
        )
    }
}

impl std::error::Error for CircuitBreakerOpen {}

/// Returns true when the given IP is in a loopback, private (RFC 1918 / ULA),
/// link-local, broadcast, unspecified, or documentation range.
/// Also handles IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`).
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            // IPv4-mapped addresses (::ffff:0:0/96) — check the inner v4
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(v4);
            }
            let seg0 = v6.segments().first().copied().unwrap_or(0);
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (seg0 & 0xfe00) == 0xfc00 // ULA fc00::/7
        }
    }
}

fn is_private_v4(v4: std::net::Ipv4Addr) -> bool {
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_unspecified()
        || v4.is_documentation()
}

/// DNS resolver that filters out private/loopback/link-local addresses from
/// resolution results. Prevents SSRF via DNS rebinding or hostnames that
/// resolve to internal IPs (e.g. `metadata.internal` -> `169.254.169.254`).
struct SsrfSafeResolver;

impl reqwest::dns::Resolve for SsrfSafeResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host(format!("{host}:0")).await?.collect();
            let safe: Vec<SocketAddr> = addrs
                .into_iter()
                .filter(|addr| !is_private_ip(addr.ip()))
                .collect();
            if safe.is_empty() {
                return Err(
                    format!("DNS resolution for {host} returned only private IPs").into(),
                );
            }
            let addrs: reqwest::dns::Addrs = Box::new(safe.into_iter());
            Ok(addrs)
        })
    }
}

/// Build a reqwest client with SSRF-safe DNS resolution. Hostnames that
/// resolve to private/loopback/link-local IPs are rejected at the DNS layer.
///
/// # Panics
///
/// Panics if the TLS backend is unavailable, which is a fatal startup error.
pub fn build_ssrf_safe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .dns_resolver(std::sync::Arc::new(SsrfSafeResolver))
        .build()
        .unwrap_or_else(|e| {
            tracing::error!("Failed to build SSRF-safe HTTP client: {e}");
            // This only fails when the TLS backend is missing. Proceeding with
            // an unprotected client would silently remove DNS-level SSRF guards,
            // so we propagate the failure as a panic at startup.
            unreachable!("TLS backend required for HTTP client")
        })
}

/// HTTP client with circuit breaker pattern.
///
/// Tracks failure rates per host and fails fast when a host is unhealthy.
#[derive(Clone)]
pub struct CircuitBreakerClient {
    inner: reqwest::Client,
    states: std::sync::Arc<RwLock<HashMap<String, CircuitState>>>,
    config: CircuitBreakerConfig,
}

impl CircuitBreakerClient {
    /// Create a new circuit breaker client wrapping the given reqwest client.
    pub fn new(client: reqwest::Client, config: CircuitBreakerConfig) -> Self {
        Self {
            inner: client,
            states: std::sync::Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Create with default configuration.
    pub fn with_defaults(client: reqwest::Client) -> Self {
        Self::new(client, CircuitBreakerConfig::default())
    }

    /// Create with default configuration and SSRF-safe DNS resolution.
    /// Hostnames resolving to private/loopback/link-local IPs are blocked.
    pub fn with_ssrf_protection() -> Self {
        Self::new(build_ssrf_safe_client(), CircuitBreakerConfig::default())
    }

    /// Get the underlying reqwest client for building requests.
    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Create a request client view with an optional default request timeout.
    pub fn with_timeout(&self, timeout: Option<Duration>) -> HttpClient {
        HttpClient::new(self.clone(), timeout)
    }

    /// Extract host from URL for tracking.
    fn extract_host(url: &reqwest::Url) -> String {
        format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str().unwrap_or("unknown"),
            url.port().map(|p| format!(":{}", p)).unwrap_or_default()
        )
    }

    /// Returns true when the URL's host is a literal IP in a private range.
    /// Hostnames are not resolved here — DNS-level SSRF protection is handled
    /// by `SsrfSafeResolver` when the client is built via `build_ssrf_safe_client`.
    fn url_targets_private_ip(url: &reqwest::Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let trimmed = host.trim_start_matches('[').trim_end_matches(']');
        let Ok(ip) = trimmed.parse::<IpAddr>() else {
            return false;
        };
        is_private_ip(ip)
    }

    /// Check if a request to the given host should be allowed.
    pub fn should_allow(&self, host: &str) -> Result<(), CircuitBreakerOpen> {
        if !self.config.enabled {
            return Ok(());
        }

        let states = self.states.read().unwrap_or_else(|e| {
            tracing::error!("Circuit breaker lock was poisoned, recovering");
            e.into_inner()
        });
        let state = match states.get(host) {
            Some(s) => s,
            None => return Ok(()), // No state = first request, allow
        };

        match state.state {
            CircuitStatus::Closed => Ok(()),
            CircuitStatus::HalfOpen => Ok(()), // Allow test requests
            CircuitStatus::Open => {
                let opened_at = state.opened_at.unwrap_or_else(Instant::now);
                let elapsed = opened_at.elapsed();

                if elapsed >= state.current_backoff {
                    // Timeout expired, will transition to half-open
                    Ok(())
                } else {
                    Err(CircuitBreakerOpen {
                        host: host.to_string(),
                        retry_after: state.current_backoff - elapsed,
                    })
                }
            }
        }
    }

    /// Record a successful request.
    pub fn record_success(&self, host: &str) {
        if !self.config.enabled {
            return;
        }

        let mut states = self.states.write().unwrap_or_else(|e| {
            tracing::error!("Circuit breaker lock was poisoned, recovering");
            e.into_inner()
        });
        let state = states.entry(host.to_string()).or_default();

        match state.state {
            CircuitStatus::Closed => {
                // Reset failure count on success
                state.failure_count = 0;
            }
            CircuitStatus::HalfOpen => {
                state.success_count += 1;
                if state.success_count >= self.config.success_threshold {
                    // Service recovered, close the circuit
                    tracing::info!(host = %host, "Circuit breaker closed, service recovered");
                    state.state = CircuitStatus::Closed;
                    state.failure_count = 0;
                    state.success_count = 0;
                    state.opened_at = None;
                    state.current_backoff = self.config.base_timeout;
                }
            }
            CircuitStatus::Open => {
                // Transition to half-open on first success after timeout
                tracing::info!(host = %host, "Circuit breaker half-open, testing service");
                state.state = CircuitStatus::HalfOpen;
                state.success_count = 1;
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self, host: &str) {
        if !self.config.enabled {
            return;
        }

        let mut states = self.states.write().unwrap_or_else(|e| {
            tracing::error!("Circuit breaker lock was poisoned, recovering");
            e.into_inner()
        });
        let state = states.entry(host.to_string()).or_default();

        match state.state {
            CircuitStatus::Closed => {
                state.failure_count += 1;
                if state.failure_count >= self.config.failure_threshold {
                    // Trip the circuit
                    tracing::warn!(
                        host = %host,
                        failures = state.failure_count,
                        "Circuit breaker opened, service unhealthy"
                    );
                    state.state = CircuitStatus::Open;
                    state.opened_at = Some(Instant::now());
                }
            }
            CircuitStatus::HalfOpen => {
                // Failed during test, reopen with increased backoff
                let new_backoff = Duration::from_secs_f64(
                    (state.current_backoff.as_secs_f64() * self.config.backoff_multiplier)
                        .min(self.config.max_backoff.as_secs_f64()),
                );
                tracing::warn!(
                    host = %host,
                    backoff_secs = new_backoff.as_secs(),
                    "Circuit breaker reopened, service still unhealthy"
                );
                state.state = CircuitStatus::Open;
                state.opened_at = Some(Instant::now());
                state.current_backoff = new_backoff;
                state.success_count = 0;
            }
            CircuitStatus::Open => {
                // Already open, just update timestamp
                state.opened_at = Some(Instant::now());
            }
        }
    }

    /// Execute a request with circuit breaker protection.
    pub async fn execute(&self, request: Request) -> Result<Response, CircuitBreakerError> {
        // SSRF guard: refuse private/loopback/link-local IP literals unless
        // the operator has opted in.
        if !self.config.allow_private && Self::url_targets_private_ip(request.url()) {
            return Err(CircuitBreakerError::PrivateHostBlocked(
                request.url().host_str().unwrap_or("unknown").to_string(),
            ));
        }

        let host = Self::extract_host(request.url());

        // Check circuit state
        self.should_allow(&host)
            .map_err(CircuitBreakerError::CircuitOpen)?;

        // If circuit is open but timeout expired, transition to half-open
        {
            let mut states = self.states.write().unwrap_or_else(|e| {
                tracing::error!("Circuit breaker lock was poisoned, recovering");
                e.into_inner()
            });
            if let Some(state) = states.get_mut(&host)
                && state.state == CircuitStatus::Open
                && let Some(opened_at) = state.opened_at
                && opened_at.elapsed() >= state.current_backoff
            {
                tracing::info!(host = %host, "Circuit breaker half-open, testing service");
                state.state = CircuitStatus::HalfOpen;
                state.success_count = 0;
            }
        }

        // Execute the request
        match self.inner.execute(request).await {
            Ok(response) => {
                // Check if response indicates server error
                if response.status().is_server_error() {
                    self.record_failure(&host);
                } else {
                    self.record_success(&host);
                }
                Ok(response)
            }
            Err(e) => {
                self.record_failure(&host);
                Err(CircuitBreakerError::Request(e))
            }
        }
    }

    /// Get the current state for a host.
    pub fn get_state(&self, host: &str) -> Option<CircuitState> {
        self.states
            .read()
            .unwrap_or_else(|e| {
                tracing::error!("Circuit breaker lock was poisoned, recovering");
                e.into_inner()
            })
            .get(host)
            .cloned()
    }

    /// Reset the circuit breaker state for a host.
    pub fn reset(&self, host: &str) {
        self.states
            .write()
            .unwrap_or_else(|e| {
                tracing::error!("Circuit breaker lock was poisoned, recovering");
                e.into_inner()
            })
            .remove(host);
    }

    /// Reset all circuit breaker states.
    pub fn reset_all(&self) {
        self.states
            .write()
            .unwrap_or_else(|e| {
                tracing::error!("Circuit breaker lock was poisoned, recovering");
                e.into_inner()
            })
            .clear();
    }
}

/// Error type for circuit breaker operations.
#[derive(Debug)]
pub enum CircuitBreakerError {
    /// The circuit is open, request was not attempted.
    CircuitOpen(CircuitBreakerOpen),
    /// Outbound request blocked because the URL host resolves to a
    /// private/loopback/link-local IP and `allow_private` is false.
    PrivateHostBlocked(String),
    /// The request failed.
    Request(reqwest::Error),
}

impl std::fmt::Display for CircuitBreakerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitBreakerError::CircuitOpen(e) => write!(f, "{}", e),
            CircuitBreakerError::PrivateHostBlocked(_host) => write!(
                f,
                "Outbound request blocked: target resolves to a private IP"
            ),
            CircuitBreakerError::Request(e) => write!(f, "HTTP request failed: {}", e),
        }
    }
}

impl std::error::Error for CircuitBreakerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CircuitBreakerError::CircuitOpen(e) => Some(e),
            CircuitBreakerError::PrivateHostBlocked(_) => None,
            CircuitBreakerError::Request(e) => Some(e),
        }
    }
}

impl From<reqwest::Error> for CircuitBreakerError {
    fn from(e: reqwest::Error) -> Self {
        CircuitBreakerError::Request(e)
    }
}

/// HTTP client facade that routes requests through a circuit breaker and can
/// apply a default timeout to requests that do not set one explicitly.
#[derive(Clone)]
pub struct HttpClient {
    circuit_breaker: CircuitBreakerClient,
    default_timeout: Option<Duration>,
}

impl HttpClient {
    /// Create a new HTTP client facade.
    pub fn new(circuit_breaker: CircuitBreakerClient, default_timeout: Option<Duration>) -> Self {
        Self {
            circuit_breaker,
            default_timeout,
        }
    }

    /// Get the underlying reqwest client.
    pub fn inner(&self) -> &reqwest::Client {
        self.circuit_breaker.inner()
    }

    /// Get the underlying circuit breaker client.
    pub fn circuit_breaker(&self) -> &CircuitBreakerClient {
        &self.circuit_breaker
    }

    /// Get the default timeout applied to requests that do not override it.
    pub fn default_timeout(&self) -> Option<Duration> {
        self.default_timeout
    }

    /// Create a request builder.
    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> HttpRequestBuilder {
        HttpRequestBuilder::new(self.clone(), self.inner().request(method, url))
    }

    pub fn get<U: IntoUrl>(&self, url: U) -> HttpRequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post<U: IntoUrl>(&self, url: U) -> HttpRequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn put<U: IntoUrl>(&self, url: U) -> HttpRequestBuilder {
        self.request(Method::PUT, url)
    }

    pub fn patch<U: IntoUrl>(&self, url: U) -> HttpRequestBuilder {
        self.request(Method::PATCH, url)
    }

    pub fn delete<U: IntoUrl>(&self, url: U) -> HttpRequestBuilder {
        self.request(Method::DELETE, url)
    }

    pub fn head<U: IntoUrl>(&self, url: U) -> HttpRequestBuilder {
        self.request(Method::HEAD, url)
    }

    /// Execute a pre-built request through the circuit breaker.
    pub async fn execute(&self, mut request: Request) -> crate::Result<Response> {
        self.apply_default_timeout(&mut request);
        self.circuit_breaker
            .execute(request)
            .await
            .map_err(Into::into)
    }

    fn apply_default_timeout(&self, request: &mut Request) {
        if request.timeout().is_none()
            && let Some(timeout) = self.default_timeout
        {
            *request.timeout_mut() = Some(timeout);
        }
    }
}

/// Request builder paired with a circuit-breaker-backed HTTP client.
pub struct HttpRequestBuilder {
    client: HttpClient,
    request: RequestBuilder,
}

impl HttpRequestBuilder {
    fn new(client: HttpClient, request: RequestBuilder) -> Self {
        Self { client, request }
    }

    pub fn header(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        Self {
            request: self.request.header(key.as_ref(), value.as_ref()),
            ..self
        }
    }

    pub fn headers(self, headers: reqwest::header::HeaderMap) -> Self {
        Self {
            request: self.request.headers(headers),
            ..self
        }
    }

    pub fn bearer_auth(self, token: impl std::fmt::Display) -> Self {
        Self {
            request: self.request.bearer_auth(token),
            ..self
        }
    }

    pub fn basic_auth(
        self,
        username: impl std::fmt::Display,
        password: Option<impl std::fmt::Display>,
    ) -> Self {
        Self {
            request: self.request.basic_auth(username, password),
            ..self
        }
    }

    pub fn body(self, body: impl Into<reqwest::Body>) -> Self {
        Self {
            request: self.request.body(body),
            ..self
        }
    }

    pub fn json(self, json: &impl serde::Serialize) -> Self {
        Self {
            request: self.request.json(json),
            ..self
        }
    }

    pub fn form(self, form: &impl serde::Serialize) -> Self {
        Self {
            request: self.request.form(form),
            ..self
        }
    }

    pub fn query(self, query: &impl serde::Serialize) -> Self {
        Self {
            request: self.request.query(query),
            ..self
        }
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        Self {
            request: self.request.timeout(timeout),
            ..self
        }
    }

    pub fn version(self, version: reqwest::Version) -> Self {
        Self {
            request: self.request.version(version),
            ..self
        }
    }

    pub fn try_clone(&self) -> Option<Self> {
        self.request.try_clone().map(|request| Self {
            client: self.client.clone(),
            request,
        })
    }

    pub fn build(self) -> crate::Result<Request> {
        self.request
            .build()
            .map_err(|e| crate::ForgeError::Internal(e.to_string()))
    }

    pub async fn send(self) -> crate::Result<Response> {
        let client = self.client.clone();
        let request = self.build()?;
        client.execute(request).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_defaults() {
        let config = CircuitBreakerConfig::default();
        assert_eq!(config.failure_threshold, 5);
        assert_eq!(config.success_threshold, 2);
        assert!(config.enabled);
    }

    #[test]
    fn test_circuit_state_transitions() {
        let client = reqwest::Client::new();
        let breaker = CircuitBreakerClient::with_defaults(client);
        let host = "https://api.example.com";

        // Initial state should allow
        assert!(breaker.should_allow(host).is_ok());

        // Record failures to trip the circuit
        for _ in 0..5 {
            breaker.record_failure(host);
        }

        // Circuit should be open
        let state = breaker.get_state(host).unwrap();
        assert_eq!(state.state, CircuitStatus::Open);

        // Should be blocked
        assert!(breaker.should_allow(host).is_err());

        // Reset and verify
        breaker.reset(host);
        assert!(breaker.should_allow(host).is_ok());
    }

    #[test]
    fn test_extract_host() {
        let url = reqwest::Url::parse("https://api.example.com:8080/path").unwrap();
        assert_eq!(
            CircuitBreakerClient::extract_host(&url),
            "https://api.example.com:8080"
        );

        let url2 = reqwest::Url::parse("http://localhost/api").unwrap();
        assert_eq!(
            CircuitBreakerClient::extract_host(&url2),
            "http://localhost"
        );
    }

    #[test]
    fn test_http_client_applies_default_timeout_when_missing() {
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let client = breaker.with_timeout(Some(Duration::from_secs(5)));
        let mut request = reqwest::Request::new(
            Method::GET,
            reqwest::Url::parse("https://example.com").unwrap(),
        );

        client.apply_default_timeout(&mut request);

        assert_eq!(request.timeout(), Some(&Duration::from_secs(5)));
    }

    #[test]
    fn test_http_client_preserves_explicit_timeout() {
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let client = breaker.with_timeout(Some(Duration::from_secs(5)));
        let mut request = reqwest::Request::new(
            Method::GET,
            reqwest::Url::parse("https://example.com").unwrap(),
        );
        *request.timeout_mut() = Some(Duration::from_secs(1));

        client.apply_default_timeout(&mut request);

        assert_eq!(request.timeout(), Some(&Duration::from_secs(1)));
    }

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).expect("valid url")
    }

    fn breaker_with(config: CircuitBreakerConfig) -> CircuitBreakerClient {
        CircuitBreakerClient::new(reqwest::Client::new(), config)
    }

    // ---- SSRF guard ----

    #[test]
    fn private_ip_guard_blocks_ipv4_loopback_and_metadata_endpoint() {
        // These are the cases that most matter operationally — 127.0.0.1 and
        // the AWS/GCE metadata IP 169.254.169.254 (link-local).
        assert!(CircuitBreakerClient::url_targets_private_ip(&url(
            "http://127.0.0.1/"
        )));
        assert!(CircuitBreakerClient::url_targets_private_ip(&url(
            "http://169.254.169.254/latest/meta-data/"
        )));
    }

    #[test]
    fn private_ip_guard_blocks_all_ipv4_classes_doc_says_it_blocks() {
        // Walk every RFC class the docstring promises to cover; if one slips
        // the matrix, the SSRF guarantee is broken.
        let blocked = [
            "http://10.0.0.1/",      // private 10/8
            "http://172.16.0.1/",    // private 172.16/12
            "http://192.168.1.1/",   // private 192.168/16
            "http://169.254.1.1/",   // link-local
            "http://0.0.0.0/",       // unspecified
            "http://255.255.255.255/", // broadcast
            "http://192.0.2.1/",     // documentation TEST-NET-1
            "http://198.51.100.1/",  // documentation TEST-NET-2
            "http://203.0.113.1/",   // documentation TEST-NET-3
        ];
        for u in blocked {
            assert!(
                CircuitBreakerClient::url_targets_private_ip(&url(u)),
                "should block {u}"
            );
        }
    }

    #[test]
    fn private_ip_guard_blocks_ipv6_loopback_link_local_and_ula() {
        // IPv6 mirror of the IPv4 cases. The bracket-trimming logic must
        // strip the URL-encoded brackets before parsing.
        let blocked = [
            "http://[::1]/",          // loopback
            "http://[::]/",           // unspecified
            "http://[fe80::1]/",      // link-local fe80::/10
            "http://[febf::1]/",      // link-local upper edge
            "http://[fc00::1]/",      // ULA fc00::/7
            "http://[fd00::1]/",      // ULA upper half
        ];
        for u in blocked {
            assert!(
                CircuitBreakerClient::url_targets_private_ip(&url(u)),
                "should block {u}"
            );
        }
    }

    #[test]
    fn private_ip_guard_allows_public_ips_and_dns_hostnames() {
        // Public IP literals must pass — the guard is opt-out via
        // allow_private, not a blanket "no IPs at all" filter.
        let allowed = [
            "http://1.1.1.1/",
            "http://8.8.8.8/",
            "http://[2001:4860:4860::8888]/", // Google public DNS v6
            // Hostnames pass the URL-literal check; DNS-level blocking is
            // handled by SsrfSafeResolver at connect time.
            "http://api.example.com/",
            "http://localhost/",
        ];
        for u in allowed {
            assert!(
                !CircuitBreakerClient::url_targets_private_ip(&url(u)),
                "should NOT block {u}"
            );
        }
    }

    #[tokio::test]
    async fn execute_returns_private_host_blocked_error_when_guard_trips() {
        // Verify the guard is wired into execute() — not just the helper.
        let breaker = breaker_with(CircuitBreakerConfig {
            allow_private: false,
            ..Default::default()
        });
        let req = reqwest::Request::new(Method::GET, url("http://127.0.0.1/"));
        let err = breaker.execute(req).await.expect_err("loopback blocked");
        match err {
            CircuitBreakerError::PrivateHostBlocked(host) => {
                assert_eq!(host, "127.0.0.1");
            }
            other => panic!("expected PrivateHostBlocked, got {other:?}"),
        }
    }

    // ---- is_private_ip ----

    #[test]
    fn is_private_ip_blocks_all_private_ranges() {
        let blocked: Vec<IpAddr> = vec![
            "127.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "172.16.0.1".parse().unwrap(),
            "192.168.1.1".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            "255.255.255.255".parse().unwrap(),
            "::1".parse().unwrap(),
            "::".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            "fc00::1".parse().unwrap(),
            "fd00::1".parse().unwrap(),
        ];
        for ip in blocked {
            assert!(is_private_ip(ip), "should block {ip}");
        }
    }

    #[test]
    fn is_private_ip_blocks_ipv4_mapped_ipv6() {
        let mapped: Vec<IpAddr> = vec![
            "::ffff:127.0.0.1".parse().unwrap(),
            "::ffff:10.0.0.1".parse().unwrap(),
            "::ffff:169.254.169.254".parse().unwrap(),
            "::ffff:192.168.1.1".parse().unwrap(),
        ];
        for ip in mapped {
            assert!(is_private_ip(ip), "should block IPv4-mapped {ip}");
        }
    }

    #[test]
    fn is_private_ip_allows_public_addresses() {
        let allowed: Vec<IpAddr> = vec![
            "1.1.1.1".parse().unwrap(),
            "8.8.8.8".parse().unwrap(),
            "93.184.216.34".parse().unwrap(),
            "2001:4860:4860::8888".parse().unwrap(),
        ];
        for ip in allowed {
            assert!(!is_private_ip(ip), "should allow {ip}");
        }
    }

    // ---- Half-open state machine ----

    #[test]
    fn success_in_half_open_below_threshold_keeps_circuit_half_open() {
        // success_threshold defaults to 2 — so a single success after
        // open->half-open must NOT yet close the circuit.
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let host = "https://flaky.example.com";

        for _ in 0..5 {
            breaker.record_failure(host);
        }
        assert_eq!(breaker.get_state(host).unwrap().state, CircuitStatus::Open);

        // Open->HalfOpen on first success after timeout.
        breaker.record_success(host);
        let s = breaker.get_state(host).unwrap();
        assert_eq!(s.state, CircuitStatus::HalfOpen);
        assert_eq!(s.success_count, 1);

        // One more success would meet threshold — but we stop at one to
        // pin "below threshold stays half-open."
    }

    #[test]
    fn second_success_in_half_open_closes_circuit_and_resets_counters() {
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let host = "https://flaky2.example.com";

        for _ in 0..5 {
            breaker.record_failure(host);
        }
        breaker.record_success(host); // -> HalfOpen
        breaker.record_success(host); // -> Closed (threshold = 2)

        let s = breaker.get_state(host).unwrap();
        assert_eq!(s.state, CircuitStatus::Closed);
        assert_eq!(s.failure_count, 0);
        assert_eq!(s.success_count, 0);
        assert!(s.opened_at.is_none(), "opened_at must clear on full recovery");
    }

    #[test]
    fn failure_in_half_open_reopens_with_exponential_backoff() {
        let breaker = breaker_with(CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            base_timeout: Duration::from_secs(10),
            max_backoff: Duration::from_secs(600),
            backoff_multiplier: 2.0,
            enabled: true,
            allow_private: true,
        });
        let host = "https://still-down.example.com";

        // Trip and partially recover.
        for _ in 0..3 {
            breaker.record_failure(host);
        }
        // current_backoff defaults to 30s from CircuitState::default(); the
        // first failure-in-half-open multiplies that by backoff_multiplier.
        let initial_backoff = breaker.get_state(host).unwrap().current_backoff;
        breaker.record_success(host); // -> HalfOpen
        breaker.record_failure(host); // -> Open with backoff * multiplier

        let s = breaker.get_state(host).unwrap();
        assert_eq!(s.state, CircuitStatus::Open);
        assert_eq!(s.success_count, 0, "success_count must reset on reopen");
        let expected = Duration::from_secs_f64(initial_backoff.as_secs_f64() * 2.0);
        assert_eq!(
            s.current_backoff, expected,
            "backoff must scale by multiplier on reopen"
        );
    }

    #[test]
    fn failure_in_half_open_caps_backoff_at_max() {
        // Pick a max well below what the multiplier would otherwise produce
        // and verify saturation kicks in.
        let breaker = breaker_with(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            base_timeout: Duration::from_secs(30),
            max_backoff: Duration::from_secs(45),
            backoff_multiplier: 10.0,
            enabled: true,
            allow_private: true,
        });
        let host = "https://capped.example.com";

        breaker.record_failure(host); // -> Open
        breaker.record_success(host); // -> HalfOpen
        breaker.record_failure(host); // -> Open, attempted backoff = 30 * 10 = 300s, capped at 45

        let s = breaker.get_state(host).unwrap();
        assert_eq!(s.current_backoff, Duration::from_secs(45));
    }

    #[test]
    fn record_failure_while_open_just_refreshes_opened_at_without_changing_state() {
        // No transition; just confirms the Open branch in record_failure
        // doesn't accidentally trip a re-counted "extra failure" path.
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let host = "https://still-open.example.com";
        for _ in 0..5 {
            breaker.record_failure(host);
        }
        let before = breaker.get_state(host).unwrap();
        assert_eq!(before.state, CircuitStatus::Open);

        // Sleep a tick so opened_at can advance even at coarse clock res.
        std::thread::sleep(Duration::from_millis(2));
        breaker.record_failure(host);

        let after = breaker.get_state(host).unwrap();
        assert_eq!(after.state, CircuitStatus::Open);
        assert!(
            after.opened_at.unwrap() >= before.opened_at.unwrap(),
            "opened_at should be refreshed or unchanged, not regressed"
        );
        assert_eq!(after.current_backoff, before.current_backoff);
    }

    // ---- enabled = false short-circuits ----

    #[test]
    fn disabled_breaker_never_blocks_and_never_records_state() {
        let breaker = breaker_with(CircuitBreakerConfig {
            enabled: false,
            ..Default::default()
        });
        let host = "https://noop.example.com";

        for _ in 0..100 {
            breaker.record_failure(host);
        }
        // Nothing should have been stored — the early-return in
        // record_failure must skip even creating a state entry.
        assert!(breaker.get_state(host).is_none());
        assert!(breaker.should_allow(host).is_ok());

        // record_success is similarly a no-op.
        breaker.record_success(host);
        assert!(breaker.get_state(host).is_none());
    }

    // ---- reset / reset_all ----

    #[test]
    fn reset_all_clears_state_for_every_host() {
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        breaker.record_failure("https://a.example.com");
        breaker.record_failure("https://b.example.com");
        breaker.record_failure("https://c.example.com");
        assert!(breaker.get_state("https://a.example.com").is_some());

        breaker.reset_all();
        assert!(breaker.get_state("https://a.example.com").is_none());
        assert!(breaker.get_state("https://b.example.com").is_none());
        assert!(breaker.get_state("https://c.example.com").is_none());
    }

    // ---- should_allow with expired timeout ----

    #[test]
    fn should_allow_returns_ok_when_open_timeout_has_elapsed() {
        // Stuff an already-expired opened_at into the map and verify
        // should_allow lets the request through to drive the transition.
        let breaker = breaker_with(CircuitBreakerConfig {
            failure_threshold: 1,
            base_timeout: Duration::from_millis(10),
            ..Default::default()
        });
        let host = "https://ready.example.com";
        breaker.record_failure(host);
        // Force an opened_at well in the past so elapsed >= current_backoff.
        {
            let mut states = breaker.states.write().unwrap();
            let s = states.get_mut(host).unwrap();
            s.opened_at = Some(Instant::now() - Duration::from_secs(3600));
            s.current_backoff = Duration::from_millis(10);
        }
        assert!(
            breaker.should_allow(host).is_ok(),
            "expired open circuit must allow the next request through"
        );
    }

    #[test]
    fn should_allow_reports_retry_after_when_open_and_within_backoff() {
        let breaker = breaker_with(CircuitBreakerConfig {
            failure_threshold: 1,
            base_timeout: Duration::from_secs(60),
            ..Default::default()
        });
        let host = "https://hot.example.com";
        breaker.record_failure(host);

        let err = breaker.should_allow(host).expect_err("still open");
        assert_eq!(err.host, host);
        // retry_after must be > 0 and <= current_backoff.
        let backoff = breaker.get_state(host).unwrap().current_backoff;
        assert!(err.retry_after > Duration::ZERO);
        assert!(err.retry_after <= backoff);
    }

    // ---- extract_host edges ----

    #[test]
    fn extract_host_handles_default_ports_and_no_port() {
        // Default 443 / 80 are elided by the URL parser, so they should not
        // appear in the extracted host string.
        assert_eq!(
            CircuitBreakerClient::extract_host(&url("https://api.example.com/")),
            "https://api.example.com"
        );
        assert_eq!(
            CircuitBreakerClient::extract_host(&url("http://api.example.com/")),
            "http://api.example.com"
        );
        // Non-default port appears.
        assert_eq!(
            CircuitBreakerClient::extract_host(&url("https://api.example.com:8443/")),
            "https://api.example.com:8443"
        );
    }

    #[test]
    fn extract_host_includes_ipv6_brackets() {
        // host_str() returns the bare IPv6 without brackets — the formatter
        // produces a host string that round-trips through later URL parsing.
        let h = CircuitBreakerClient::extract_host(&url("http://[::1]:8080/"));
        assert!(h.contains("::1"), "got: {h}");
        assert!(h.ends_with(":8080"), "got: {h}");
    }

    // ---- error Display / source ----

    #[test]
    fn circuit_breaker_open_display_mentions_host_and_retry_after() {
        let err = CircuitBreakerOpen {
            host: "https://flaky.example.com".to_string(),
            retry_after: Duration::from_secs(42),
        };
        let s = err.to_string();
        assert!(s.contains("https://flaky.example.com"));
        assert!(s.contains("42"));
    }

    #[test]
    fn private_host_blocked_display_redacts_host() {
        let err = CircuitBreakerError::PrivateHostBlocked("127.0.0.1".to_string());
        let s = err.to_string();
        assert!(!s.contains("127.0.0.1"), "host must not leak through Display");
        assert!(s.contains("private IP"));
    }

    #[test]
    fn circuit_breaker_error_source_chains_through_inner_variants() {
        // CircuitOpen wraps CircuitBreakerOpen — must surface as source.
        let inner = CircuitBreakerOpen {
            host: "h".to_string(),
            retry_after: Duration::from_secs(1),
        };
        let err = CircuitBreakerError::CircuitOpen(inner);
        assert!(
            std::error::Error::source(&err).is_some(),
            "CircuitOpen should expose its wrapped error as source"
        );

        // PrivateHostBlocked has no upstream cause.
        let err = CircuitBreakerError::PrivateHostBlocked("h".to_string());
        assert!(
            std::error::Error::source(&err).is_none(),
            "PrivateHostBlocked has no source"
        );
    }

    // ---- HttpClient defaults ----

    #[test]
    fn http_client_apply_default_timeout_is_noop_when_default_unset() {
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let client = breaker.with_timeout(None);
        let mut req = reqwest::Request::new(Method::GET, url("https://example.com/"));
        client.apply_default_timeout(&mut req);
        assert_eq!(req.timeout(), None);
    }

    #[test]
    fn http_client_accessors_expose_underlying_pieces() {
        let breaker = CircuitBreakerClient::with_defaults(reqwest::Client::new());
        let client = breaker.with_timeout(Some(Duration::from_secs(7)));
        assert_eq!(client.default_timeout(), Some(Duration::from_secs(7)));
        // inner() and circuit_breaker() exist as load-bearing public API;
        // calling them confirms they don't panic and return a usable handle.
        let _ = client.inner();
        let _ = client.circuit_breaker();
    }
}

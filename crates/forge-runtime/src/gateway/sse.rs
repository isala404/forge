//! Server-Sent Events (SSE) handler for real-time updates.

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Maximum number of outstanding SSE tickets held in memory. Each ticket is
/// small (~200 B), so 10k is a ~2 MB cap. New issuance evicts expired
/// entries before falling back to a hard reject.
const MAX_SSE_TICKETS: usize = 10_000;
/// SSE ticket lifetime. The original ISSUES.md item requires "≤ 60 s";
/// 30 s is short enough to bound replay risk yet generous for slow clients
/// to complete the POST + EventSource handshake.
const SSE_TICKET_TTL_SECS: u64 = 30;
const SSE_TICKET_TTL_SECS_STR: &str = "30";

/// One-shot SSE authentication ticket. Issued via `POST /events/ticket`
/// against a validated bearer header; consumed exactly once by the SSE
/// `GET /events?ticket=…` upgrade. Stored in-process (no DB), bound to
/// the caller's resolved client IP so a leaked ticket from logs cannot
/// be replayed from a different origin.
struct TicketEntry {
    auth: AuthContext,
    client_ip: Option<String>,
    expires_at: Instant,
}

/// Process-local store of outstanding SSE tickets. Bounded, TTL-evicted,
/// one-time-use. Tickets are opaque uuid v4 strings (122 bits).
#[derive(Default)]
struct TicketStore {
    entries: DashMap<String, TicketEntry>,
}

impl TicketStore {
    fn new() -> Self {
        Self {
            entries: DashMap::new(),
        }
    }

    /// Drop expired tickets. Called opportunistically on insert and consume.
    fn sweep_expired(&self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires_at > now);
    }

    /// Insert a fresh ticket. Returns `false` when at capacity even after
    /// sweeping expired entries (caller should reject with 503).
    fn insert(&self, ticket: String, entry: TicketEntry) -> bool {
        if self.entries.len() >= MAX_SSE_TICKETS {
            self.sweep_expired();
            if self.entries.len() >= MAX_SSE_TICKETS {
                return false;
            }
        }
        self.entries.insert(ticket, entry);
        true
    }

    /// Atomically remove and validate a ticket. Returns `None` if missing,
    /// already consumed, or expired.
    fn consume(&self, ticket: &str) -> Option<TicketEntry> {
        let (_, entry) = self.entries.remove(ticket)?;
        if entry.expires_at <= Instant::now() {
            return None;
        }
        Some(entry)
    }
}

/// Wraps an mpsc::Receiver as a Stream for SSE.
struct ReceiverStream<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.rx.poll_recv(cx)
    }
}

use forge_core::function::AuthContext;
use forge_core::realtime::{SessionId, SubscriptionId};

use crate::realtime::Reactor;
use crate::realtime::RealtimeMessage;

/// Maximum length for client subscription IDs to prevent memory bloat
const MAX_CLIENT_SUB_ID_LEN: usize = 255;

/// Retry-After hint sent on the SSE-at-capacity 503 response. Clients are
/// expected to back off and retry after this many seconds.
const SSE_AT_CAPACITY_RETRY_SECS: u64 = 5;
const SSE_AT_CAPACITY_RETRY_SECS_STR: &str = "5";

fn try_parse_session_id(session_id: &str) -> Option<SessionId> {
    uuid::Uuid::parse_str(session_id)
        .ok()
        .map(SessionId::from_uuid)
}

fn same_principal(a: &AuthContext, b: &AuthContext) -> bool {
    match (a.is_authenticated(), b.is_authenticated()) {
        (false, false) => true,
        (true, true) => a.principal_id().is_some() && a.principal_id() == b.principal_id(),
        _ => false,
    }
}

#[allow(clippy::result_large_err)]
fn authorize_session_access(
    session: &SseSessionData,
    session_secret: &str,
    requester_auth: &AuthContext,
) -> Result<AuthContext, (StatusCode, Json<SseSubscribeResponse>)> {
    let secret_match: bool = session
        .session_secret
        .as_bytes()
        .ct_eq(session_secret.as_bytes())
        .into();
    if !secret_match {
        return Err(subscribe_error(
            StatusCode::UNAUTHORIZED,
            "INVALID_SESSION_SECRET",
            "Session secret mismatch",
        ));
    }

    if !same_principal(&session.auth_context, requester_auth) {
        return Err(subscribe_error(
            StatusCode::FORBIDDEN,
            "SESSION_PRINCIPAL_MISMATCH",
            "Request principal does not match session principal",
        ));
    }

    Ok(session.auth_context.clone())
}

/// Validate that a client subscription ID does not exceed the maximum length.
#[allow(clippy::result_large_err)]
fn validate_client_sub_id(id: &str) -> Result<(), (StatusCode, Json<SseSubscribeResponse>)> {
    if id.len() > MAX_CLIENT_SUB_ID_LEN {
        return Err(subscribe_error(
            StatusCode::BAD_REQUEST,
            "INVALID_ID",
            format!(
                "Subscription ID too long (max {} chars)",
                MAX_CLIENT_SUB_ID_LEN
            ),
        ));
    }
    Ok(())
}

/// Parse, look up, and authorize a session from a subscribe-style request.
///
/// Returns `(session_id, session_auth_context)` on success, or an error
/// response that can be returned directly from the handler.
#[allow(clippy::result_large_err)]
async fn validate_session(
    state: &SseState,
    session_id_str: &str,
    session_secret: &str,
    request_auth: &AuthContext,
) -> Result<(SessionId, AuthContext), (StatusCode, Json<SseSubscribeResponse>)> {
    let Some(session_id) = try_parse_session_id(session_id_str) else {
        return Err(subscribe_error(
            StatusCode::BAD_REQUEST,
            "INVALID_SESSION",
            "Invalid session ID format",
        ));
    };

    match state.sessions.get(&session_id) {
        Some(session) => {
            let auth = authorize_session_access(&session, session_secret, request_auth)?;
            Ok((session_id, auth))
        }
        None => Err(subscribe_error(
            StatusCode::NOT_FOUND,
            "SESSION_NOT_FOUND",
            "Session not found or expired",
        )),
    }
}

/// SSE configuration.
#[derive(Debug, Clone)]
pub struct SseConfig {
    /// Maximum sessions per server instance.
    pub max_sessions: usize,
    /// Channel buffer size for SSE messages.
    pub channel_buffer_size: usize,
    /// Keepalive interval in seconds.
    pub keepalive_interval_secs: u64,
    /// Maximum subscriptions per session.
    pub max_subscriptions_per_session: usize,
    /// Maximum concurrent SSE sessions per authenticated user.
    pub max_sessions_per_user: usize,
    /// Maximum concurrent SSE sessions per source IP.
    pub max_sessions_per_ip: usize,
    /// Cap on a user's total subscriptions across every active session.
    pub max_subscriptions_per_user: usize,
}

impl Default for SseConfig {
    fn default() -> Self {
        Self {
            max_sessions: 10_000,
            channel_buffer_size: 256,
            keepalive_interval_secs: 30,
            max_subscriptions_per_session: 100,
            max_sessions_per_user: 8,
            max_sessions_per_ip: 32,
            max_subscriptions_per_user: 500,
        }
    }
}

/// SSE query parameters.
#[derive(Debug, Deserialize)]
pub struct SseQuery {
    /// One-shot ticket obtained from `POST /events/ticket`. Required when
    /// `EventSource` cannot send an `Authorization` header (browsers).
    pub ticket: Option<String>,
}

struct SseSessionData {
    auth_context: AuthContext,
    session_secret: String,
    client_ip: Option<String>,
    /// Maps client subscription ID -> internal SubscriptionId
    subscriptions: HashMap<String, SubscriptionId>,
}

/// State for SSE handler.
#[derive(Clone)]
pub struct SseState {
    reactor: Arc<Reactor>,
    /// Per-session data: auth context and subscription mappings (sharded).
    sessions: Arc<DashMap<SessionId, SseSessionData>>,
    /// Per-user session count for O(1) limit enforcement.
    user_session_counts: Arc<DashMap<uuid::Uuid, AtomicUsize>>,
    /// Per-IP session count for O(1) limit enforcement.
    ip_session_counts: Arc<DashMap<String, AtomicUsize>>,
    /// Per-user subscription count across all sessions.
    user_subscription_counts: Arc<DashMap<uuid::Uuid, AtomicUsize>>,
    /// One-shot SSE auth tickets. See `TicketStore` for semantics.
    tickets: Arc<TicketStore>,
    config: SseConfig,
}

impl SseState {
    /// Create new SSE state with default config.
    pub fn new(reactor: Arc<Reactor>) -> Self {
        Self::with_config(reactor, SseConfig::default())
    }

    /// Create new SSE state with custom config.
    pub fn with_config(reactor: Arc<Reactor>, config: SseConfig) -> Self {
        Self {
            reactor,
            sessions: Arc::new(DashMap::new()),
            user_session_counts: Arc::new(DashMap::new()),
            ip_session_counts: Arc::new(DashMap::new()),
            user_subscription_counts: Arc::new(DashMap::new()),
            tickets: Arc::new(TicketStore::new()),
            config,
        }
    }

    /// Check if we can accept new sessions.
    pub fn can_accept_session(&self) -> bool {
        self.sessions.len() < self.config.max_sessions
    }

    fn increment_user_sessions(&self, user_id: uuid::Uuid) {
        self.user_session_counts
            .entry(user_id)
            .or_insert_with(|| AtomicUsize::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    fn decrement_user_sessions(&self, user_id: uuid::Uuid) {
        if let Some(counter) = self.user_session_counts.get(&user_id) {
            let prev = counter.fetch_sub(1, Ordering::Relaxed);
            if prev <= 1 {
                drop(counter);
                self.user_session_counts.remove(&user_id);
            }
        }
    }

    fn increment_ip_sessions(&self, ip: &str) {
        self.ip_session_counts
            .entry(ip.to_string())
            .or_insert_with(|| AtomicUsize::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    fn decrement_ip_sessions(&self, ip: &str) {
        if let Some(counter) = self.ip_session_counts.get(ip) {
            let prev = counter.fetch_sub(1, Ordering::Relaxed);
            if prev <= 1 {
                drop(counter);
                self.ip_session_counts.remove(ip);
            }
        }
    }

    fn user_session_count(&self, user_id: uuid::Uuid) -> usize {
        self.user_session_counts
            .get(&user_id)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn ip_session_count(&self, ip: &str) -> usize {
        self.ip_session_counts
            .get(ip)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn increment_user_subscriptions(&self, user_id: uuid::Uuid) {
        self.user_subscription_counts
            .entry(user_id)
            .or_insert_with(|| AtomicUsize::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    fn decrement_user_subscriptions(&self, user_id: uuid::Uuid, count: usize) {
        if let Some(counter) = self.user_subscription_counts.get(&user_id) {
            let prev = counter.fetch_sub(count, Ordering::Relaxed);
            if prev <= count {
                drop(counter);
                self.user_subscription_counts.remove(&user_id);
            }
        }
    }

    fn user_subscription_count(&self, user_id: uuid::Uuid) -> usize {
        self.user_subscription_counts
            .get(&user_id)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn remove_session(&self, session_id: SessionId) {
        if let Some((_, session)) = self.sessions.remove(&session_id) {
            if let Some(user_id) = session.auth_context.user_id() {
                self.decrement_user_sessions(user_id);
                let sub_count = session.subscriptions.len();
                if sub_count > 0 {
                    self.decrement_user_subscriptions(user_id, sub_count);
                }
            }
            if let Some(ip) = &session.client_ip {
                self.decrement_ip_sessions(ip);
            }
        }
    }
}

/// Guard to ensure session cleanup on disconnect.
/// Spawns a cleanup task on drop to handle abrupt disconnects.
struct SessionCleanupGuard {
    session_id: SessionId,
    reactor: Arc<Reactor>,
    state: Arc<SseState>,
    dropped: bool,
}

impl SessionCleanupGuard {
    fn new(session_id: SessionId, reactor: Arc<Reactor>, state: Arc<SseState>) -> Self {
        Self {
            session_id,
            reactor,
            state,
            dropped: false,
        }
    }

    /// Mark as cleanly closed (cleanup will be skipped).
    fn mark_closed(&mut self) {
        self.dropped = true;
    }
}

impl Drop for SessionCleanupGuard {
    fn drop(&mut self) {
        if self.dropped {
            return;
        }
        let session_id = self.session_id;
        let reactor = self.reactor.clone();
        let state = self.state.clone();

        crate::observability::set_active_connections("sse", -1);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                reactor.remove_session(session_id).await;
                state.remove_session(session_id);
                tracing::debug!(%session_id, "SSE session cleaned up on disconnect");
            });
        } else {
            tracing::warn!(%session_id, "Could not spawn cleanup task, runtime unavailable");
        }
    }
}

/// SSE event payload sent to clients. Discriminated by `"type"` (snake_case).
///
/// `#[non_exhaustive]` so 1.0.x can add new variants without breaking Rust
/// matchers. JSON consumers should already ignore unknown `type` values per
/// the Forge wire-format contract.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SsePayload {
    /// Subscription data update.
    Update {
        target: String,
        payload: serde_json::Value,
    },
    /// Error message.
    Error {
        target: String,
        code: String,
        message: String,
    },
    /// Connection acknowledged.
    Connected {
        session_id: String,
        session_secret: String,
    },
}

/// Internal message type for SSE stream.
#[derive(Debug)]
pub enum SseMessage {
    Data {
        target: String,
        payload: serde_json::Value,
    },
    Error {
        target: String,
        code: String,
        message: String,
    },
}

/// SSE subscribe request.
#[derive(Debug, Deserialize)]
pub struct SseSubscribeRequest {
    pub session_id: String,
    pub session_secret: String,
    pub id: String,
    pub function: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// SSE unsubscribe request.
#[derive(Debug, Deserialize)]
pub struct SseUnsubscribeRequest {
    pub session_id: String,
    pub session_secret: String,
    pub id: String,
}

/// SSE job subscribe request.
#[derive(Debug, Deserialize)]
pub struct SseJobSubscribeRequest {
    pub session_id: String,
    pub session_secret: String,
    pub id: String,
    pub job_id: String,
}

/// SSE workflow subscribe request.
#[derive(Debug, Deserialize)]
pub struct SseWorkflowSubscribeRequest {
    pub session_id: String,
    pub session_secret: String,
    pub id: String,
    pub workflow_id: String,
}

/// SSE error response. Wire shape mirrors `RpcError` so clients can parse
/// failures from `/_api/events`, `/_api/subscribe`, and `/_api/rpc/*` with
/// the same shape:
///
/// ```text
/// { "code": "...", "message": "...", "retry_after_secs"?: u64, "details"?: any }
/// ```
///
/// `#[non_exhaustive]` so additional fields (e.g. `quota`) can be added
/// without a breaking change.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct SseError {
    pub code: String,
    pub message: String,
    /// Seconds to wait before retrying. Set on rate-limit and at-capacity responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// Additional structured context (e.g. limit values, quota names).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl SseError {
    /// Create a basic error with just code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retry_after_secs: None,
            details: None,
        }
    }

    /// Attach a retry hint (seconds).
    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }
}

/// SSE subscribe response.
#[derive(Debug, Serialize)]
pub struct SseSubscribeResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SseError>,
}

/// SSE unsubscribe response.
#[derive(Debug, Serialize)]
pub struct SseUnsubscribeResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SseError>,
}

/// Create an SSE subscribe error response.
fn subscribe_error(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> (StatusCode, Json<SseSubscribeResponse>) {
    (
        status,
        Json(SseSubscribeResponse {
            success: false,
            data: None,
            error: Some(SseError::new(code, message)),
        }),
    )
}

/// Create an SSE unsubscribe error response.
fn unsubscribe_error(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> (StatusCode, Json<SseUnsubscribeResponse>) {
    (
        status,
        Json(SseUnsubscribeResponse {
            success: false,
            error: Some(SseError::new(code, message)),
        }),
    )
}

/// SSE handler for GET /events.
pub async fn sse_handler(
    State(state): State<Arc<SseState>>,
    Extension(request_auth): Extension<AuthContext>,
    Extension(resolved_ip): Extension<super::ResolvedClientIp>,
    Query(query): Query<SseQuery>,
) -> impl IntoResponse {
    // Check session limit. Body matches the standard SseError shape so
    // clients can parse capacity rejections the same way as rate limits.
    if !state.can_accept_session() {
        let body = SseError::new("SSE_AT_CAPACITY", "Server at maximum SSE session capacity")
            .with_retry_after(SSE_AT_CAPACITY_RETRY_SECS);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(
                axum::http::header::RETRY_AFTER,
                SSE_AT_CAPACITY_RETRY_SECS_STR,
            )],
            Json(body),
        )
            .into_response();
    }

    let session_id = SessionId::new();
    let buffer_size = state.config.channel_buffer_size;
    let keepalive_secs = state.config.keepalive_interval_secs;
    let cancel_token = CancellationToken::new();

    let client_ip = resolved_ip.0;

    // Authentication resolution order:
    //   1. If the request was authenticated by the `Authorization` header
    //      (auth_middleware ran upstream), use that. Header is authoritative.
    //   2. Otherwise, if a `?ticket=` is supplied, consume it. The ticket
    //      was minted against a validated bearer header at `/events/ticket`
    //      and is bound to the resolved client IP, so a leaked URL cannot
    //      be replayed from a different origin.
    //   3. Otherwise, treat the connection as anonymous.
    //
    // JWTs are deliberately never accepted in the URL. Query strings appear
    // in access logs, browser history, referrer headers, and proxy caches.
    let auth_context = if request_auth.is_authenticated() {
        request_auth.clone()
    } else if let Some(ticket) = &query.ticket {
        match state.tickets.consume(ticket) {
            Some(entry) => {
                // Bind ticket to the IP that requested it. Reject if either
                // side has no resolved IP (strict) or they disagree. The
                // server-side store already guarantees one-shot use.
                let ip_match = match (&entry.client_ip, &client_ip) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                if !ip_match {
                    tracing::warn!("SSE ticket IP mismatch; rejecting");
                    return super::response::RpcResponse::error(
                        super::response::RpcError::unauthorized("SSE ticket IP mismatch"),
                    )
                    .into_response();
                }
                entry.auth
            }
            None => {
                tracing::warn!("SSE ticket missing, expired, or already consumed");
                return super::response::RpcResponse::error(
                    super::response::RpcError::unauthorized("Invalid or expired SSE ticket"),
                )
                .into_response();
            }
        }
    } else {
        request_auth.clone()
    };
    // UUIDv4 provides 122 bits of randomness, sufficient for session secret entropy
    let session_secret = uuid::Uuid::new_v4().to_string();
    // Authenticated sessions without an explicit exp claim get a default
    // expiry of 1 hour to prevent indefinite streaming on malformed tokens.
    let token_exp = if auth_context.is_authenticated() {
        Some(
            auth_context
                .token_exp()
                .unwrap_or_else(|| chrono::Utc::now().timestamp() + 3600),
        )
    } else {
        None
    };

    // Check per-user and per-IP session limits via atomic counters (O(1)).
    if let Some(user_id) = auth_context.user_id()
        && state.user_session_count(user_id) >= state.config.max_sessions_per_user
    {
        let body = SseError::new(
            "TOO_MANY_SESSIONS",
            format!(
                "User has reached the maximum of {} concurrent sessions",
                state.config.max_sessions_per_user
            ),
        )
        .with_retry_after(SSE_AT_CAPACITY_RETRY_SECS);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                SSE_AT_CAPACITY_RETRY_SECS_STR,
            )],
            Json(body),
        )
            .into_response();
    }

    if let Some(ip) = &client_ip
        && state.ip_session_count(ip) >= state.config.max_sessions_per_ip
    {
        let body = SseError::new(
            "TOO_MANY_SESSIONS",
            format!(
                "IP has reached the maximum of {} concurrent sessions",
                state.config.max_sessions_per_ip
            ),
        )
        .with_retry_after(SSE_AT_CAPACITY_RETRY_SECS);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                SSE_AT_CAPACITY_RETRY_SECS_STR,
            )],
            Json(body),
        )
            .into_response();
    }

    if let Some(user_id) = auth_context.user_id() {
        state.increment_user_sessions(user_id);
    }
    if let Some(ip) = &client_ip {
        state.increment_ip_sessions(ip);
    }
    state.sessions.insert(
        session_id,
        SseSessionData {
            auth_context: auth_context.clone(),
            session_secret: session_secret.clone(),
            client_ip,
            subscriptions: HashMap::new(),
        },
    );

    let reactor = state.reactor.clone();
    let cancel = cancel_token.clone();
    let (rt_tx, mut rt_rx) = mpsc::channel(buffer_size);
    reactor.register_session(session_id, rt_tx, token_exp);

    let state_for_cleanup = Arc::new((*state).clone());
    let cleanup_guard =
        SessionCleanupGuard::new(session_id, reactor.clone(), state_for_cleanup.clone());
    crate::observability::set_active_connections("sse", 1);

    // Single task: bridge reactor messages directly to SSE events.
    // Token expiry is enforced by SessionServer::try_send_to_session (per-push)
    // and cleanup_expired_tokens (periodic sweep). No inline check needed here.
    let (event_tx, event_rx) = mpsc::channel::<Result<Event, Infallible>>(buffer_size);

    tokio::spawn(async move {
        let mut _guard = cleanup_guard;

        let connected = SsePayload::Connected {
            session_id: session_id.to_string(),
            session_secret: session_secret.clone(),
        };
        match serde_json::to_string(&connected) {
            Ok(json) => {
                let _ = event_tx
                    .send(Ok(Event::default().event("connected").data(json)))
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to serialize SSE connected payload: {}", e);
            }
        }

        loop {
            tokio::select! {
                msg = rt_rx.recv() => {
                    match msg {
                        Some(rt_msg) => {
                            let event = convert_realtime_to_sse(rt_msg).and_then(|sse_msg| {
                                match sse_msg {
                                    SseMessage::Data { target, payload } => {
                                        let data = SsePayload::Update { target, payload };
                                        serde_json::to_string(&data).ok().map(|json| {
                                            Event::default().event("update").data(json)
                                        })
                                    }
                                    SseMessage::Error { target, code, message } => {
                                        let data = SsePayload::Error { target, code, message };
                                        serde_json::to_string(&data).ok().map(|json| {
                                            Event::default().event("error").data(json)
                                        })
                                    }
                                }
                            });
                            if let Some(evt) = event {
                                match tokio::time::timeout(
                                    Duration::from_secs(5),
                                    event_tx.send(Ok(evt)),
                                ).await {
                                    Ok(Err(_)) | Err(_) => break,
                                    Ok(Ok(())) => {}
                                }
                            }
                        }
                        None => break,
                    }
                }
                _ = cancel.cancelled() => break,
            }
        }

        _guard.mark_closed();
        crate::observability::set_active_connections("sse", -1);
        reactor.remove_session(session_id).await;
        state_for_cleanup.remove_session(session_id);
    });

    let stream = ReceiverStream { rx: event_rx };

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(keepalive_secs))
                .text("ping"),
        )
        .into_response()
}

/// Response body for `POST /events/ticket`.
#[derive(Debug, Serialize)]
pub struct SseTicketResponse {
    /// Opaque single-use ticket. Send back as `?ticket=…` on the next
    /// `GET /events` request.
    pub ticket: String,
    /// Lifetime hint in seconds; clients should connect well before this.
    pub expires_in_secs: u64,
}

/// SSE ticket handler for POST `/events/ticket`. Issues a short-lived,
/// IP-bound, single-use ticket so browsers (whose `EventSource` cannot
/// set custom headers) can authenticate the SSE upgrade without putting
/// a long-lived JWT in the URL.
///
/// Requires an authenticated bearer header. Anonymous callers get 401:
/// anonymous SSE streams can simply connect to `/events` without a ticket.
pub async fn sse_ticket_handler(
    State(state): State<Arc<SseState>>,
    Extension(request_auth): Extension<AuthContext>,
    Extension(resolved_ip): Extension<super::ResolvedClientIp>,
) -> impl IntoResponse {
    if !request_auth.is_authenticated() {
        return super::response::RpcResponse::error(super::response::RpcError::unauthorized(
            "Authentication required to mint an SSE ticket",
        ))
        .into_response();
    }

    let ticket = uuid::Uuid::new_v4().to_string();
    let entry = TicketEntry {
        auth: request_auth.clone(),
        client_ip: resolved_ip.0,
        expires_at: Instant::now() + Duration::from_secs(SSE_TICKET_TTL_SECS),
    };

    if !state.tickets.insert(ticket.clone(), entry) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, SSE_TICKET_TTL_SECS_STR)],
            Json(
                SseError::new("SSE_TICKET_CAPACITY", "Too many outstanding SSE tickets")
                    .with_retry_after(SSE_TICKET_TTL_SECS),
            ),
        )
            .into_response();
    }

    Json(SseTicketResponse {
        ticket,
        expires_in_secs: SSE_TICKET_TTL_SECS,
    })
    .into_response()
}

/// Convert realtime message to SSE message.
fn convert_realtime_to_sse(msg: RealtimeMessage) -> Option<SseMessage> {
    match msg {
        RealtimeMessage::Data {
            subscription_id,
            data,
        } => Some(SseMessage::Data {
            target: format!("sub:{}", subscription_id),
            payload: std::sync::Arc::try_unwrap(data)
                .unwrap_or_else(|arc| serde_json::Value::clone(&arc)),
        }),
        RealtimeMessage::DeltaUpdate {
            subscription_id,
            delta,
        } => match serde_json::to_value(&delta) {
            Ok(payload) => Some(SseMessage::Data {
                target: format!("sub:{}", subscription_id),
                payload,
            }),
            Err(e) => {
                tracing::error!("Failed to serialize delta update: {}", e);
                Some(SseMessage::Error {
                    target: format!("sub:{}", subscription_id),
                    code: "SERIALIZATION_ERROR".to_string(),
                    message: "Failed to serialize update data".to_string(),
                })
            }
        },
        RealtimeMessage::JobUpdate { client_sub_id, job } => match serde_json::to_value(&job) {
            Ok(payload) => Some(SseMessage::Data {
                target: format!("job:{}", client_sub_id),
                payload,
            }),
            Err(e) => {
                tracing::error!("Failed to serialize job update: {}", e);
                Some(SseMessage::Error {
                    target: format!("job:{}", client_sub_id),
                    code: "SERIALIZATION_ERROR".to_string(),
                    message: "Failed to serialize job update".to_string(),
                })
            }
        },
        RealtimeMessage::WorkflowUpdate {
            client_sub_id,
            workflow,
        } => match serde_json::to_value(&workflow) {
            Ok(payload) => Some(SseMessage::Data {
                target: format!("wf:{}", client_sub_id),
                payload,
            }),
            Err(e) => {
                tracing::error!("Failed to serialize workflow update: {}", e);
                Some(SseMessage::Error {
                    target: format!("wf:{}", client_sub_id),
                    code: "SERIALIZATION_ERROR".to_string(),
                    message: "Failed to serialize workflow update".to_string(),
                })
            }
        },
        // Lagging is an internal signal to slow clients; no client-visible event.
        RealtimeMessage::Lagging => None,
        RealtimeMessage::AuthFailed { reason } => Some(SseMessage::Error {
            target: "session".to_string(),
            code: "SESSION_EXPIRED".to_string(),
            message: reason,
        }),
    }
}

/// SSE subscribe handler for POST /subscribe.
pub async fn sse_subscribe_handler(
    State(state): State<Arc<SseState>>,
    Extension(request_auth): Extension<AuthContext>,
    Json(request): Json<SseSubscribeRequest>,
) -> impl IntoResponse {
    // Validate subscription ID length to prevent memory bloat
    if let Err(resp) = validate_client_sub_id(&request.id) {
        return resp;
    }

    let Some(session_id) = try_parse_session_id(&request.session_id) else {
        return subscribe_error(
            StatusCode::BAD_REQUEST,
            "INVALID_SESSION",
            "Invalid session ID format",
        );
    };

    let session_data = match state.sessions.get(&session_id) {
        Some(data) => {
            if data.subscriptions.len() >= state.config.max_subscriptions_per_session {
                return subscribe_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "TOO_MANY_SUBSCRIPTIONS",
                    format!(
                        "Session has reached the maximum of {} subscriptions",
                        state.config.max_subscriptions_per_session
                    ),
                );
            }
            if let Some(user_id) = data.auth_context.user_id()
                && state.user_subscription_count(user_id) >= state.config.max_subscriptions_per_user
            {
                return subscribe_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "TOO_MANY_SUBSCRIPTIONS",
                    format!(
                        "User has reached the maximum of {} total subscriptions",
                        state.config.max_subscriptions_per_user
                    ),
                );
            }
            match authorize_session_access(&data, &request.session_secret, &request_auth) {
                Ok(auth) => auth,
                Err(resp) => return resp,
            }
        }
        None => {
            return subscribe_error(
                StatusCode::NOT_FOUND,
                "SESSION_NOT_FOUND",
                "Session not found or expired",
            );
        }
    };

    let result = state
        .reactor
        .subscribe(
            session_id,
            request.id.clone(),
            request.function,
            request.args,
            session_data,
        )
        .await;

    match result {
        Ok((subscription_id, data)) => {
            // Store the subscription mapping. Re-check the per-session limit
            // as a concurrent subscribe may have raced past the pre-call check.
            match state.sessions.get_mut(&session_id) {
                Some(mut session) => {
                    if session.subscriptions.len() >= state.config.max_subscriptions_per_session {
                        drop(session);
                        state.reactor.unsubscribe(subscription_id);
                        return subscribe_error(
                            StatusCode::TOO_MANY_REQUESTS,
                            "TOO_MANY_SUBSCRIPTIONS",
                            format!(
                                "Session has reached the maximum of {} subscriptions",
                                state.config.max_subscriptions_per_session
                            ),
                        );
                    }
                    session.subscriptions.insert(request.id, subscription_id);
                    if let Some(user_id) = session.auth_context.user_id() {
                        state.increment_user_subscriptions(user_id);
                    }
                }
                None => {
                    state.reactor.unsubscribe(subscription_id);
                    return subscribe_error(
                        StatusCode::NOT_FOUND,
                        "SESSION_NOT_FOUND",
                        "Session expired during subscription",
                    );
                }
            }

            tracing::debug!(
                %session_id,
                %subscription_id,
                "SSE subscription registered"
            );

            (
                StatusCode::OK,
                Json(SseSubscribeResponse {
                    success: true,
                    data: Some(data),
                    error: None,
                }),
            )
        }
        Err(e) => {
            tracing::warn!(%session_id, error = %e, "SSE subscription failed");
            match e {
                forge_core::ForgeError::Unauthorized(msg) => {
                    subscribe_error(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg)
                }
                forge_core::ForgeError::Forbidden(msg) => {
                    subscribe_error(StatusCode::FORBIDDEN, "FORBIDDEN", msg)
                }
                forge_core::ForgeError::InvalidArgument(msg)
                | forge_core::ForgeError::Validation(msg) => {
                    subscribe_error(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", msg)
                }
                forge_core::ForgeError::NotFound(msg) => {
                    subscribe_error(StatusCode::NOT_FOUND, "NOT_FOUND", msg)
                }
                _ => subscribe_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "SUBSCRIPTION_FAILED",
                    "Subscription failed",
                ),
            }
        }
    }
}

/// SSE unsubscribe handler for POST /unsubscribe.
pub async fn sse_unsubscribe_handler(
    State(state): State<Arc<SseState>>,
    Extension(request_auth): Extension<AuthContext>,
    Json(request): Json<SseUnsubscribeRequest>,
) -> impl IntoResponse {
    let Some(session_id) = try_parse_session_id(&request.session_id) else {
        return unsubscribe_error(
            StatusCode::BAD_REQUEST,
            "INVALID_SESSION",
            "Invalid session ID format",
        );
    };

    let (subscription_id, user_id) = {
        match state.sessions.get(&session_id) {
            Some(session) => {
                let secret_match: bool = session
                    .session_secret
                    .as_bytes()
                    .ct_eq(request.session_secret.as_bytes())
                    .into();
                if !secret_match || !same_principal(&session.auth_context, &request_auth) {
                    return unsubscribe_error(
                        StatusCode::FORBIDDEN,
                        "SESSION_PRINCIPAL_MISMATCH",
                        "Request principal does not match session principal",
                    );
                }
                (
                    session.subscriptions.get(&request.id).copied(),
                    session.auth_context.user_id(),
                )
            }
            None => (None, None),
        }
    };

    let Some(subscription_id) = subscription_id else {
        return unsubscribe_error(
            StatusCode::NOT_FOUND,
            "SUBSCRIPTION_NOT_FOUND",
            "Subscription not found",
        );
    };

    state.reactor.unsubscribe(subscription_id);

    if let Some(mut session) = state.sessions.get_mut(&session_id) {
        session.subscriptions.remove(&request.id);
    }
    if let Some(uid) = user_id {
        state.decrement_user_subscriptions(uid, 1);
    }

    tracing::debug!(
        %session_id,
        %subscription_id,
        "SSE subscription removed"
    );

    (
        StatusCode::OK,
        Json(SseUnsubscribeResponse {
            success: true,
            error: None,
        }),
    )
}

/// SSE job subscribe handler for POST /subscribe-job.
pub async fn sse_job_subscribe_handler(
    State(state): State<Arc<SseState>>,
    Extension(request_auth): Extension<AuthContext>,
    Json(request): Json<SseJobSubscribeRequest>,
) -> impl IntoResponse {
    if let Err(resp) = validate_client_sub_id(&request.id) {
        return resp;
    }

    let (session_id, session_auth) = match validate_session(
        &state,
        &request.session_id,
        &request.session_secret,
        &request_auth,
    )
    .await
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let job_uuid = match uuid::Uuid::parse_str(&request.job_id) {
        Ok(uuid) => uuid,
        Err(_) => {
            return subscribe_error(
                StatusCode::BAD_REQUEST,
                "INVALID_JOB_ID",
                "Invalid job ID format",
            );
        }
    };

    match state
        .reactor
        .subscribe_job(session_id, request.id.clone(), job_uuid, &session_auth)
        .await
    {
        Ok(job_data) => {
            let data = match serde_json::to_value(&job_data) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("Failed to serialize job data: {}", e);
                    return subscribe_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "SERIALIZE_ERROR",
                        "Failed to serialize job data",
                    );
                }
            };
            tracing::debug!(
                %session_id,
                job_id = %request.job_id,
                client_sub_id = %request.id,
                "SSE job subscription registered"
            );
            (
                StatusCode::OK,
                Json(SseSubscribeResponse {
                    success: true,
                    data: Some(data),
                    error: None,
                }),
            )
        }
        Err(e) => match e {
            forge_core::ForgeError::Unauthorized(msg) => {
                subscribe_error(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg)
            }
            forge_core::ForgeError::Forbidden(msg) => {
                subscribe_error(StatusCode::FORBIDDEN, "FORBIDDEN", msg)
            }
            forge_core::ForgeError::NotFound(msg) => {
                subscribe_error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", msg)
            }
            _ => subscribe_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SUBSCRIPTION_FAILED",
                "Subscription failed",
            ),
        },
    }
}

/// SSE workflow subscribe handler for POST /subscribe-workflow.
pub async fn sse_workflow_subscribe_handler(
    State(state): State<Arc<SseState>>,
    Extension(request_auth): Extension<AuthContext>,
    Json(request): Json<SseWorkflowSubscribeRequest>,
) -> impl IntoResponse {
    if let Err(resp) = validate_client_sub_id(&request.id) {
        return resp;
    }

    let (session_id, session_auth) = match validate_session(
        &state,
        &request.session_id,
        &request.session_secret,
        &request_auth,
    )
    .await
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let workflow_uuid = match uuid::Uuid::parse_str(&request.workflow_id) {
        Ok(uuid) => uuid,
        Err(_) => {
            return subscribe_error(
                StatusCode::BAD_REQUEST,
                "INVALID_WORKFLOW_ID",
                "Invalid workflow ID format",
            );
        }
    };

    match state
        .reactor
        .subscribe_workflow(session_id, request.id.clone(), workflow_uuid, &session_auth)
        .await
    {
        Ok(workflow_data) => {
            let data = match serde_json::to_value(&workflow_data) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("Failed to serialize workflow data: {}", e);
                    return subscribe_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "SERIALIZE_ERROR",
                        "Failed to serialize workflow data",
                    );
                }
            };
            tracing::debug!(
                %session_id,
                workflow_id = %request.workflow_id,
                client_sub_id = %request.id,
                "SSE workflow subscription registered"
            );
            (
                StatusCode::OK,
                Json(SseSubscribeResponse {
                    success: true,
                    data: Some(data),
                    error: None,
                }),
            )
        }
        Err(e) => match e {
            forge_core::ForgeError::Unauthorized(msg) => {
                subscribe_error(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg)
            }
            forge_core::ForgeError::Forbidden(msg) => {
                subscribe_error(StatusCode::FORBIDDEN, "FORBIDDEN", msg)
            }
            forge_core::ForgeError::NotFound(msg) => {
                subscribe_error(StatusCode::NOT_FOUND, "WORKFLOW_NOT_FOUND", msg)
            }
            _ => subscribe_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SUBSCRIPTION_FAILED",
                "Subscription failed",
            ),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::realtime::{JobData, WorkflowData};
    use uuid::Uuid;

    #[test]
    fn test_sse_payload_serialization() {
        let payload = SsePayload::Update {
            target: "sub:123".to_string(),
            payload: serde_json::json!({"id": 1}),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"type\":\"update\""));
        assert!(json.contains("\"target\":\"sub:123\""));
    }

    #[test]
    fn test_sse_error_serialization() {
        let payload = SsePayload::Error {
            target: "sub:456".to_string(),
            code: "NOT_FOUND".to_string(),
            message: "Subscription not found".to_string(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("NOT_FOUND"));
    }

    #[test]
    fn ticket_store_consume_is_one_shot() {
        let store = TicketStore::new();
        let auth =
            AuthContext::authenticated(Uuid::new_v4(), vec!["user".to_string()], HashMap::new());
        let entry = TicketEntry {
            auth,
            client_ip: Some("1.2.3.4".into()),
            expires_at: Instant::now() + Duration::from_secs(30),
        };
        assert!(store.insert("t1".into(), entry));
        assert!(store.consume("t1").is_some());
        assert!(store.consume("t1").is_none(), "second consume must fail");
    }

    #[test]
    fn ticket_store_rejects_expired() {
        let store = TicketStore::new();
        let auth =
            AuthContext::authenticated(Uuid::new_v4(), vec!["user".to_string()], HashMap::new());
        let entry = TicketEntry {
            auth,
            client_ip: None,
            expires_at: Instant::now() - Duration::from_secs(1),
        };
        assert!(store.insert("t2".into(), entry));
        assert!(
            store.consume("t2").is_none(),
            "expired ticket must not validate"
        );
    }

    #[test]
    fn ticket_store_caps_at_max() {
        let store = TicketStore::new();
        let make_entry = || TicketEntry {
            auth: AuthContext::unauthenticated(),
            client_ip: None,
            expires_at: Instant::now() + Duration::from_secs(30),
        };
        // Fill to cap.
        for i in 0..MAX_SSE_TICKETS {
            assert!(store.insert(format!("k{i}"), make_entry()));
        }
        // One more should fail (no expired entries to sweep).
        assert!(!store.insert("overflow".into(), make_entry()));
    }

    #[test]
    fn sse_error_carries_retry_after_when_set() {
        // Standardized 429/503 body must include retry_after_secs so
        // clients can back off without parsing the Retry-After header.
        let err = SseError::new("SSE_AT_CAPACITY", "at cap").with_retry_after(5);
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"code\":\"SSE_AT_CAPACITY\""), "{json}");
        assert!(json.contains("\"retry_after_secs\":5"), "{json}");
    }

    #[test]
    fn try_parse_session_id_accepts_valid_uuid() {
        let uuid = Uuid::new_v4();
        let parsed = try_parse_session_id(&uuid.to_string()).unwrap();
        assert_eq!(parsed.as_uuid(), uuid);
    }

    #[test]
    fn try_parse_session_id_rejects_garbage() {
        assert!(try_parse_session_id("not-a-uuid").is_none());
        assert!(try_parse_session_id("").is_none());
        assert!(try_parse_session_id("12345").is_none());
    }

    #[test]
    fn same_principal_two_anonymous_match() {
        let a = AuthContext::unauthenticated();
        let b = AuthContext::unauthenticated();
        assert!(same_principal(&a, &b));
    }

    #[test]
    fn same_principal_anonymous_vs_authenticated_does_not_match() {
        let anon = AuthContext::unauthenticated();
        let auth = AuthContext::authenticated(Uuid::new_v4(), vec![], HashMap::new());
        assert!(!same_principal(&anon, &auth));
        assert!(!same_principal(&auth, &anon));
    }

    #[test]
    fn same_principal_same_uuid_matches() {
        let id = Uuid::new_v4();
        let a = AuthContext::authenticated(id, vec!["user".into()], HashMap::new());
        let b = AuthContext::authenticated(id, vec!["admin".into()], HashMap::new());
        // Roles differ but principal is the same.
        assert!(same_principal(&a, &b));
    }

    #[test]
    fn same_principal_different_uuids_do_not_match() {
        let a = AuthContext::authenticated(Uuid::new_v4(), vec![], HashMap::new());
        let b = AuthContext::authenticated(Uuid::new_v4(), vec![], HashMap::new());
        assert!(!same_principal(&a, &b));
    }

    #[test]
    fn same_principal_authenticated_without_uuid_never_matches() {
        // Authenticated-without-uuid (e.g. external IDP sub) cannot prove sameness
        // through `principal_id`, so guard against false matches.
        let a = AuthContext::authenticated_without_uuid(vec!["user".into()], HashMap::new());
        let b = AuthContext::authenticated_without_uuid(vec!["user".into()], HashMap::new());
        assert!(!same_principal(&a, &b));
    }

    #[test]
    fn validate_client_sub_id_accepts_short_id() {
        assert!(validate_client_sub_id("abc-123").is_ok());
    }

    #[test]
    fn validate_client_sub_id_accepts_max_length() {
        let id = "x".repeat(MAX_CLIENT_SUB_ID_LEN);
        assert!(validate_client_sub_id(&id).is_ok());
    }

    #[test]
    fn validate_client_sub_id_rejects_oversize() {
        let id = "x".repeat(MAX_CLIENT_SUB_ID_LEN + 1);
        let err = validate_client_sub_id(&id).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn convert_realtime_to_sse_lagging_is_swallowed() {
        // Lagging is an internal eviction precursor; clients must not see it.
        assert!(convert_realtime_to_sse(RealtimeMessage::Lagging).is_none());
    }

    #[test]
    fn convert_realtime_to_sse_data_uses_sub_prefix() {
        let msg = RealtimeMessage::Data {
            subscription_id: "abc".into(),
            data: std::sync::Arc::new(serde_json::json!({"x": 1})),
        };
        let Some(SseMessage::Data { target, payload }) = convert_realtime_to_sse(msg) else {
            panic!("expected Data");
        };
        assert_eq!(target, "sub:abc");
        assert_eq!(payload, serde_json::json!({"x": 1}));
    }

    #[test]
    fn convert_realtime_to_sse_job_uses_job_prefix() {
        let msg = RealtimeMessage::JobUpdate {
            client_sub_id: "j1".into(),
            job: JobData {
                job_id: "00000000-0000-0000-0000-000000000001".into(),
                status: "running".into(),
                progress_percent: Some(50),
                progress_message: None,
                output: None,
                error: None,
            },
        };
        let Some(SseMessage::Data { target, .. }) = convert_realtime_to_sse(msg) else {
            panic!("expected Data");
        };
        assert_eq!(target, "job:j1");
    }

    #[test]
    fn convert_realtime_to_sse_workflow_uses_wf_prefix() {
        let msg = RealtimeMessage::WorkflowUpdate {
            client_sub_id: "w1".into(),
            workflow: WorkflowData {
                workflow_id: "00000000-0000-0000-0000-000000000002".into(),
                status: "running".into(),
                current_step: None,
                waiting_for: None,
                steps: vec![],
                output: None,
                error: None,
            },
        };
        let Some(SseMessage::Data { target, .. }) = convert_realtime_to_sse(msg) else {
            panic!("expected Data");
        };
        assert_eq!(target, "wf:w1");
    }

    #[test]
    fn convert_realtime_to_sse_auth_failed_targets_session() {
        let msg = RealtimeMessage::AuthFailed {
            reason: "token expired".into(),
        };
        let Some(SseMessage::Error {
            target,
            code,
            message,
        }) = convert_realtime_to_sse(msg)
        else {
            panic!("expected Error");
        };
        assert_eq!(target, "session");
        assert_eq!(code, "SESSION_EXPIRED");
        assert_eq!(message, "token expired");
    }
}

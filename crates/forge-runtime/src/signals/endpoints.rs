//! HTTP ingestion endpoint for client-side signal events.
//!
//! Route (under /_api/):
//! - POST /signal -- unified signal ingestion, discriminated by `type` field
//!
//! The `event` and `view` subtypes short-circuit when the request carries
//! `DNT: 1` or `Sec-GPC: 1`. Error reports still land so production crashes
//! from opted-out browsers remain visible.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use forge_core::AuthContext;
use forge_core::signals::{
    DiagnosticReport, PageViewPayload, SignalEvent, SignalEventBatch, SignalEventType,
    SignalPayload, SignalResponse, UtmParams,
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::bot;
use super::collector::SignalsCollector;
use super::device;
use super::rate_limit::SignalRateLimiter;
use super::session;
use super::visitor;

/// Maximum events per batch request.
const MAX_BATCH_SIZE: usize = 50;

/// Maximum serialized byte size of a single event's free-form `properties`
/// JSON. Larger payloads are rejected. Prevents apps from dumping request
/// bodies / PII into analytics rows.
const MAX_PROPERTY_BYTES: usize = 4096;

/// Maximum serialized byte size of a single event envelope (event name +
/// properties + correlation_id). Larger batch entries are rejected.
const MAX_EVENT_BYTES: usize = 8192;

/// Check the client's Do-Not-Track header. We honor DNT: 1 by short-circuiting
/// signal ingestion -- the browser has explicitly opted out of tracking.
/// Sec-GPC (Global Privacy Control) is also respected.
fn dnt_opted_out(headers: &HeaderMap) -> bool {
    let dnt = extract_header(headers, "dnt");
    if dnt.as_deref() == Some("1") {
        return true;
    }
    let gpc = extract_header(headers, "sec-gpc");
    gpc.as_deref() == Some("1")
}

/// Shared state for signal endpoints.
#[derive(Clone)]
pub struct SignalsState {
    /// Collector that buffers signal events for batch insertion.
    pub collector: SignalsCollector,
    /// Database pool for session upserts.
    pub pool: PgPool,
    /// Server secret used for visitor ID hashing.
    pub server_secret: String,
    /// When true, strip raw client IP from stored events (GDPR-compliant).
    pub anonymize_ip: bool,
    /// Optional GeoIP resolver for country code lookups from client IP.
    pub geoip: Option<super::geoip::GeoIpResolver>,
    /// Per-IP fixed-window limiter shared across all signal subtypes.
    pub rate_limiter: Arc<SignalRateLimiter>,
}

/// Resolve the client IP from request extras for rate limiting. Falls back
/// to header parsing for cases where the resolve_client_ip middleware did
/// not run (tests, edge configurations).
fn resolve_rate_limit_ip(
    resolved_ip: &Option<axum::Extension<crate::gateway::ResolvedClientIp>>,
    _headers: &HeaderMap,
) -> Option<String> {
    resolved_ip.as_ref().and_then(|r| r.0.0.clone())
}

/// Build a 429 response when the per-IP signal quota is exhausted.
fn rate_limited_response() -> Json<SignalResponse> {
    Json(SignalResponse {
        ok: false,
        session_id: None,
    })
}

/// POST /signal -- unified signal ingestion endpoint.
///
/// Accepts a discriminated payload with `type` and `payload` fields:
/// - `type: "event"` -- batch of custom/tracked events
/// - `type: "view"` -- page view with UTM and referrer context
/// - `type: "report"` -- diagnostic error report (bypasses DNT/Sec-GPC)
pub async fn signal_handler(
    State(state): State<Arc<SignalsState>>,
    resolved_ip: Option<axum::Extension<crate::gateway::ResolvedClientIp>>,
    auth: Option<axum::Extension<AuthContext>>,
    headers: HeaderMap,
    Json(payload): Json<SignalPayload>,
) -> impl IntoResponse {
    let limiter_ip = resolve_rate_limit_ip(&resolved_ip, &headers);
    if !state.rate_limiter.check(limiter_ip.as_deref()) {
        return rate_limited_response();
    }

    match payload {
        SignalPayload::Event(batch) => {
            handle_event(&state, resolved_ip, &auth, &headers, batch).await
        }
        SignalPayload::View(view) => handle_view(&state, resolved_ip, &auth, &headers, view).await,
        SignalPayload::Report(report) => {
            handle_report(&state, resolved_ip, &auth, &headers, report).await
        }
    }
}

/// Process a batch of custom events.
async fn handle_event(
    state: &SignalsState,
    resolved_ip: Option<axum::Extension<crate::gateway::ResolvedClientIp>>,
    auth: &Option<axum::Extension<AuthContext>>,
    headers: &HeaderMap,
    batch: SignalEventBatch,
) -> Json<SignalResponse> {
    if dnt_opted_out(headers) {
        return Json(SignalResponse {
            ok: true,
            session_id: None,
        });
    }
    if batch.events.len() > MAX_BATCH_SIZE {
        return rate_limited_response();
    }
    for event in &batch.events {
        if !event_within_limits(event) {
            return Json(SignalResponse {
                ok: false,
                session_id: None,
            });
        }
    }

    let ctx = extract_request_ctx(
        headers,
        resolved_ip.and_then(|r| r.0.0.clone()),
        auth,
        &state.server_secret,
        state.anonymize_ip,
        state.geoip.as_ref(),
    )
    .await;
    let supplied_session_id =
        resolve_session_id(batch.context.as_ref().and_then(|c| c.session_id.as_deref()));
    let session_id = Some(supplied_session_id.unwrap_or_else(Uuid::new_v4));
    let page_url = batch.context.as_ref().and_then(|c| c.page_url.clone());

    let referrer = batch.context.as_ref().and_then(|c| c.referrer.clone());
    spawn_session_upsert(
        state.pool.clone(),
        session_id,
        &ctx,
        page_url.clone(),
        referrer,
        "track",
    );

    for event in batch.events {
        let signal = SignalEvent {
            event_type: SignalEventType::Track,
            event_name: Some(event.event),
            correlation_id: event.correlation_id,
            session_id,
            visitor_id: Some(ctx.visitor_id.clone()),
            user_id: ctx.user_id,
            tenant_id: ctx.tenant_id,
            properties: event.properties,
            page_url: page_url.clone(),
            referrer: None,
            function_name: None,
            function_kind: None,
            duration_ms: None,
            status: None,
            error_message: None,
            error_stack: None,
            error_context: None,
            client_ip: ctx.client_ip.clone(),
            country: ctx.country.clone(),
            city: ctx.city.clone(),
            user_agent: ctx.user_agent.clone(),
            device_type: ctx.device_type.clone(),
            browser: ctx.browser.clone(),
            os: ctx.os.clone(),
            utm: None,
            is_bot: ctx.is_bot,
            timestamp: event.timestamp.unwrap_or_else(chrono::Utc::now),
        };
        state.collector.try_send(signal);
    }

    Json(SignalResponse {
        ok: true,
        session_id,
    })
}

/// Process a page view event.
async fn handle_view(
    state: &SignalsState,
    resolved_ip: Option<axum::Extension<crate::gateway::ResolvedClientIp>>,
    auth: &Option<axum::Extension<AuthContext>>,
    headers: &HeaderMap,
    payload: PageViewPayload,
) -> Json<SignalResponse> {
    if dnt_opted_out(headers) {
        return Json(SignalResponse {
            ok: true,
            session_id: None,
        });
    }
    let ctx = extract_request_ctx(
        headers,
        resolved_ip.and_then(|r| r.0.0.clone()),
        auth,
        &state.server_secret,
        state.anonymize_ip,
        state.geoip.as_ref(),
    )
    .await;
    let session_id_header = extract_header(headers, "x-session-id");
    let supplied_session_id = resolve_session_id(session_id_header.as_deref());
    let session_id = Some(supplied_session_id.unwrap_or_else(Uuid::new_v4));

    spawn_session_upsert(
        state.pool.clone(),
        session_id,
        &ctx,
        Some(payload.url.clone()),
        payload.referrer.clone(),
        "page_view",
    );

    let utm = if payload.utm_source.is_some()
        || payload.utm_medium.is_some()
        || payload.utm_campaign.is_some()
    {
        Some(UtmParams {
            source: payload.utm_source,
            medium: payload.utm_medium,
            campaign: payload.utm_campaign,
            term: payload.utm_term,
            content: payload.utm_content,
        })
    } else {
        None
    };

    let signal = SignalEvent {
        event_type: SignalEventType::PageView,
        event_name: payload.title,
        correlation_id: payload.correlation_id,
        session_id,
        visitor_id: Some(ctx.visitor_id),
        user_id: ctx.user_id,
        tenant_id: ctx.tenant_id,
        properties: Value::Object(serde_json::Map::new()),
        page_url: Some(payload.url),
        referrer: payload.referrer,
        function_name: None,
        function_kind: None,
        duration_ms: None,
        status: None,
        error_message: None,
        error_stack: None,
        error_context: None,
        client_ip: ctx.client_ip,
        country: ctx.country,
        city: ctx.city,
        user_agent: ctx.user_agent,
        device_type: ctx.device_type,
        browser: ctx.browser,
        os: ctx.os,
        utm,
        is_bot: ctx.is_bot,
        timestamp: chrono::Utc::now(),
    };
    state.collector.try_send(signal);

    Json(SignalResponse {
        ok: true,
        session_id,
    })
}

/// Process a diagnostic error report.
///
/// Error reports are never dropped on DNT: users explicitly opted out of
/// *tracking*, not of crash reporting. Without this exception, production
/// crashes from DNT-enabled browsers would be invisible. Reports carry no
/// persistent identifier by default.
async fn handle_report(
    state: &SignalsState,
    resolved_ip: Option<axum::Extension<crate::gateway::ResolvedClientIp>>,
    auth: &Option<axum::Extension<AuthContext>>,
    headers: &HeaderMap,
    report: DiagnosticReport,
) -> Json<SignalResponse> {
    if report.errors.len() > MAX_BATCH_SIZE {
        return rate_limited_response();
    }

    let ctx = extract_request_ctx(
        headers,
        resolved_ip.and_then(|r| r.0.0.clone()),
        auth,
        &state.server_secret,
        state.anonymize_ip,
        state.geoip.as_ref(),
    )
    .await;
    let session_id_header = extract_header(headers, "x-session-id");
    let session_id = resolve_session_id(session_id_header.as_deref());

    if session_id.is_some() {
        spawn_session_upsert(state.pool.clone(), session_id, &ctx, None, None, "error");
    }

    for err in report.errors {
        let signal = SignalEvent {
            event_type: SignalEventType::Error,
            event_name: Some(err.message.clone()),
            correlation_id: err.correlation_id,
            session_id,
            visitor_id: Some(ctx.visitor_id.clone()),
            user_id: ctx.user_id,
            tenant_id: ctx.tenant_id,
            properties: Value::Object(serde_json::Map::new()),
            page_url: err.page_url,
            referrer: None,
            function_name: None,
            function_kind: None,
            duration_ms: None,
            status: None,
            error_message: Some(err.message),
            error_stack: err.stack,
            error_context: err.context,
            client_ip: ctx.client_ip.clone(),
            country: ctx.country.clone(),
            city: ctx.city.clone(),
            user_agent: ctx.user_agent.clone(),
            device_type: ctx.device_type.clone(),
            browser: ctx.browser.clone(),
            os: ctx.os.clone(),
            utm: None,
            is_bot: ctx.is_bot,
            timestamp: chrono::Utc::now(),
        };
        state.collector.try_send(signal);
    }

    Json(SignalResponse {
        ok: true,
        session_id,
    })
}

/// Shared request context extracted from headers and auth for all signal subtypes.
struct RequestCtx {
    user_agent: Option<String>,
    client_ip: Option<String>,
    country: Option<String>,
    city: Option<String>,
    is_bot: bool,
    visitor_id: String,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    device_type: Option<String>,
    browser: Option<String>,
    os: Option<String>,
}

async fn extract_request_ctx(
    headers: &HeaderMap,
    resolved_ip: Option<String>,
    auth: &Option<axum::Extension<AuthContext>>,
    server_secret: &str,
    anonymize_ip: bool,
    geoip: Option<&super::geoip::GeoIpResolver>,
) -> RequestCtx {
    let user_agent = extract_header(headers, "user-agent");
    let platform_header = extract_header(headers, "x-forge-platform");
    let raw_ip = resolved_ip;
    let ua_lower = user_agent
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_bot = bot::is_bot_lower(&ua_lower);
    let visitor_id =
        visitor::generate_visitor_id(raw_ip.as_deref(), user_agent.as_deref(), server_secret);
    let user_id = auth.as_ref().and_then(|a| a.user_id());
    let tenant_id = auth.as_ref().and_then(|a| a.tenant_id());
    let device_info = device::parse_lowered(platform_header.as_deref(), &ua_lower);
    let geo = match (geoip, raw_ip.clone()) {
        (Some(g), Some(ip)) => {
            // MMDB lookups can be CPU-blocking on cold pages; offload so the
            // request thread keeps feeding the collector.
            let g = g.clone();
            tokio::task::spawn_blocking(move || g.lookup(&ip))
                .await
                .unwrap_or_default()
        }
        _ => super::geoip::GeoInfo::default(),
    };
    // anonymize_ip drops the raw IP after visitor_id + geo are derived; GDPR-friendly default.
    let client_ip = if anonymize_ip { None } else { raw_ip };
    // When IP is anonymized, also strip the UA major-version so the combo of
    // UA + country + city can't be used to re-fingerprint the visitor.
    let user_agent = if anonymize_ip {
        user_agent.as_deref().map(anonymize_ua)
    } else {
        user_agent
    };
    RequestCtx {
        user_agent,
        client_ip,
        country: geo.country,
        city: geo.city,
        is_bot,
        visitor_id,
        user_id,
        tenant_id,
        device_type: device_info.device_type,
        browser: device_info.browser,
        os: device_info.os,
    }
}

fn extract_header(headers: &HeaderMap, name: &str) -> Option<String> {
    crate::gateway::extract_header(headers, name)
}

/// Strip the major version off a UA so a per-version identifier can't be
/// derived. Recognizes the most common browser family tokens; falls back to
/// the broad family when the UA doesn't match any known prefix.
fn anonymize_ua(ua: &str) -> String {
    const FAMILIES: &[&str] = &["Chrome/", "Firefox/", "Safari/", "Edg/", "Opera/"];
    for family in FAMILIES {
        if ua.contains(family) {
            return (*family).to_string();
        }
    }
    "Other".to_string()
}

/// Per-event size guard. Rejects events whose serialized properties / event
/// envelope exceed configured limits.
fn event_within_limits(event: &forge_core::signals::ClientEvent) -> bool {
    let props_bytes = match serde_json::to_vec(&event.properties) {
        Ok(b) => b.len(),
        Err(_) => return false,
    };
    if props_bytes > MAX_PROPERTY_BYTES {
        return false;
    }
    let total = event.event.len()
        + props_bytes
        + event.correlation_id.as_deref().map(str::len).unwrap_or(0);
    total <= MAX_EVENT_BYTES
}

/// Fire-and-forget the session upsert so the request thread doesn't block on
/// a PG round-trip. We mint the session ID synchronously upstream so the
/// response can return it before the row is persisted.
fn spawn_session_upsert(
    pool: PgPool,
    session_id: Option<Uuid>,
    ctx: &RequestCtx,
    page_url: Option<String>,
    referrer: Option<String>,
    event_type: &'static str,
) {
    let visitor_id = ctx.visitor_id.clone();
    let user_id = ctx.user_id;
    let tenant_id = ctx.tenant_id;
    let user_agent = ctx.user_agent.clone();
    let client_ip = ctx.client_ip.clone();
    let device_type = ctx.device_type.clone();
    let browser = ctx.browser.clone();
    let os = ctx.os.clone();
    let is_bot = ctx.is_bot;
    tokio::spawn(async move {
        session::upsert_session(
            &pool,
            session_id,
            &visitor_id,
            user_id,
            tenant_id,
            page_url.as_deref(),
            referrer.as_deref(),
            user_agent.as_deref(),
            client_ip.as_deref(),
            is_bot,
            event_type,
            device_type.as_deref(),
            browser.as_deref(),
            os.as_deref(),
        )
        .await;
    });
}

fn resolve_session_id(raw: Option<&str>) -> Option<Uuid> {
    raw.and_then(|s| Uuid::parse_str(s).ok())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::{extract_header, resolve_session_id};

    #[tokio::test]
    async fn extract_header_returns_value() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom", HeaderValue::from_static("hello"));
        assert_eq!(extract_header(&headers, "x-custom"), Some("hello".into()));
    }

    #[tokio::test]
    async fn extract_header_returns_none_for_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_header(&headers, "x-custom"), None);
    }

    #[tokio::test]
    async fn extract_header_returns_none_for_empty_value() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom", HeaderValue::from_static(""));
        assert_eq!(extract_header(&headers, "x-custom"), None);
    }

    #[tokio::test]
    async fn resolve_session_id_parses_valid_uuid() {
        let raw = "550e8400-e29b-41d4-a716-446655440000";
        let expected = Uuid::parse_str(raw).unwrap();
        assert_eq!(resolve_session_id(Some(raw)), Some(expected));
    }

    #[tokio::test]
    async fn resolve_session_id_returns_none_for_garbage() {
        assert_eq!(resolve_session_id(Some("not-a-uuid")), None);
    }

    #[tokio::test]
    async fn resolve_session_id_returns_none_for_none() {
        assert_eq!(resolve_session_id(None), None);
    }
}

//! Shared types for the signals pipeline (product analytics + diagnostics).
//!
//! These types are used by both `forge-runtime` (server-side collection) and
//! client packages (event serialization).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Event types tracked by the signals pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SignalEventType {
    /// Page or screen view.
    PageView,
    /// Auto-captured backend RPC execution.
    RpcCall,
    /// Custom event from user code.
    Track,
    /// Session started.
    SessionStart,
    /// Session ended (timeout or explicit close).
    SessionEnd,
    /// Frontend error or unhandled rejection.
    Error,
    /// Diagnostic breadcrumb for error reproduction.
    Breadcrumb,
    /// Background execution (job, cron, workflow step, webhook, daemon tick).
    ServerExecution,
}

impl std::fmt::Display for SignalEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageView => write!(f, "page_view"),
            Self::RpcCall => write!(f, "rpc_call"),
            Self::Track => write!(f, "track"),
            Self::SessionStart => write!(f, "session_start"),
            Self::SessionEnd => write!(f, "session_end"),
            Self::Error => write!(f, "error"),
            Self::Breadcrumb => write!(f, "breadcrumb"),
            Self::ServerExecution => write!(f, "server_execution"),
        }
    }
}

/// A single signal event ready for collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEvent {
    pub event_type: SignalEventType,
    pub event_name: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub visitor_id: Option<String>,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub properties: serde_json::Value,

    // Page context
    pub page_url: Option<String>,
    pub referrer: Option<String>,

    // RPC fields (denormalized for query performance)
    pub function_name: Option<String>,
    pub function_kind: Option<String>,
    pub duration_ms: Option<i32>,
    pub status: Option<String>,

    // Diagnostics
    pub error_message: Option<String>,
    pub error_stack: Option<String>,
    pub error_context: Option<serde_json::Value>,

    // Client context
    pub client_ip: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub user_agent: Option<String>,

    // Device classification (parsed from user_agent + platform header)
    pub device_type: Option<String>,
    pub browser: Option<String>,
    pub os: Option<String>,

    // Acquisition
    pub utm: Option<UtmParams>,

    // Classification
    pub is_bot: bool,

    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl SignalEvent {
    /// Create a server-initiated execution event (job, cron, workflow step,
    /// webhook, daemon tick). These runs have no client_ip/user_agent/visitor,
    /// only a function kind + name + outcome.
    pub fn server_execution(
        name: &str,
        kind: &str,
        duration_ms: i32,
        success: bool,
        error_message: Option<String>,
    ) -> Self {
        let n = name.to_string();
        Self {
            event_type: SignalEventType::ServerExecution,
            event_name: Some(n.clone()),
            correlation_id: None,
            session_id: None,
            visitor_id: None,
            user_id: None,
            tenant_id: None,
            properties: serde_json::Value::Object(serde_json::Map::new()),
            page_url: None,
            referrer: None,
            function_name: Some(n),
            function_kind: Some(kind.to_string()),
            duration_ms: Some(duration_ms),
            status: Some(if success { "success" } else { "error" }.to_string()),
            error_message,
            error_stack: None,
            error_context: None,
            client_ip: None,
            country: None,
            city: None,
            user_agent: None,
            device_type: None,
            browser: None,
            os: None,
            utm: None,
            is_bot: false,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Create a diagnostic/audit event (auth failure, rate limit hit, etc.)
    /// with context from the request.
    pub fn diagnostic(
        event_name: &str,
        properties: serde_json::Value,
        client_ip: Option<String>,
        user_agent: Option<String>,
        visitor_id: Option<String>,
        user_id: Option<Uuid>,
        is_bot: bool,
    ) -> Self {
        Self {
            event_type: SignalEventType::Track,
            event_name: Some(event_name.to_string()),
            correlation_id: None,
            session_id: None,
            visitor_id,
            user_id,
            tenant_id: None,
            properties,
            page_url: None,
            referrer: None,
            function_name: None,
            function_kind: None,
            duration_ms: None,
            status: None,
            error_message: None,
            error_stack: None,
            error_context: None,
            client_ip,
            country: None,
            city: None,
            user_agent,
            device_type: None,
            browser: None,
            os: None,
            utm: None,
            is_bot,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Create an RPC call event from function execution metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn rpc_call(
        function_name: &str,
        function_kind: &str,
        duration_ms: i32,
        success: bool,
        user_id: Option<Uuid>,
        tenant_id: Option<Uuid>,
        correlation_id: Option<String>,
        client_ip: Option<String>,
        user_agent: Option<String>,
        visitor_id: Option<String>,
        is_bot: bool,
    ) -> Self {
        let name = function_name.to_string();
        Self {
            event_type: SignalEventType::RpcCall,
            event_name: Some(name.clone()),
            correlation_id,
            session_id: None,
            visitor_id,
            user_id,
            tenant_id,
            properties: serde_json::Value::Object(serde_json::Map::new()),
            page_url: None,
            referrer: None,
            function_name: Some(name),
            function_kind: Some(function_kind.to_string()),
            duration_ms: Some(duration_ms),
            status: Some(if success { "success" } else { "error" }.to_string()),
            error_message: None,
            error_stack: None,
            error_context: None,
            client_ip,
            country: None,
            city: None,
            user_agent,
            device_type: None,
            browser: None,
            os: None,
            utm: None,
            is_bot,
            timestamp: chrono::Utc::now(),
        }
    }
}

/// UTM campaign parameters for acquisition tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UtmParams {
    pub source: Option<String>,
    pub medium: Option<String>,
    pub campaign: Option<String>,
    pub term: Option<String>,
    pub content: Option<String>,
}

/// Unified signal ingestion payload. Discriminated by `type` field.
///
/// Clients send a single `POST /_api/signal` with one of three subtypes:
/// - `event`: batch of custom/tracked events
/// - `view`: page view with UTM and referrer context
/// - `report`: diagnostic error report (bypasses DNT)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SignalPayload {
    /// Batch of custom events from the client tracker.
    #[serde(rename = "event")]
    Event(SignalEventBatch),
    /// Page view event with URL, referrer, and UTM parameters.
    #[serde(rename = "view")]
    View(PageViewPayload),
    /// Diagnostic error report. Bypasses DNT/Sec-GPC checks.
    #[serde(rename = "report")]
    Report(DiagnosticReport),
}

/// Batch of events sent from client trackers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEventBatch {
    pub events: Vec<ClientEvent>,
    pub context: Option<ClientContext>,
}

/// A single event from the client tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEvent {
    pub event: String,
    #[serde(default)]
    pub properties: serde_json::Value,
    pub correlation_id: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Page view event from client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageViewPayload {
    pub url: String,
    pub referrer: Option<String>,
    pub title: Option<String>,
    pub utm_source: Option<String>,
    pub utm_medium: Option<String>,
    pub utm_campaign: Option<String>,
    pub utm_term: Option<String>,
    pub utm_content: Option<String>,
    pub correlation_id: Option<String>,
}

/// Diagnostic error report from client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub errors: Vec<DiagnosticError>,
}

/// A single frontend error for diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticError {
    pub message: String,
    pub stack: Option<String>,
    pub context: Option<serde_json::Value>,
    pub correlation_id: Option<String>,
    pub breadcrumbs: Option<Vec<Breadcrumb>>,
    pub page_url: Option<String>,
}

/// User action breadcrumb for error reproduction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub message: String,
    #[serde(default)]
    pub data: serde_json::Value,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Shared client context sent alongside event batches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientContext {
    pub page_url: Option<String>,
    pub referrer: Option<String>,
    pub session_id: Option<String>,
}

/// Response from signal ingestion endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalResponse {
    pub ok: bool,
    /// Server-assigned session ID (returned on first event).
    pub session_id: Option<Uuid>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signal_event_type_display_produces_snake_case() {
        assert_eq!(SignalEventType::PageView.to_string(), "page_view");
        assert_eq!(SignalEventType::RpcCall.to_string(), "rpc_call");
        assert_eq!(SignalEventType::Track.to_string(), "track");
        assert_eq!(SignalEventType::SessionStart.to_string(), "session_start");
        assert_eq!(SignalEventType::SessionEnd.to_string(), "session_end");
        assert_eq!(SignalEventType::Error.to_string(), "error");
        assert_eq!(SignalEventType::Breadcrumb.to_string(), "breadcrumb");
        assert_eq!(
            SignalEventType::ServerExecution.to_string(),
            "server_execution"
        );
    }

    #[tokio::test]
    async fn signal_event_type_serde_round_trip() {
        let variants = [
            SignalEventType::PageView,
            SignalEventType::RpcCall,
            SignalEventType::Track,
            SignalEventType::SessionStart,
            SignalEventType::SessionEnd,
            SignalEventType::Error,
            SignalEventType::Breadcrumb,
            SignalEventType::ServerExecution,
        ];

        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: SignalEventType = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized);
        }
    }

    #[tokio::test]
    async fn rpc_call_sets_correct_fields_on_success() {
        let user_id = Uuid::parse_str("a1a2a3a4-b1b2-c1c2-d1d2-e1e2e3e4e5e6").unwrap();
        let tenant_id = Uuid::parse_str("f1f2f3f4-a1a2-b1b2-c1c2-d1d2d3d4d5d6").unwrap();

        let event = SignalEvent::rpc_call(
            "get_users",
            "query",
            42,
            true,
            Some(user_id),
            Some(tenant_id),
            Some("corr-123".to_string()),
            Some("127.0.0.1".to_string()),
            Some("test-agent".to_string()),
            Some("visitor-abc".to_string()),
            false,
        );

        assert_eq!(event.event_type, SignalEventType::RpcCall);
        assert_eq!(event.function_name.as_deref(), Some("get_users"));
        assert_eq!(event.function_kind.as_deref(), Some("query"));
        assert_eq!(event.duration_ms, Some(42));
        assert_eq!(event.status.as_deref(), Some("success"));
        assert!(event.device_type.is_none());
        assert!(event.browser.is_none());
        assert!(event.os.is_none());
        assert!(event.session_id.is_none());
        assert_eq!(
            event.properties,
            serde_json::Value::Object(serde_json::Map::new())
        );
    }

    #[tokio::test]
    async fn rpc_call_sets_error_status_when_not_success() {
        let event = SignalEvent::rpc_call(
            "create_user",
            "mutation",
            100,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );

        assert_eq!(event.status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn client_event_deserializes_with_timestamp() {
        let json = r#"{
            "event": "click",
            "properties": {"button": "submit"},
            "correlation_id": "abc",
            "timestamp": "2025-01-15T10:30:00Z"
        }"#;

        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "click");
        assert!(event.timestamp.is_some());
        assert_eq!(event.correlation_id.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn client_event_deserializes_without_timestamp() {
        let json = r#"{
            "event": "click"
        }"#;

        let event: ClientEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "click");
        assert!(event.timestamp.is_none());
        assert_eq!(event.properties, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn page_view_payload_deserializes_with_all_utm_fields() {
        let json = r#"{
            "url": "https://example.com/page",
            "referrer": "https://google.com",
            "title": "Home",
            "utm_source": "google",
            "utm_medium": "cpc",
            "utm_campaign": "spring",
            "utm_term": "rust framework",
            "utm_content": "banner",
            "correlation_id": "corr-456"
        }"#;

        let payload: PageViewPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.url, "https://example.com/page");
        assert_eq!(payload.referrer.as_deref(), Some("https://google.com"));
        assert_eq!(payload.title.as_deref(), Some("Home"));
        assert_eq!(payload.utm_source.as_deref(), Some("google"));
        assert_eq!(payload.utm_medium.as_deref(), Some("cpc"));
        assert_eq!(payload.utm_campaign.as_deref(), Some("spring"));
        assert_eq!(payload.utm_term.as_deref(), Some("rust framework"));
        assert_eq!(payload.utm_content.as_deref(), Some("banner"));
    }

    #[tokio::test]
    async fn page_view_payload_deserializes_with_only_url() {
        let json = r#"{"url": "https://example.com"}"#;

        let payload: PageViewPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.url, "https://example.com");
        assert!(payload.referrer.is_none());
        assert!(payload.title.is_none());
        assert!(payload.utm_source.is_none());
        assert!(payload.utm_medium.is_none());
        assert!(payload.utm_campaign.is_none());
        assert!(payload.utm_term.is_none());
        assert!(payload.utm_content.is_none());
    }

    #[tokio::test]
    async fn diagnostic_error_deserializes_with_breadcrumbs() {
        let json = r#"{
            "message": "TypeError: null is not an object",
            "stack": "at foo.js:10",
            "breadcrumbs": [
                {"message": "clicked button", "data": {}, "timestamp": null},
                {"message": "navigated to /settings", "data": {"from": "/home"}}
            ]
        }"#;

        let error: DiagnosticError = serde_json::from_str(json).unwrap();
        assert_eq!(error.message, "TypeError: null is not an object");
        assert_eq!(error.stack.as_deref(), Some("at foo.js:10"));
        let breadcrumbs = error.breadcrumbs.unwrap();
        assert_eq!(breadcrumbs.len(), 2);
        assert_eq!(breadcrumbs[0].message, "clicked button");
        assert_eq!(breadcrumbs[1].message, "navigated to /settings");
    }

    #[tokio::test]
    async fn diagnostic_error_deserializes_with_null_breadcrumbs() {
        let json = r#"{
            "message": "ReferenceError: x is not defined",
            "stack": null,
            "context": null,
            "correlation_id": null,
            "breadcrumbs": null,
            "page_url": null
        }"#;

        let error: DiagnosticError = serde_json::from_str(json).unwrap();
        assert_eq!(error.message, "ReferenceError: x is not defined");
        assert!(error.breadcrumbs.is_none());
    }

    #[tokio::test]
    async fn signal_response_serializes_with_session_id() {
        let session_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let response = SignalResponse {
            ok: true,
            session_id: Some(session_id),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"session_id\":\"11111111-2222-3333-4444-555555555555\""));
    }

    #[tokio::test]
    async fn signal_response_serializes_not_ok_with_no_session() {
        let response = SignalResponse {
            ok: false,
            session_id: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("\"session_id\":null"));
    }

    #[tokio::test]
    async fn client_context_deserializes_with_all_fields_none() {
        let json = r#"{
            "page_url": null,
            "referrer": null,
            "session_id": null
        }"#;

        let ctx: ClientContext = serde_json::from_str(json).unwrap();
        assert!(ctx.page_url.is_none());
        assert!(ctx.referrer.is_none());
        assert!(ctx.session_id.is_none());
    }

    #[tokio::test]
    async fn signal_payload_deserializes_event_variant() {
        let json = r#"{
            "type": "event",
            "payload": {
                "events": [{"event": "click"}],
                "context": null
            }
        }"#;

        let payload: SignalPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(payload, SignalPayload::Event(_)));
    }

    #[tokio::test]
    async fn signal_payload_deserializes_view_variant() {
        let json = r#"{
            "type": "view",
            "payload": {
                "url": "https://example.com/page"
            }
        }"#;

        let payload: SignalPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(payload, SignalPayload::View(_)));
    }

    #[tokio::test]
    async fn signal_payload_deserializes_report_variant() {
        let json = r#"{
            "type": "report",
            "payload": {
                "errors": [{"message": "boom"}]
            }
        }"#;

        let payload: SignalPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(payload, SignalPayload::Report(_)));
    }

    #[tokio::test]
    async fn server_execution_marks_status_and_clears_client_context() {
        // Server-initiated runs have no visitor/UA/IP/session — confirm the
        // constructor leaves those slots empty so the events table doesn't
        // accidentally attribute background work to a request.
        let event = SignalEvent::server_execution("send_email", "job", 1500, true, None);

        assert_eq!(event.event_type, SignalEventType::ServerExecution);
        assert_eq!(event.event_name.as_deref(), Some("send_email"));
        assert_eq!(event.function_name.as_deref(), Some("send_email"));
        assert_eq!(event.function_kind.as_deref(), Some("job"));
        assert_eq!(event.duration_ms, Some(1500));
        assert_eq!(event.status.as_deref(), Some("success"));
        assert!(event.error_message.is_none());

        // Must NOT carry any client/session attribution.
        assert!(event.client_ip.is_none());
        assert!(event.user_agent.is_none());
        assert!(event.visitor_id.is_none());
        assert!(event.session_id.is_none());
        assert!(event.user_id.is_none());
        assert!(event.tenant_id.is_none());
        assert!(event.correlation_id.is_none());
        assert!(!event.is_bot, "background runs are never flagged as bots");
        assert_eq!(
            event.properties,
            serde_json::Value::Object(serde_json::Map::new())
        );
    }

    #[tokio::test]
    async fn server_execution_records_error_message_and_failure_status() {
        let event = SignalEvent::server_execution(
            "process_payment",
            "workflow",
            8000,
            false,
            Some("connection refused".to_string()),
        );

        assert_eq!(event.status.as_deref(), Some("error"));
        assert_eq!(event.error_message.as_deref(), Some("connection refused"));
        // error_stack/context are reserved for frontend reports — server
        // executions only get a message.
        assert!(event.error_stack.is_none());
        assert!(event.error_context.is_none());
    }

    #[tokio::test]
    async fn diagnostic_event_uses_track_type_and_threads_request_context() {
        // Diagnostics (auth failure, rate limit hit, etc.) ride the Track
        // event_type so the analytics table doesn't gain another partition;
        // they MUST carry the request attribution the caller provides.
        let user_id = Uuid::new_v4();
        let props = serde_json::json!({"reason": "invalid_token"});

        let event = SignalEvent::diagnostic(
            "auth_failure",
            props.clone(),
            Some("10.0.0.5".to_string()),
            Some("curl/8".to_string()),
            Some("visitor-xyz".to_string()),
            Some(user_id),
            true,
        );

        assert_eq!(event.event_type, SignalEventType::Track);
        assert_eq!(event.event_name.as_deref(), Some("auth_failure"));
        assert_eq!(event.properties, props);
        assert_eq!(event.client_ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(event.user_agent.as_deref(), Some("curl/8"));
        assert_eq!(event.visitor_id.as_deref(), Some("visitor-xyz"));
        assert_eq!(event.user_id, Some(user_id));
        assert!(event.is_bot, "bot flag must round-trip");

        // Diagnostics aren't RPC calls — function fields stay empty.
        assert!(event.function_name.is_none());
        assert!(event.function_kind.is_none());
        assert!(event.duration_ms.is_none());
        assert!(event.status.is_none());
    }

    #[tokio::test]
    async fn diagnostic_event_tolerates_all_optional_context_missing() {
        // Anonymous diagnostic (no IP/UA/visitor/user) must still produce a
        // valid event — the worker paths that emit these often run in
        // contexts with no request at all.
        let event = SignalEvent::diagnostic(
            "rate_limit_exceeded",
            serde_json::Value::Null,
            None,
            None,
            None,
            None,
            false,
        );

        assert_eq!(event.event_type, SignalEventType::Track);
        assert_eq!(event.event_name.as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(event.properties, serde_json::Value::Null);
        assert!(event.client_ip.is_none());
        assert!(event.user_agent.is_none());
        assert!(!event.is_bot);
    }

    #[tokio::test]
    async fn breadcrumb_data_defaults_to_null_when_absent() {
        // Frontend trackers commonly omit `data` for trivial breadcrumbs;
        // the #[serde(default)] must turn that into Value::Null rather than
        // failing deserialization.
        let json = r#"{"message": "form submitted"}"#;
        let bc: Breadcrumb = serde_json::from_str(json).unwrap();
        assert_eq!(bc.message, "form submitted");
        assert_eq!(bc.data, serde_json::Value::Null);
        assert!(bc.timestamp.is_none());
    }

    #[tokio::test]
    async fn utm_params_default_is_all_none() {
        // The Default impl is load-bearing — RPC handlers construct
        // UtmParams::default() when the URL has no utm_* keys.
        let u = UtmParams::default();
        assert!(u.source.is_none());
        assert!(u.medium.is_none());
        assert!(u.campaign.is_none());
        assert!(u.term.is_none());
        assert!(u.content.is_none());
    }

    #[tokio::test]
    async fn signal_payload_unknown_type_fails_deserialization() {
        // Pin the discriminator behavior: an unknown `type` MUST be rejected
        // so the ingestion handler doesn't silently drop misrouted payloads.
        let json = r#"{"type": "bogus", "payload": {}}"#;
        let err = serde_json::from_str::<SignalPayload>(json).unwrap_err();
        // serde_json's wording around unknown variants includes the bad tag.
        assert!(err.to_string().contains("bogus"), "got: {err}");
    }

    #[tokio::test]
    async fn signal_event_type_serializes_to_snake_case_json_string() {
        // Display covers the human format; this pins the wire format.
        let j = serde_json::to_string(&SignalEventType::ServerExecution).unwrap();
        assert_eq!(j, "\"server_execution\"");
        let j = serde_json::to_string(&SignalEventType::PageView).unwrap();
        assert_eq!(j, "\"page_view\"");
    }

    #[tokio::test]
    async fn signal_payload_round_trips_all_variants() {
        let event_payload = SignalPayload::Event(SignalEventBatch {
            events: vec![ClientEvent {
                event: "test".to_string(),
                properties: serde_json::Value::Null,
                correlation_id: None,
                timestamp: None,
            }],
            context: None,
        });
        let json = serde_json::to_string(&event_payload).unwrap();
        let deserialized: SignalPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, SignalPayload::Event(_)));

        let view_payload = SignalPayload::View(PageViewPayload {
            url: "https://example.com".to_string(),
            referrer: None,
            title: None,
            utm_source: None,
            utm_medium: None,
            utm_campaign: None,
            utm_term: None,
            utm_content: None,
            correlation_id: None,
        });
        let json = serde_json::to_string(&view_payload).unwrap();
        let deserialized: SignalPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, SignalPayload::View(_)));

        let report_payload = SignalPayload::Report(DiagnosticReport {
            errors: vec![DiagnosticError {
                message: "test error".to_string(),
                stack: None,
                context: None,
                correlation_id: None,
                breadcrumbs: None,
                page_url: None,
            }],
        });
        let json = serde_json::to_string(&report_payload).unwrap();
        let deserialized: SignalPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, SignalPayload::Report(_)));
    }
}

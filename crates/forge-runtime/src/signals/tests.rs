//! Integration tests for the signals pipeline.
//!
//! These tests hit a real PostgreSQL database (via testcontainers) and verify
//! the full path: session management, event collection, handler round-trips,
//! and partition management.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::IntoResponse;
use forge_core::signals::{
    ClientContext, ClientEvent, DiagnosticError, DiagnosticReport, PageViewPayload, SignalEvent,
    SignalEventBatch, SignalEventType, SignalPayload,
};
use forge_core::testing::IsolatedTestDb;
use sqlx::PgPool;
use uuid::Uuid;

use super::collector::SignalsCollector;
use super::endpoints::SignalsState;
use super::{endpoints, partition, session};

const SYSTEM_SQL: &str = include_str!("../../migrations/system/v001_initial.sql");

async fn setup(name: &str) -> IsolatedTestDb {
    let db = IsolatedTestDb::setup(name, SYSTEM_SQL, std::path::Path::new("nonexistent"))
        .await
        .unwrap();
    // Ensure partitions exist so INSERTs don't fail
    partition::ensure_partitions(db.pool()).await;
    db
}

fn make_signals_state(pool: &PgPool) -> Arc<SignalsState> {
    let collector = SignalsCollector::spawn(
        Arc::new(pool.clone()),
        1, // batch_size=1 for immediate flush
        Duration::from_millis(50),
        10_000,
    );
    Arc::new(SignalsState {
        collector,
        pool: pool.clone(),
        server_secret: "test-secret".to_string(),
        anonymize_ip: false,
        geoip: None,
        rate_limiter: Arc::new(crate::signals::rate_limit::SignalRateLimiter::new()),
    })
}

fn make_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "user-agent",
        HeaderValue::from_static("TestBrowser/1.0 (Macintosh; Intel Mac OS X) Chrome/120.0"),
    );
    headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));
    headers
}

fn make_headers_with_platform(platform: &str) -> HeaderMap {
    let mut headers = make_headers();
    headers.insert("x-forge-platform", HeaderValue::from_str(platform).unwrap());
    headers
}

// ── Session lifecycle ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_session_create_new() {
    let db = setup("session_create").await;
    let pool = db.pool();

    let sid = session::upsert_session(
        pool,
        None,
        "visitor-1",
        None,
        None,
        Some("/home"),
        None,
        Some("Chrome/120"),
        Some("1.2.3.4"),
        false,
        "page_view",
        Some("web"),
        Some("Chrome"),
        Some("macOS"),
    )
    .await;

    assert!(sid.is_some());

    let row = sqlx::query_as::<_, (i32, String, bool)>(
        "SELECT event_count, entry_page, is_bounce FROM forge_signals_sessions WHERE id = $1",
    )
    .bind(sid.unwrap())
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(row.0, 1);
    assert_eq!(row.1, "/home");
    assert!(row.2); // first page view = bounce

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_session_update_existing() {
    let db = setup("session_update").await;
    let pool = db.pool();

    let sid = session::upsert_session(
        pool,
        None,
        "visitor-2",
        None,
        None,
        Some("/page1"),
        None,
        Some("Firefox/121"),
        Some("5.6.7.8"),
        false,
        "track",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Update same session
    let sid2 = session::upsert_session(
        pool,
        Some(sid),
        "visitor-2",
        None,
        None,
        Some("/page2"),
        None,
        Some("Firefox/121"),
        Some("5.6.7.8"),
        false,
        "track",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(sid, sid2);

    let count: (i32,) =
        sqlx::query_as("SELECT event_count FROM forge_signals_sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(pool)
            .await
            .unwrap();

    assert_eq!(count.0, 2);

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_session_bounce_cleared() {
    let db = setup("session_bounce").await;
    let pool = db.pool();

    let sid = session::upsert_session(
        pool,
        None,
        "visitor-bounce",
        None,
        None,
        Some("/"),
        None,
        None,
        None,
        false,
        "page_view",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // First page view: still a bounce
    let bounce: (bool,) =
        sqlx::query_as("SELECT is_bounce FROM forge_signals_sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(bounce.0);

    // Second page view clears bounce
    session::upsert_session(
        pool,
        Some(sid),
        "visitor-bounce",
        None,
        None,
        Some("/about"),
        None,
        None,
        None,
        false,
        "page_view",
        None,
        None,
        None,
    )
    .await;

    let bounce: (bool,) =
        sqlx::query_as("SELECT is_bounce FROM forge_signals_sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(!bounce.0);

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_session_device_fields_populated() {
    let db = setup("session_device").await;
    let pool = db.pool();

    let sid = session::upsert_session(
        pool,
        None,
        "visitor-device",
        None,
        None,
        Some("/"),
        None,
        Some("Chrome/120"),
        None,
        false,
        "page_view",
        Some("desktop"),
        Some("Chrome"),
        Some("macOS"),
    )
    .await
    .unwrap();

    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        "SELECT device_type, browser, os FROM forge_signals_sessions WHERE id = $1",
    )
    .bind(sid)
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(row.0.as_deref(), Some("desktop"));
    assert_eq!(row.1.as_deref(), Some("Chrome"));
    assert_eq!(row.2.as_deref(), Some("macOS"));

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_close_stale_sessions() {
    let db = setup("close_stale").await;
    let pool = db.pool();

    let sid = session::upsert_session(
        pool,
        None,
        "visitor-stale",
        None,
        None,
        Some("/"),
        None,
        None,
        None,
        false,
        "page_view",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Backdate both started_at and last_activity so duration > 0
    sqlx::query(
        "UPDATE forge_signals_sessions SET started_at = NOW() - interval '3 hours', last_activity_at = NOW() - interval '2 hours' WHERE id = $1"
    )
    .bind(sid)
    .execute(pool)
    .await
    .unwrap();

    session::close_stale_sessions(pool, 30).await;

    let row = sqlx::query_as::<_, (Option<chrono::DateTime<chrono::Utc>>, Option<i32>)>(
        "SELECT ended_at, duration_secs FROM forge_signals_sessions WHERE id = $1",
    )
    .bind(sid)
    .fetch_one(pool)
    .await
    .unwrap();

    assert!(row.0.is_some(), "ended_at should be set");
    assert!(row.1.is_some(), "duration_secs should be set");
    assert!(row.1.unwrap() > 0, "duration should be positive");

    db.cleanup().await.unwrap();
}

// ── Collector (real DB writes) ──────────────────────────────────────────────

fn make_test_event() -> SignalEvent {
    SignalEvent {
        event_type: SignalEventType::Track,
        event_name: Some("test_event".to_string()),
        correlation_id: Some("corr-1".to_string()),
        session_id: None,
        visitor_id: Some("visitor-test".to_string()),
        user_id: None,
        tenant_id: None,
        properties: serde_json::json!({"key": "value"}),
        page_url: Some("/test".to_string()),
        referrer: None,
        function_name: None,
        function_kind: None,
        duration_ms: None,
        status: None,
        error_message: None,
        error_stack: None,
        error_context: None,
        client_ip: Some("1.2.3.4".to_string()),
        country: None,
        city: None,
        user_agent: Some("TestAgent/1.0".to_string()),
        device_type: Some("web".to_string()),
        browser: Some("Chrome".to_string()),
        os: Some("macOS".to_string()),
        utm: None,
        is_bot: false,
        timestamp: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn test_collector_single_event_flush() {
    let db = setup("collector_single").await;
    let pool = db.pool();

    let collector = SignalsCollector::spawn(
        Arc::new(pool.clone()),
        1, // flush on every event
        Duration::from_secs(60),
        10_000,
    );

    collector.try_send(make_test_event());
    tokio::time::sleep(Duration::from_millis(200)).await;

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM forge_signals_events WHERE event_name = 'test_event'")
            .fetch_one(pool)
            .await
            .unwrap();

    assert_eq!(count.0, 1);

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_collector_batch_trigger() {
    let db = setup("collector_batch").await;
    let pool = db.pool();

    let collector = SignalsCollector::spawn(
        Arc::new(pool.clone()),
        5, // flush at 5 events
        Duration::from_secs(60),
        10_000,
    );

    for i in 0..5 {
        let mut event = make_test_event();
        event.event_name = Some(format!("batch_event_{i}"));
        collector.try_send(event);
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM forge_signals_events WHERE event_name LIKE 'batch_event_%'",
    )
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(count.0, 5);

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_collector_timer_trigger() {
    let db = setup("collector_timer").await;
    let pool = db.pool();

    let collector = SignalsCollector::spawn(
        Arc::new(pool.clone()),
        100,                        // high batch size so it won't trigger
        Duration::from_millis(100), // but short timer
        10_000,
    );

    for i in 0..3 {
        let mut event = make_test_event();
        event.event_name = Some(format!("timer_event_{i}"));
        collector.try_send(event);
    }

    // Wait for timer flush
    tokio::time::sleep(Duration::from_millis(300)).await;

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM forge_signals_events WHERE event_name LIKE 'timer_event_%'",
    )
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(count.0, 3);

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_collector_drop_flushes_remaining() {
    let db = setup("collector_drop").await;
    let pool = db.pool();

    {
        let collector = SignalsCollector::spawn(
            Arc::new(pool.clone()),
            100,                     // won't trigger batch
            Duration::from_secs(60), // won't trigger timer
            10_000,
        );

        for i in 0..3 {
            let mut event = make_test_event();
            event.event_name = Some(format!("drop_event_{i}"));
            collector.try_send(event);
        }

        // Collector dropped here, channel closes, flush loop flushes remaining
    }

    // Give the flush task time to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM forge_signals_events WHERE event_name LIKE 'drop_event_%'",
    )
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(count.0, 3);

    db.cleanup().await.unwrap();
}

// ── Handler round-trips ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_event_handler_roundtrip() {
    let db = setup("handler_event").await;
    let state = make_signals_state(db.pool());

    let batch = SignalEventBatch {
        events: vec![ClientEvent {
            event: "signup_click".to_string(),
            properties: serde_json::json!({"button": "hero"}),
            correlation_id: Some("corr-abc".to_string()),
            timestamp: None,
        }],
        context: Some(ClientContext {
            page_url: Some("/landing".to_string()),
            referrer: None,
            session_id: None,
        }),
    };

    let response = endpoints::signal_handler(
        State(state.clone()),
        None,
        None,
        make_headers(),
        Json(SignalPayload::Event(batch)),
    )
    .await
    .into_response();

    let body: serde_json::Value = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap();

    assert_eq!(body["ok"], true);
    assert!(body["session_id"].is_string());
    let session_id = body["session_id"].as_str().unwrap();
    assert!(Uuid::parse_str(session_id).is_ok());

    // Wait for collector flush
    tokio::time::sleep(Duration::from_millis(200)).await;

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM forge_signals_events WHERE event_name = 'signup_click'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 1);

    // Verify session was created
    let session_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM forge_signals_sessions WHERE id = $1")
            .bind(Uuid::parse_str(session_id).unwrap())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(session_count.0, 1);

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_view_handler_with_utm() {
    let db = setup("handler_view_utm").await;
    let state = make_signals_state(db.pool());

    let payload = PageViewPayload {
        url: "/campaign".to_string(),
        referrer: Some("https://google.com".to_string()),
        title: Some("Campaign Page".to_string()),
        utm_source: Some("google".to_string()),
        utm_medium: Some("cpc".to_string()),
        utm_campaign: Some("spring-sale".to_string()),
        utm_term: Some("shoes".to_string()),
        utm_content: Some("banner".to_string()),
        correlation_id: None,
    };

    let response = endpoints::signal_handler(
        State(state.clone()),
        None,
        None,
        make_headers(),
        Json(SignalPayload::View(payload)),
    )
    .await
    .into_response();

    let body: serde_json::Value = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap();
    assert_eq!(body["ok"], true);

    tokio::time::sleep(Duration::from_millis(200)).await;

    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>, Option<String>)>(
        "SELECT utm_source, utm_medium, utm_campaign, referrer FROM forge_signals_events WHERE page_url = '/campaign'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    assert_eq!(row.0.as_deref(), Some("google"));
    assert_eq!(row.1.as_deref(), Some("cpc"));
    assert_eq!(row.2.as_deref(), Some("spring-sale"));
    assert_eq!(row.3.as_deref(), Some("https://google.com"));

    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_report_handler_stores_errors() {
    let db = setup("handler_report").await;
    let state = make_signals_state(db.pool());

    let report = DiagnosticReport {
        errors: vec![
            DiagnosticError {
                message: "TypeError: null is not an object".to_string(),
                stack: Some("at render (app.js:42)".to_string()),
                context: Some(serde_json::json!({"component": "UserList"})),
                correlation_id: Some("corr-err-1".to_string()),
                breadcrumbs: None,
                page_url: Some("/users".to_string()),
            },
            DiagnosticError {
                message: "Unhandled promise rejection".to_string(),
                stack: None,
                context: None,
                correlation_id: None,
                breadcrumbs: None,
                page_url: Some("/dashboard".to_string()),
            },
        ],
    };

    let response = endpoints::signal_handler(
        State(state.clone()),
        None,
        None,
        make_headers(),
        Json(SignalPayload::Report(report)),
    )
    .await
    .into_response();

    let body: serde_json::Value = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap();
    assert_eq!(body["ok"], true);

    tokio::time::sleep(Duration::from_millis(200)).await;

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM forge_signals_events WHERE event_type = 'error'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count.0, 2);

    // Verify first error has all fields
    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        "SELECT error_message, error_stack, correlation_id FROM forge_signals_events WHERE correlation_id = 'corr-err-1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0.as_deref(), Some("TypeError: null is not an object"));
    assert_eq!(row.1.as_deref(), Some("at render (app.js:42)"));
    assert_eq!(row.2.as_deref(), Some("corr-err-1"));

    db.cleanup().await.unwrap();
}

// ── Device fields through handler pipeline ──────────────────────────────────

#[tokio::test]
async fn test_event_handler_populates_device_fields() {
    let db = setup("handler_device").await;
    let state = make_signals_state(db.pool());

    let batch = SignalEventBatch {
        events: vec![ClientEvent {
            event: "device_test".to_string(),
            properties: serde_json::json!({}),
            correlation_id: None,
            timestamp: None,
        }],
        context: None,
    };

    endpoints::signal_handler(
        State(state.clone()),
        None,
        None,
        make_headers_with_platform("desktop-macos"),
        Json(SignalPayload::Event(batch)),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        "SELECT device_type, browser, os FROM forge_signals_events WHERE event_name = 'device_test'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    assert_eq!(row.0.as_deref(), Some("desktop"));
    assert_eq!(row.1.as_deref(), Some("Chrome"));
    assert_eq!(row.2.as_deref(), Some("macOS"));

    db.cleanup().await.unwrap();
}

// ── Partition management ────────────────────────────────────────────────────

#[tokio::test]
async fn test_partition_ensure() {
    let db = setup("partition_ensure").await;

    // Should not error even when called multiple times (idempotent)
    partition::ensure_partitions(db.pool()).await;
    partition::ensure_partitions(db.pool()).await;

    // Verify at least the current month partition exists
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pg_tables WHERE tablename LIKE 'forge_signals_events_%' AND schemaname = 'public'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    // At minimum: current month + next month + default = 3 (default created by migration)
    assert!(
        count.0 >= 2,
        "expected at least 2 partitions, got {}",
        count.0
    );

    db.cleanup().await.unwrap();
}

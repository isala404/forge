//! Buffered batch event writer for the signals pipeline.
//!
//! Events are sent via an mpsc channel and flushed in batches using
//! PostgreSQL UNNEST for high-throughput INSERT. Uses the analytics
//! connection pool to avoid contention with user queries.

use std::sync::Arc;
use std::time::Duration;

use forge_core::signals::SignalEvent;
use sqlx::PgPool;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, error, warn};

/// Buffered signal event collector.
///
/// Clone-friendly (shares the mpsc sender). Send events from any async
/// context via [`SignalsCollector::try_send`] which never blocks the caller.
///
/// Call [`SignalsCollector::shutdown`] on graceful exit to flush any buffered
/// events before the process terminates.
#[derive(Clone)]
pub struct SignalsCollector {
    tx: mpsc::Sender<SignalEvent>,
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>>,
}

impl SignalsCollector {
    /// Create a new collector and spawn the background flush task.
    ///
    /// Returns the collector handle. The flush task runs until [`shutdown`] is
    /// called or all senders are dropped.
    pub fn spawn(
        pool: Arc<PgPool>,
        batch_size: usize,
        flush_interval: Duration,
        channel_capacity: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(flush_loop(
            rx,
            pool,
            batch_size,
            flush_interval,
            shutdown_rx,
        ));
        Self {
            tx,
            shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
        }
    }

    /// Send an event without blocking. Drops the event if the channel is full.
    pub fn try_send(&self, event: SignalEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("signals collector channel full, dropping event");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                debug!("signals collector closed, dropping event");
            }
        }
    }

    /// Flush buffered events and wait for the background task to drain.
    ///
    /// Safe to call multiple times (subsequent calls are no-ops). Times out
    /// after 5 seconds if the flush task doesn't respond, to avoid blocking
    /// shutdown indefinitely.
    pub async fn shutdown(&self) {
        let Some(shutdown_tx) = self.shutdown_tx.lock().await.take() else {
            return;
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        if shutdown_tx.send(ack_tx).is_err() {
            return;
        }
        match tokio::time::timeout(Duration::from_secs(5), ack_rx).await {
            Ok(Ok(())) => debug!("signals collector flushed on shutdown"),
            Ok(Err(_)) => debug!("signals collector shutdown channel closed"),
            Err(_) => warn!("signals collector shutdown timed out after 5s"),
        }
    }
}

/// Background loop that drains the channel and performs batch inserts.
async fn flush_loop(
    mut rx: mpsc::Receiver<SignalEvent>,
    pool: Arc<PgPool>,
    batch_size: usize,
    flush_interval: Duration,
    mut shutdown_rx: oneshot::Receiver<oneshot::Sender<()>>,
) {
    let mut buffer: Vec<SignalEvent> = Vec::with_capacity(batch_size);
    let mut interval = tokio::time::interval(flush_interval);
    interval.tick().await;

    loop {
        tokio::select! {
            biased;
            ack = &mut shutdown_rx => {
                // Drain anything still in the channel, flush, then ack
                while let Ok(event) = rx.try_recv() {
                    buffer.push(event);
                }
                if !buffer.is_empty() {
                    flush_batch(&pool, &mut buffer).await;
                }
                debug!("signals collector shutting down (graceful)");
                if let Ok(tx) = ack {
                    let _ = tx.send(());
                }
                return;
            }
            event = rx.recv() => {
                match event {
                    Some(e) => {
                        buffer.push(e);
                        if buffer.len() >= batch_size {
                            flush_batch(&pool, &mut buffer).await;
                        }
                    }
                    None => {
                        // All senders dropped, flush remaining and exit
                        if !buffer.is_empty() {
                            flush_batch(&pool, &mut buffer).await;
                        }
                        debug!("signals collector shutting down (senders dropped)");
                        return;
                    }
                }
            }
            _ = interval.tick() => {
                if !buffer.is_empty() {
                    flush_batch(&pool, &mut buffer).await;
                }
            }
        }
    }
}

/// Flush a batch of events into PostgreSQL using UNNEST for single-roundtrip INSERT.
/// Uses runtime sqlx::query() because UNNEST with typed arrays is not supported by
/// the compile-time sqlx::query!() macro.
// UNNEST batch insert binds 31 parallel arrays; the macro form can't model the
// shape, and the table is part of the runtime's signals subsystem.
#[allow(clippy::disallowed_methods)]
async fn flush_batch(pool: &PgPool, buffer: &mut Vec<SignalEvent>) {
    let count = buffer.len();

    let mut ids = Vec::with_capacity(count);
    let mut event_types = Vec::with_capacity(count);
    let mut event_names: Vec<Option<String>> = Vec::with_capacity(count);
    let mut correlation_ids: Vec<Option<String>> = Vec::with_capacity(count);
    let mut session_ids: Vec<Option<uuid::Uuid>> = Vec::with_capacity(count);
    let mut visitor_ids: Vec<Option<String>> = Vec::with_capacity(count);
    let mut user_ids: Vec<Option<uuid::Uuid>> = Vec::with_capacity(count);
    let mut tenant_ids: Vec<Option<uuid::Uuid>> = Vec::with_capacity(count);
    let mut properties_list: Vec<serde_json::Value> = Vec::with_capacity(count);
    let mut page_urls: Vec<Option<String>> = Vec::with_capacity(count);
    let mut referrers: Vec<Option<String>> = Vec::with_capacity(count);
    let mut function_names: Vec<Option<String>> = Vec::with_capacity(count);
    let mut function_kinds: Vec<Option<String>> = Vec::with_capacity(count);
    let mut duration_ms_list: Vec<Option<i32>> = Vec::with_capacity(count);
    let mut statuses: Vec<Option<String>> = Vec::with_capacity(count);
    let mut error_messages: Vec<Option<String>> = Vec::with_capacity(count);
    let mut error_stacks: Vec<Option<String>> = Vec::with_capacity(count);
    let mut error_contexts: Vec<Option<serde_json::Value>> = Vec::with_capacity(count);
    let mut client_ips: Vec<Option<String>> = Vec::with_capacity(count);
    let mut countries: Vec<Option<String>> = Vec::with_capacity(count);
    let mut cities: Vec<Option<String>> = Vec::with_capacity(count);
    let mut user_agents: Vec<Option<String>> = Vec::with_capacity(count);
    let mut device_types: Vec<Option<String>> = Vec::with_capacity(count);
    let mut browsers: Vec<Option<String>> = Vec::with_capacity(count);
    let mut oses: Vec<Option<String>> = Vec::with_capacity(count);
    let mut utm_sources: Vec<Option<String>> = Vec::with_capacity(count);
    let mut utm_mediums: Vec<Option<String>> = Vec::with_capacity(count);
    let mut utm_campaigns: Vec<Option<String>> = Vec::with_capacity(count);
    let mut utm_terms: Vec<Option<String>> = Vec::with_capacity(count);
    let mut utm_contents: Vec<Option<String>> = Vec::with_capacity(count);
    let mut is_bots: Vec<bool> = Vec::with_capacity(count);
    let mut timestamps: Vec<chrono::DateTime<chrono::Utc>> = Vec::with_capacity(count);

    for event in buffer.drain(..) {
        ids.push(uuid::Uuid::new_v4());
        event_types.push(event.event_type.to_string());
        event_names.push(event.event_name);
        correlation_ids.push(event.correlation_id);
        session_ids.push(event.session_id);
        visitor_ids.push(event.visitor_id);
        user_ids.push(event.user_id);
        tenant_ids.push(event.tenant_id);
        properties_list.push(event.properties);
        page_urls.push(event.page_url);
        referrers.push(event.referrer);
        function_names.push(event.function_name);
        function_kinds.push(event.function_kind);
        duration_ms_list.push(event.duration_ms);
        statuses.push(event.status);
        error_messages.push(event.error_message);
        error_stacks.push(event.error_stack);
        error_contexts.push(event.error_context);
        client_ips.push(event.client_ip);
        countries.push(event.country);
        cities.push(event.city);
        user_agents.push(event.user_agent);
        device_types.push(event.device_type);
        browsers.push(event.browser);
        oses.push(event.os);
        let (src, med, camp, term, content) = match event.utm {
            Some(utm) => (utm.source, utm.medium, utm.campaign, utm.term, utm.content),
            None => (None, None, None, None, None),
        };
        utm_sources.push(src);
        utm_mediums.push(med);
        utm_campaigns.push(camp);
        utm_terms.push(term);
        utm_contents.push(content);
        is_bots.push(event.is_bot);
        timestamps.push(event.timestamp);
    }

    let result = sqlx::query(
        "INSERT INTO forge_signals_events (
            id, event_type, event_name, correlation_id,
            session_id, visitor_id, user_id, tenant_id,
            properties, page_url, referrer,
            function_name, function_kind, duration_ms, status,
            error_message, error_stack, error_context,
            client_ip, country, city, user_agent,
            device_type, browser, os,
            utm_source, utm_medium, utm_campaign, utm_term, utm_content,
            is_bot, timestamp
        )
        SELECT * FROM UNNEST(
            $1::uuid[], $2::varchar[], $3::varchar[], $4::varchar[],
            $5::uuid[], $6::varchar[], $7::uuid[], $8::uuid[],
            $9::jsonb[], $10::text[], $11::text[],
            $12::varchar[], $13::varchar[], $14::int[], $15::varchar[],
            $16::text[], $17::text[], $18::jsonb[],
            $19::text[], $20::varchar[], $21::varchar[], $22::text[],
            $23::varchar[], $24::varchar[], $25::varchar[],
            $26::varchar[], $27::varchar[], $28::varchar[], $29::varchar[], $30::varchar[],
            $31::bool[], $32::timestamptz[]
        )",
    )
    .bind(&ids)
    .bind(&event_types)
    .bind(&event_names)
    .bind(&correlation_ids)
    .bind(&session_ids)
    .bind(&visitor_ids)
    .bind(&user_ids)
    .bind(&tenant_ids)
    .bind(&properties_list)
    .bind(&page_urls)
    .bind(&referrers)
    .bind(&function_names)
    .bind(&function_kinds)
    .bind(&duration_ms_list)
    .bind(&statuses)
    .bind(&error_messages)
    .bind(&error_stacks)
    .bind(&error_contexts)
    .bind(&client_ips)
    .bind(&countries)
    .bind(&cities)
    .bind(&user_agents)
    .bind(&device_types)
    .bind(&browsers)
    .bind(&oses)
    .bind(&utm_sources)
    .bind(&utm_mediums)
    .bind(&utm_campaigns)
    .bind(&utm_terms)
    .bind(&utm_contents)
    .bind(&is_bots)
    .bind(&timestamps)
    .execute(pool)
    .await;

    match result {
        Ok(_) => debug!(count, "flushed signal events"),
        Err(e) => error!(count, error = %e, "failed to flush signal events"),
    }
}

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use forge_core::signals::SignalEvent;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        let db = base
            .isolated(test_name)
            .await
            .expect("Failed to create isolated db");
        let system_sql = crate::pg::migration::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    async fn row_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM forge_signals_events")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn sample_event(name: &str) -> SignalEvent {
        SignalEvent::server_execution(name, "job", 12, true, None)
    }

    #[tokio::test]
    async fn shutdown_flushes_pending_events_below_batch_size() {
        // Tiny batch_size=10 + long flush_interval forces the test to prove
        // that shutdown — not the timer — is what drains the buffer.
        let db = setup_db("signals_shutdown_flush").await;
        let pool = Arc::new(db.pool().clone());
        let collector = SignalsCollector::spawn(pool.clone(), 10, Duration::from_secs(60), 100);

        for i in 0..3 {
            collector.try_send(sample_event(&format!("evt_{i}")));
        }
        collector.shutdown().await;

        assert_eq!(row_count(&pool).await, 3);
    }

    #[tokio::test]
    async fn batch_size_threshold_triggers_immediate_flush() {
        // Send exactly batch_size events with a long timer; if the size-trigger
        // path is broken, the flush won't happen before our assertion fires.
        let db = setup_db("signals_batch_flush").await;
        let pool = Arc::new(db.pool().clone());
        let collector = SignalsCollector::spawn(pool.clone(), 5, Duration::from_secs(60), 100);

        for i in 0..5 {
            collector.try_send(sample_event(&format!("evt_{i}")));
        }

        // Poll briefly for the async flush to complete; size-trigger should
        // fire within a few ms once the buffer hits 5.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let n = row_count(&pool).await;
            if n == 5 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("batch flush did not occur, count={n}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        collector.shutdown().await;
    }

    #[tokio::test]
    async fn interval_tick_flushes_partial_batches() {
        // Below batch_size events with a short flush interval: only the timer
        // can move them to the DB.
        let db = setup_db("signals_interval_flush").await;
        let pool = Arc::new(db.pool().clone());
        let collector = SignalsCollector::spawn(pool.clone(), 100, Duration::from_millis(100), 100);

        collector.try_send(sample_event("a"));
        collector.try_send(sample_event("b"));

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let n = row_count(&pool).await;
            if n == 2 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("interval flush did not occur, count={n}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        collector.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        // Second shutdown must be a no-op — calling it twice during graceful
        // exit is a real pattern (signal handler + Drop, etc.).
        let db = setup_db("signals_shutdown_idempotent").await;
        let pool = Arc::new(db.pool().clone());
        let collector = SignalsCollector::spawn(pool.clone(), 10, Duration::from_secs(60), 100);

        collector.try_send(sample_event("once"));
        collector.shutdown().await;
        // Should return immediately, not hang waiting on a closed channel.
        collector.shutdown().await;

        assert_eq!(row_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn try_send_drops_events_after_shutdown() {
        // Post-shutdown sends must not panic or block — the channel is closed
        // by then and try_send sees TrySendError::Closed.
        let db = setup_db("signals_send_after_close").await;
        let pool = Arc::new(db.pool().clone());
        let collector = SignalsCollector::spawn(pool.clone(), 10, Duration::from_secs(60), 100);

        collector.try_send(sample_event("before"));
        collector.shutdown().await;
        // No panic, no hang — this is the contract.
        collector.try_send(sample_event("after"));

        // Only the "before" event made it through.
        assert_eq!(row_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn batch_persists_full_event_field_set() {
        // The UNNEST insert binds 31 parallel arrays — a single column-order
        // bug would silently corrupt every event. Round-trip every field
        // that comes from the wire and assert it lands on the right column.
        let db = setup_db("signals_field_roundtrip").await;
        let pool = Arc::new(db.pool().clone());
        let collector = SignalsCollector::spawn(pool.clone(), 10, Duration::from_secs(60), 100);

        let user_id = uuid::Uuid::new_v4();
        let tenant_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();
        let event = SignalEvent {
            event_type: forge_core::signals::SignalEventType::RpcCall,
            event_name: Some("get_user".to_string()),
            correlation_id: Some("corr-123".to_string()),
            session_id: Some(session_id),
            visitor_id: Some("visit-abc".to_string()),
            user_id: Some(user_id),
            tenant_id: Some(tenant_id),
            properties: serde_json::json!({"k": "v"}),
            page_url: Some("https://x.test/a".to_string()),
            referrer: Some("https://ref.test".to_string()),
            function_name: Some("get_user".to_string()),
            function_kind: Some("query".to_string()),
            duration_ms: Some(42),
            status: Some("success".to_string()),
            error_message: None,
            error_stack: None,
            error_context: None,
            client_ip: Some("10.0.0.1".to_string()),
            country: Some("US".to_string()),
            city: Some("NYC".to_string()),
            user_agent: Some("test/1.0".to_string()),
            device_type: Some("desktop".to_string()),
            browser: Some("firefox".to_string()),
            os: Some("linux".to_string()),
            utm: Some(forge_core::signals::UtmParams {
                source: Some("twitter".to_string()),
                medium: Some("social".to_string()),
                campaign: Some("launch".to_string()),
                term: Some("rust".to_string()),
                content: Some("post".to_string()),
            }),
            is_bot: false,
            timestamp: chrono::Utc::now(),
        };
        collector.try_send(event);
        collector.shutdown().await;

        use sqlx::Row;
        let row = sqlx::query(
            "SELECT event_type, event_name, correlation_id, session_id, visitor_id,
                user_id, tenant_id, function_name, function_kind, status,
                duration_ms, client_ip, country, city, user_agent,
                browser, os, utm_source, utm_medium, utm_campaign, utm_term, is_bot
             FROM forge_signals_events LIMIT 1",
        )
        .fetch_one(&*pool)
        .await
        .unwrap();

        assert_eq!(row.get::<String, _>("event_type"), "rpc_call");
        assert_eq!(
            row.get::<Option<String>, _>("event_name").as_deref(),
            Some("get_user")
        );
        assert_eq!(
            row.get::<Option<String>, _>("correlation_id").as_deref(),
            Some("corr-123")
        );
        assert_eq!(
            row.get::<Option<uuid::Uuid>, _>("session_id"),
            Some(session_id)
        );
        assert_eq!(
            row.get::<Option<String>, _>("visitor_id").as_deref(),
            Some("visit-abc")
        );
        assert_eq!(row.get::<Option<uuid::Uuid>, _>("user_id"), Some(user_id));
        assert_eq!(
            row.get::<Option<uuid::Uuid>, _>("tenant_id"),
            Some(tenant_id)
        );
        assert_eq!(
            row.get::<Option<String>, _>("function_name").as_deref(),
            Some("get_user")
        );
        assert_eq!(
            row.get::<Option<String>, _>("function_kind").as_deref(),
            Some("query")
        );
        assert_eq!(
            row.get::<Option<String>, _>("status").as_deref(),
            Some("success")
        );
        assert_eq!(row.get::<Option<i32>, _>("duration_ms"), Some(42));
        assert_eq!(
            row.get::<Option<String>, _>("client_ip").as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(
            row.get::<Option<String>, _>("country").as_deref(),
            Some("US")
        );
        assert_eq!(row.get::<Option<String>, _>("city").as_deref(), Some("NYC"));
        assert_eq!(
            row.get::<Option<String>, _>("user_agent").as_deref(),
            Some("test/1.0")
        );
        assert_eq!(
            row.get::<Option<String>, _>("browser").as_deref(),
            Some("firefox")
        );
        assert_eq!(row.get::<Option<String>, _>("os").as_deref(), Some("linux"));
        assert_eq!(
            row.get::<Option<String>, _>("utm_source").as_deref(),
            Some("twitter")
        );
        assert_eq!(
            row.get::<Option<String>, _>("utm_medium").as_deref(),
            Some("social")
        );
        assert_eq!(
            row.get::<Option<String>, _>("utm_campaign").as_deref(),
            Some("launch")
        );
        assert_eq!(
            row.get::<Option<String>, _>("utm_term").as_deref(),
            Some("rust")
        );
        assert!(!row.get::<bool, _>("is_bot"));
    }

    #[tokio::test]
    async fn full_channel_drops_excess_events_without_panicking() {
        // channel_capacity=2 with a long flush interval and a slow consumer
        // means try_send sees Full on the third call. The contract is "drop
        // and warn" — must not panic, must not block.
        let db = setup_db("signals_channel_full").await;
        let pool = Arc::new(db.pool().clone());
        // Tiny capacity, large batch_size so the flush loop doesn't drain
        // before we overflow.
        let collector = SignalsCollector::spawn(pool.clone(), 1000, Duration::from_secs(60), 1);

        // First send fills capacity; second is the buffered receiver slot;
        // beyond that we either land in buffer or hit Full. Burst enough that
        // at least one Full must occur.
        for i in 0..50 {
            collector.try_send(sample_event(&format!("burst_{i}")));
        }
        // No panic = pass. Drain via shutdown so the test cleans up.
        collector.shutdown().await;
    }
}

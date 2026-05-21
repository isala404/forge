//! Signals: product analytics and frontend diagnostics for Dioxus apps.
//!
//! Tracks user behavior, page views, errors, and custom events.
//! GDPR-compliant (no cookies, no persistent client IDs).
//!
//! ## Usage
//!
//! ```rust,ignore
//! let signals = use_signals();
//! signals.track_with_properties("button_clicked", json!({"id": "signup"}));
//! signals.capture_error("Something went wrong", None);
//! ```

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ForgeClient;

const DEFAULT_FLUSH_INTERVAL_MS: u32 = 5000;
const DEFAULT_MAX_BATCH: usize = 20;
const MAX_BREADCRUMBS: usize = 20;
const MAX_QUEUE_SIZE: usize = 1000;
#[cfg(target_arch = "wasm32")]
const AUTO_CAPTURE_DELAY_MS: u64 = 2000;

// Matches the Svelte client's localStorage key so events queued on one
// runtime can be reclaimed by the other across page reloads.
#[cfg(target_arch = "wasm32")]
const PERSIST_KEY: &str = "forge_signals_queue_v1";

fn warn_serialize_failed(label: &str, err: &serde_json::Error) {
    #[cfg(target_arch = "wasm32")]
    {
        let msg = format!("[forge-signals] dropped {label}: {err}");
        web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&msg));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("[forge-signals] dropped {label}: {err}");
    }
}

// Inline JS shims compiled by wasm-bindgen at build time. Avoids runtime
// `eval()` so the tracker keeps working under strict CSP (`script-src 'self'`).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function forge_patch_history() {
    var origPush = history.pushState;
    var origReplace = history.replaceState;
    history.pushState = function() {
        var before = location.href;
        origPush.apply(this, arguments);
        if (location.href !== before) {
            window.dispatchEvent(new Event('forge-pushstate'));
        }
    };
    history.replaceState = function() {
        var before = location.href;
        origReplace.apply(this, arguments);
        if (location.href !== before) {
            window.dispatchEvent(new Event('forge-pushstate'));
        }
    };
}

export function forge_install_web_vitals(baseUrl, getSessionId) {
    try {
        function send(name, value, rating, attribution) {
            try {
                const body = JSON.stringify({
                    type: 'event',
                    payload: {
                        events: [{
                            event: 'webvital.' + name,
                            properties: {
                                value: value,
                                rating: rating || null,
                                attribution: attribution || {},
                            },
                            timestamp: new Date().toISOString(),
                        }],
                        context: {
                            page_url: location.href,
                            session_id: getSessionId() || null,
                        }
                    }
                });
                const url = baseUrl + '/_api/signal';
                const headers = { 'Content-Type': 'application/json', 'x-forge-platform': 'web' };
                if (navigator.sendBeacon) {
                    navigator.sendBeacon(url, body);
                } else {
                    fetch(url, { method: 'POST', headers: headers, body: body, keepalive: true });
                }
            } catch (_) {}
        }
        function obs(type, cb) {
            try {
                new PerformanceObserver(function(list) {
                    list.getEntries().forEach(cb);
                }).observe({ type: type, buffered: true });
            } catch (_) {}
        }
        var lcp = 0;
        obs('largest-contentful-paint', function(e) { lcp = e.renderTime || e.loadTime || e.startTime; });
        var cls = 0;
        obs('layout-shift', function(e) { if (!e.hadRecentInput) cls += e.value; });
        obs('paint', function(e) {
            if (e.name === 'first-contentful-paint') {
                var r = e.startTime < 1800 ? 'good' : e.startTime < 3000 ? 'needs-improvement' : 'poor';
                send('fcp', e.startTime, r);
            }
        });
        obs('event', function(e) {
            if (e.interactionId && e.duration > 40) {
                var r = e.duration < 200 ? 'good' : e.duration < 500 ? 'needs-improvement' : 'poor';
                send('inp', e.duration, r, { name: e.name });
            }
        });
        obs('longtask', function(e) {
            send('long_task', e.duration, null, { name: e.name, startTime: e.startTime });
        });
        function onLoad() {
            try {
                var nav = performance.getEntriesByType('navigation')[0];
                if (nav) {
                    if (nav.responseStart > 0) {
                        var r = nav.responseStart < 800 ? 'good' : nav.responseStart < 1800 ? 'needs-improvement' : 'poor';
                        send('ttfb', nav.responseStart, r);
                    }
                    send('navigation', nav.loadEventEnd - nav.startTime, null, {
                        dom_content_loaded: nav.domContentLoadedEventEnd - nav.startTime,
                        dom_interactive: nav.domInteractive - nav.startTime,
                        transfer_size: nav.transferSize,
                        type: nav.type,
                    });
                }
            } catch (_) {}
        }
        if (document.readyState === 'complete') onLoad();
        else window.addEventListener('load', onLoad);
        document.addEventListener('visibilitychange', function() {
            if (document.visibilityState === 'hidden') {
                if (lcp > 0) {
                    var r = lcp < 2500 ? 'good' : lcp < 4000 ? 'needs-improvement' : 'poor';
                    send('lcp', lcp, r);
                    lcp = 0;
                }
                if (cls > 0) {
                    var r = cls < 0.1 ? 'good' : cls < 0.25 ? 'needs-improvement' : 'poor';
                    send('cls', cls, r);
                    cls = 0;
                }
            }
        });
    } catch (_) {}
}
"#)]
extern "C" {
    fn forge_patch_history();
    fn forge_install_web_vitals(base_url: &str, get_session: &js_sys::Function);
}

fn now_iso() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(s) = js_sys::Date::new_0().to_iso_string().as_string() {
            return s;
        }
        // toISOString unexpectedly returned a non-string. Fall through to the
        // portable formatter so analytics never carry an empty timestamp.
        let secs = (js_sys::Date::now() / 1000.0) as u64;
        return format_iso(secs);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format_iso(secs)
    }
}

/// ISO 8601 second-precision formatter ("2024-03-28T12:34:56Z") without
/// pulling in chrono. Shared across wasm and native code paths.
fn format_iso(secs: u64) -> String {
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Civil date from days since Unix epoch (Euclidean affine algorithm)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a correlation ID (counter + random suffix).
fn generate_correlation_id() -> String {
    let counter = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("dx-{counter}-{:08x}", rand_u32())
}

fn rand_u32() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Math::random() * f64::from(u32::MAX)) as u32
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Correlation IDs only need to be unique within a process, not
        // cryptographically random. Mix nanos with the global counter so
        // rapid successive calls (which would land in the same nanosecond)
        // still diverge.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        nanos ^ CORRELATION_COUNTER.load(Ordering::Relaxed) as u32
    }
}

/// Configuration for signals collection.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalsConfig {
    /// Enable signals collection (default: true).
    pub enabled: bool,
    /// Auto-track page views on navigation (default: true).
    pub auto_page_views: bool,
    /// Auto-capture frontend errors (default: true).
    pub auto_capture_errors: bool,
    /// Auto-capture Web Vitals (WASM only: FCP, LCP, TTFB, long tasks) (default: true).
    pub auto_web_vitals: bool,
    /// Auto-capture online/offline transitions (default: true).
    pub auto_network_events: bool,
    /// Respect DNT / Sec-GPC and disable on opt-out (default: true).
    pub respect_dnt: bool,
    /// Flush interval in ms (default: 5000).
    pub flush_interval: u32,
    /// Max events per batch (default: 20).
    pub max_batch_size: usize,
    /// Persist the outbound queue to localStorage so events queued before
    /// a reload survive (WASM only; ignored on native). Default: true.
    pub persist_queue: bool,
}

impl Default for SignalsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_page_views: true,
            auto_capture_errors: true,
            auto_web_vitals: true,
            auto_network_events: true,
            respect_dnt: true,
            flush_interval: DEFAULT_FLUSH_INTERVAL_MS,
            max_batch_size: DEFAULT_MAX_BATCH,
            persist_queue: true,
        }
    }
}

/// Check Do-Not-Track / Sec-GPC on the current browser (WASM only).
#[cfg(target_arch = "wasm32")]
fn has_opted_out() -> bool {
    if let Some(win) = web_sys::window() {
        let nav = win.navigator();
        if let Ok(val) = js_sys::Reflect::get(&nav, &"doNotTrack".into())
            && let Some(s) = val.as_string()
        {
            if s == "1" || s == "yes" {
                return true;
            }
        }
        if let Ok(val) = js_sys::Reflect::get(&nav, &"globalPrivacyControl".into())
            && val.as_bool() == Some(true)
        {
            return true;
        }
    }
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn has_opted_out() -> bool {
    false
}

#[derive(Clone, Serialize, Deserialize)]
struct SignalEventPayload {
    event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    properties: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

#[derive(Clone, Serialize)]
struct EventBatch {
    events: Vec<SignalEventPayload>,
    context: Option<BatchContext>,
}

#[derive(Clone, Serialize)]
struct BatchContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    page_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

#[derive(Clone, Serialize)]
struct DiagnosticPayload {
    errors: Vec<ErrorPayload>,
}

#[derive(Clone, Serialize)]
struct ErrorPayload {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    #[serde(skip_serializing_if = "VecDeque::is_empty")]
    breadcrumbs: VecDeque<BreadcrumbEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_url: Option<String>,
}

#[derive(Clone, Serialize)]
struct BreadcrumbEntry {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    timestamp: String,
}

struct SignalsInner {
    client: ForgeClient,
    config: SignalsConfig,
    queue: Vec<SignalEventPayload>,
    breadcrumbs: VecDeque<BreadcrumbEntry>,
    session_id: Option<String>,
    last_correlation_id: Option<String>,
    utm_params: Option<Value>,
    destroyed: bool,
}

/// Product analytics and diagnostics handle.
///
/// Obtain via `use_signals()` inside a `ForgeProvider`.
#[derive(Clone)]
pub struct ForgeSignals {
    inner: Rc<RefCell<SignalsInner>>,
}

/// Error value accepted by [`ForgeSignals::capture_error`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SignalError {
    message: String,
    stack: Option<String>,
}

impl SignalError {
    /// Create a signal error from a message and optional stack trace.
    pub fn new(message: impl Into<String>, stack: Option<String>) -> Self {
        Self {
            message: message.into(),
            stack,
        }
    }

    /// Create a signal error from a Rust error value.
    pub fn from_error(error: &(dyn std::error::Error + '_)) -> Self {
        Self::new(error.to_string(), None)
    }
}

impl From<&str> for SignalError {
    fn from(value: &str) -> Self {
        Self::new(value, None)
    }
}

impl From<String> for SignalError {
    fn from(value: String) -> Self {
        Self::new(value, None)
    }
}

#[cfg(target_arch = "wasm32")]
impl From<js_sys::Error> for SignalError {
    fn from(value: js_sys::Error) -> Self {
        Self::new(value.message().as_string().unwrap_or_default(), None)
    }
}

impl ForgeSignals {
    /// Create a new signals instance tied to a ForgeClient.
    pub fn new(client: ForgeClient, mut config: SignalsConfig) -> Self {
        // Honor DNT / Sec-GPC up front so the rest of the module can just
        // check `config.enabled` once.
        if config.enabled && config.respect_dnt && has_opted_out() {
            config.enabled = false;
        }
        let utm_params = if config.enabled { extract_utm() } else { None };
        let signals = Self {
            inner: Rc::new(RefCell::new(SignalsInner {
                client,
                config,
                queue: Vec::new(),
                breadcrumbs: VecDeque::new(),
                session_id: None,
                last_correlation_id: None,
                utm_params,
                destroyed: false,
            })),
        };
        signals.restore_queue();
        signals
    }

    /// Restore any events stashed in localStorage from a prior page session.
    /// No-op on native (no localStorage) and when `persist_queue` is disabled.
    fn restore_queue(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let inner = self.inner.borrow();
            if !inner.config.enabled || !inner.config.persist_queue { return; }
            drop(inner);
            let Some(storage) = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
            else {
                return;
            };
            let raw = match storage.get_item(PERSIST_KEY) {
                Ok(Some(v)) => v,
                _ => return,
            };
            match serde_json::from_str::<Vec<SignalEventPayload>>(&raw) {
                Ok(mut restored) => {
                    if restored.is_empty() { return; }
                    restored.truncate(MAX_QUEUE_SIZE);
                    self.inner.borrow_mut().queue.extend(restored);
                }
                Err(_) => {
                    // Corrupt entry — drop it so we don't try again next reload.
                    let _ = storage.remove_item(PERSIST_KEY);
                }
            }
        }
    }

    /// Persist the pending queue so events survive a reload.
    /// No-op on native and when `persist_queue` is disabled.
    fn persist_queue(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let inner = self.inner.borrow();
            if !inner.config.persist_queue { return; }
            let Some(storage) = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
            else {
                return;
            };
            if inner.queue.is_empty() {
                let _ = storage.remove_item(PERSIST_KEY);
                return;
            }
            match serde_json::to_string(&inner.queue) {
                Ok(s) => {
                    // Quota / private mode failures are silent by design — the
                    // queue stays in memory and the next flush will drain it.
                    let _ = storage.set_item(PERSIST_KEY, &s);
                }
                Err(e) => warn_serialize_failed("persisted queue", &e),
            }
        }
    }

    /// Send a Web Vitals-style measurement. Batches with track events.
    pub fn vital(&self, name: &str, value: f64, rating: Option<&str>) {
        let mut props = serde_json::Map::new();
        props.insert(
            "value".to_string(),
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        if let Some(r) = rating {
            props.insert("rating".to_string(), Value::String(r.to_string()));
        }
        self.track_with_properties(&format!("webvital.{name}"), Value::Object(props));
    }

    #[must_use]
    pub fn auto_web_vitals(&self) -> bool {
        let inner = self.inner.borrow();
        inner.config.enabled && inner.config.auto_web_vitals
    }

    #[must_use]
    pub fn auto_network_events(&self) -> bool {
        let inner = self.inner.borrow();
        inner.config.enabled && inner.config.auto_network_events
    }

    /// Track a custom event with no custom properties.
    pub fn track(&self, event: &str) {
        self.enqueue_track(event, None);
    }

    /// Track a custom event with arbitrary JSON properties.
    pub fn track_with_properties(&self, event: &str, properties: Value) {
        self.enqueue_track(event, Some(properties));
    }

    fn enqueue_track(&self, event: &str, properties: Option<Value>) {
        let inner = self.inner.borrow();
        if !inner.config.enabled { return; }
        drop(inner);

        let correlation_id = self.inner.borrow().last_correlation_id.clone();
        let payload = SignalEventPayload {
            event: event.to_string(),
            properties,
            correlation_id,
            timestamp: Some(now_iso()),
        };
        let mut inner = self.inner.borrow_mut();
        inner.queue.push(payload);
        let should_flush = inner.queue.len() >= inner.config.max_batch_size;
        drop(inner);
        self.persist_queue();
        if should_flush {
            let this = self.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move { this.flush().await; });
            #[cfg(not(target_arch = "wasm32"))]
            spawn(async move { this.flush().await; });
        }
    }

    /// Identify the current user (links session to user).
    pub fn identify(&self, user_id: &str, traits: Value) {
        self.track_with_properties(
            "identify",
            json!({ "user_id": user_id, "traits": traits }),
        );
    }

    /// Track a page view.
    pub async fn page(&self, url_path: &str) {
        let (base_url, session_id, utm) = {
            let mut inner = self.inner.borrow_mut();
            if !inner.config.enabled { return; }
            let utm = inner.utm_params.take();
            (inner.client.get_url().to_string(), inner.session_id.clone(), utm)
        };

        let mut payload = json!({ "url": url_path });
        if let Some(utm_val) = utm
            && let (Some(target), Some(source)) = (payload.as_object_mut(), utm_val.as_object())
        {
            for (k, v) in source {
                target.insert(k.clone(), v.clone());
            }
        }

        let wrapped = json!({ "type": "view", "payload": payload });
        if let Ok(resp) = post_signal(&base_url, "signal", &wrapped, session_id.as_deref()).await
            && let Some(sid) = resp.get("session_id").and_then(|v| v.as_str())
        {
            self.inner.borrow_mut().session_id = Some(sid.to_string());
        }
    }

    /// Capture a frontend error with optional context.
    pub fn capture_error(&self, error: impl Into<SignalError>, context: Option<Value>) {
        let error = error.into();
        let this = self.clone();
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            this.report_error(error, context).await;
        });
        #[cfg(not(target_arch = "wasm32"))]
        spawn(async move {
            this.report_error(error, context).await;
        });
    }

    async fn report_error(&self, error: SignalError, context: Option<Value>) {
        let (url, session_id, correlation_id, breadcrumbs, page_url) = {
            let inner = self.inner.borrow();
            if !inner.config.enabled { return; }
            (
                inner.client.get_url().to_string(),
                inner.session_id.clone(),
                inner.last_correlation_id.clone(),
                inner.breadcrumbs.clone(),
                current_page_url(),
            )
        };

        let body = DiagnosticPayload {
            errors: vec![ErrorPayload {
                message: error.message,
                stack: error.stack,
                context,
                correlation_id,
                breadcrumbs,
                page_url,
            }],
        };
        let payload = match serde_json::to_value(&body) {
            Ok(v) => v,
            Err(e) => {
                warn_serialize_failed("error report", &e);
                return;
            }
        };
        let wrapped = json!({ "type": "report", "payload": payload });
        let _ = post_signal(&url, "signal", &wrapped, session_id.as_deref()).await;
    }

    /// Add a breadcrumb for error reproduction context.
    pub fn breadcrumb(&self, message: &str, data: Option<Value>) {
        let mut inner = self.inner.borrow_mut();
        if !inner.config.enabled { return; }
        inner.breadcrumbs.push_back(BreadcrumbEntry {
            message: message.to_string(),
            data,
            timestamp: now_iso(),
        });
        if inner.breadcrumbs.len() > MAX_BREADCRUMBS {
            inner.breadcrumbs.pop_front();
        }
    }

    /// Generate a correlation ID for the next RPC call.
    pub fn next_correlation_id(&self) -> String {
        let id = generate_correlation_id();
        self.inner.borrow_mut().last_correlation_id = Some(id.clone());
        id
    }

    #[must_use]
    pub fn get_session_id(&self) -> Option<String> {
        self.inner.borrow().session_id.clone()
    }

    /// Flush queued events to the server.
    pub async fn flush(&self) {
        let (url, mut events, session_id) = {
            let mut inner = self.inner.borrow_mut();
            if inner.queue.is_empty() { return; }
            let max = inner.config.max_batch_size;
            let count = inner.queue.len().min(max);
            let events: Vec<_> = inner.queue.drain(..count).collect();
            (inner.client.get_url().to_string(), events, inner.session_id.clone())
        };

        let batch = EventBatch {
            events: events.clone(),
            context: Some(BatchContext {
                page_url: current_page_url(),
                session_id: session_id.clone(),
            }),
        };

        let payload = match serde_json::to_value(&batch) {
            Ok(v) => v,
            Err(e) => {
                warn_serialize_failed("event batch", &e);
                let mut inner = self.inner.borrow_mut();
                events.append(&mut inner.queue);
                events.truncate(MAX_QUEUE_SIZE);
                inner.queue = events;
                drop(inner);
                self.persist_queue();
                return;
            }
        };
        let wrapped = json!({ "type": "event", "payload": payload });
        match post_signal(&url, "signal", &wrapped, session_id.as_deref()).await
        {
            Ok(resp) => {
                if let Some(sid) = resp.get("session_id").and_then(|v| v.as_str()) {
                    self.inner.borrow_mut().session_id = Some(sid.to_string());
                }
                self.persist_queue();
            }
            Err(()) => {
                let mut inner = self.inner.borrow_mut();
                events.extend(inner.queue.drain(..));
                events.truncate(MAX_QUEUE_SIZE);
                inner.queue = events;
                drop(inner);
                self.persist_queue();
            }
        }
    }

    /// Clean up timers and flush remaining events.
    pub fn destroy(&self) {
        self.inner.borrow_mut().destroyed = true;
        flush_beacon(self);
    }

    #[must_use]
    pub fn auto_page_views(&self) -> bool {
        let inner = self.inner.borrow();
        inner.config.enabled && inner.config.auto_page_views
    }

    #[must_use]
    pub fn auto_capture_errors(&self) -> bool {
        let inner = self.inner.borrow();
        inner.config.enabled && inner.config.auto_capture_errors
    }

    #[must_use]
    pub fn flush_interval(&self) -> u32 {
        self.inner.borrow().config.flush_interval
    }

    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.inner.borrow().destroyed
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.inner.borrow().config.enabled
    }
}

/// Send remaining events via beacon API on page unload (WASM only).
fn flush_beacon(signals: &ForgeSignals) {
    let (url, events, session_id) = {
        let mut inner = signals.inner.borrow_mut();
        if inner.queue.is_empty() { return; }
        let events = std::mem::take(&mut inner.queue);
        (inner.client.get_url().to_string(), events, inner.session_id.clone())
    };
    // Beacon is best-effort; clear the persisted copy so a reload after
    // unload doesn't double-send.
    signals.persist_queue();

    let batch = EventBatch {
        events,
        context: Some(BatchContext {
            page_url: current_page_url(),
            session_id,
        }),
    };

    let wrapped = json!({ "type": "event", "payload": &batch });
    let body = match serde_json::to_string(&wrapped) {
        Ok(s) => s,
        Err(e) => {
            warn_serialize_failed("beacon event batch", &e);
            return;
        }
    };

    #[cfg(target_arch = "wasm32")]
    {
        let url = format!("{url}/_api/signal");
        if let Some(navigator) = web_sys::window().map(|w| w.navigator()) {
            let _ = navigator.send_beacon_with_opt_str(&url, Some(&body));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (url, body);
    }
}

/// Get current page URL if in browser.
fn current_page_url() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.location().href().ok())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Extract UTM parameters from query string.
fn extract_utm() -> Option<Value> {
    #[cfg(target_arch = "wasm32")]
    {
        let search = web_sys::window()?.location().search().ok()?;
        let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
        let mut utm = serde_json::Map::new();
        for key in &["utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content"] {
            if let Some(val) = params.get(key) {
                utm.insert(key.to_string(), Value::String(val));
            }
        }
        if utm.is_empty() { None } else { Some(Value::Object(utm)) }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Platform identifier sent as `x-forge-platform` header so the server
/// can populate `device_type` without guessing from the User-Agent.
pub(crate) fn platform_tag() -> &'static str {
    #[cfg(target_arch = "wasm32")]
    { "web" }

    #[cfg(not(target_arch = "wasm32"))]
    {
        #[cfg(target_os = "macos")]
        { "desktop-macos" }
        #[cfg(target_os = "ios")]
        { "ios" }
        #[cfg(target_os = "android")]
        { "android" }
        #[cfg(target_os = "windows")]
        { "desktop-windows" }
        #[cfg(target_os = "linux")]
        { "desktop-linux" }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "android",
            target_os = "windows",
            target_os = "linux",
        )))]
        { "desktop" }
    }
}

/// POST to a signal endpoint and return the JSON response.
async fn post_signal(
    base_url: &str,
    path: &str,
    body: &Value,
    session_id: Option<&str>,
) -> Result<Value, ()> {
    #[cfg(target_arch = "wasm32")]
    {
        use gloo_net::http::Request;
        let mut req = Request::post(&format!("{base_url}/_api/{path}"))
            .header("Content-Type", "application/json")
            .header("x-forge-platform", platform_tag());
        if let Some(sid) = session_id {
            req = req.header("x-session-id", sid);
        }
        let resp = req.body(body.to_string()).map_err(|_| ())?.send().await.map_err(|_| ())?;
        resp.json().await.map_err(|_| ())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use reqwest::Client;
        let mut req = Client::new()
            .post(format!("{base_url}/_api/{path}"))
            .header("x-forge-platform", platform_tag())
            .json(body);
        if let Some(sid) = session_id {
            req = req.header("x-session-id", sid);
        }
        let resp = req.send().await.map_err(|_| ())?;
        resp.json().await.map_err(|_| ())
    }
}

/// Hook to access the signals instance from within a ForgeProvider.
pub fn use_signals() -> ForgeSignals {
    use_context::<ForgeSignals>()
}

/// Setup auto-capture features (page views, errors, periodic flush, unload flush).
/// Called from ForgeProvider after signals are provided as context.
#[cfg(target_arch = "wasm32")]
pub(crate) fn setup_auto_capture(signals: ForgeSignals) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::spawn_local;

    if !signals.is_enabled() { return; }

    let flush_interval = signals.flush_interval();

    // Periodic flush timer
    {
        let signals = signals.clone();
        spawn_local(async move {
            loop {
                gloo_timers::future::sleep(std::time::Duration::from_millis(u64::from(flush_interval))).await;
                if signals.is_destroyed() { break; }
                signals.flush().await;
            }
        });
    }

    // Deferred auto-capture setup to avoid competing with SSE for DB pool connections
    {
        let signals = signals.clone();
        spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(AUTO_CAPTURE_DELAY_MS)).await;
            if signals.is_destroyed() { return; }

            let window = match web_sys::window() {
                Some(w) => w,
                None => return,
            };

            // Auto page views: track initial + monkey-patch history for SPA navigation
            if signals.auto_page_views() {
                let path = window.location().pathname().unwrap_or_else(|_| "/".to_string());
                let signals_page = signals.clone();
                spawn_local(async move { signals_page.page(&path).await; });

                // Listen for navigation events (pushState, replaceState, popstate)
                {
                    let signals = signals.clone();
                    let closure = Closure::<dyn Fn()>::new(move || {
                        let path = web_sys::window()
                            .and_then(|w| w.location().pathname().ok())
                            .unwrap_or_else(|| "/".to_string());
                        let signals = signals.clone();
                        spawn_local(async move { signals.page(&path).await; });
                    });
                    let _ = window.add_event_listener_with_callback(
                        "forge-pushstate",
                        closure.as_ref().unchecked_ref(),
                    );
                    let _ = window.add_event_listener_with_callback(
                        "popstate",
                        closure.as_ref().unchecked_ref(),
                    );
                    // WASM closures passed to JS have no destructor hook, must leak
                    closure.forget();
                }

                // Monkey-patch pushState/replaceState to dispatch custom events
                // Only fires when the URL actually changes to avoid redundant page views
                forge_patch_history();
            }

            // Auto error capture
            if signals.auto_capture_errors() {
                // window.onerror
                {
                    let signals = signals.clone();
                    let closure = Closure::<dyn Fn(web_sys::ErrorEvent)>::new(move |e: web_sys::ErrorEvent| {
                        let msg = e.message();
                        if msg.is_empty() { return; }
                        let signals = signals.clone();
                        spawn_local(async move { signals.capture_error(msg, None); });
                    });
                    let _ = window.add_event_listener_with_callback(
                        "error",
                        closure.as_ref().unchecked_ref(),
                    );
                    // WASM closures passed to JS have no destructor hook, must leak
                    closure.forget();
                }

                {
                    let signals = signals.clone();
                    let closure = Closure::<dyn Fn(web_sys::PromiseRejectionEvent)>::new(
                        move |e: web_sys::PromiseRejectionEvent| {
                            let reason = e.reason();
                            let msg = reason.as_string().unwrap_or_else(|| "Unhandled promise rejection".to_string());
                            let signals = signals.clone();
                            spawn_local(async move { signals.capture_error(msg, None); });
                        },
                    );
                    let _ = window.add_event_listener_with_callback(
                        "unhandledrejection",
                        closure.as_ref().unchecked_ref(),
                    );
                    closure.forget();
                }
            }

            // Flush via beacon when page goes hidden (tab close, navigate away).
            // Bind both visibilitychange and pagehide; Safari sometimes fires only
            // one, and we never want to miss a flush.
            {
                let signals = signals.clone();
                let closure = Closure::<dyn Fn()>::new(move || {
                    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                        if doc.visibility_state() == web_sys::VisibilityState::Hidden {
                            flush_beacon(&signals);
                        }
                    }
                });
                if let Some(doc) = window.document() {
                    let _ = doc.add_event_listener_with_callback(
                        "visibilitychange",
                        closure.as_ref().unchecked_ref(),
                    );
                }
                closure.forget();
            }
            {
                let signals = signals.clone();
                let closure = Closure::<dyn Fn()>::new(move || {
                    flush_beacon(&signals);
                });
                let _ = window.add_event_listener_with_callback(
                    "pagehide",
                    closure.as_ref().unchecked_ref(),
                );
                closure.forget();
            }

            // Network status events
            if signals.auto_network_events() {
                let online_signals = signals.clone();
                let online = Closure::<dyn Fn()>::new(move || {
                    online_signals.track("network.online");
                    let online_signals2 = online_signals.clone();
                    spawn_local(async move { online_signals2.flush().await; });
                });
                let _ = window.add_event_listener_with_callback(
                    "online",
                    online.as_ref().unchecked_ref(),
                );
                online.forget();

                let offline_signals = signals.clone();
                let offline = Closure::<dyn Fn()>::new(move || {
                    offline_signals.track("network.offline");
                });
                let _ = window.add_event_listener_with_callback(
                    "offline",
                    offline.as_ref().unchecked_ref(),
                );
                offline.forget();
            }

            // Web Vitals via PerformanceObserver. Uses best-effort entry types
            // so we don't hard-depend on any bindings that the caller might not
            // have enabled in web-sys features.
            if signals.auto_web_vitals() {
                let base_url = {
                    let inner = signals.inner.borrow();
                    inner.client.get_url().to_string()
                };
                let get_session = {
                    let signals = signals.clone();
                    Closure::<dyn Fn() -> wasm_bindgen::JsValue>::new(move || {
                        match signals.get_session_id() {
                            Some(sid) => wasm_bindgen::JsValue::from_str(&sid),
                            None => wasm_bindgen::JsValue::NULL,
                        }
                    })
                };
                forge_install_web_vitals(&base_url, get_session.as_ref().unchecked_ref());
                get_session.forget();
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn setup_auto_capture(signals: ForgeSignals) {
    if !signals.is_enabled() { return; }

    let flush_interval = signals.flush_interval();

    // Periodic flush timer using tokio
    {
        let signals = signals.clone();
        spawn(async move {
            let mut interval = tokio::time::interval(
                std::time::Duration::from_millis(u64::from(flush_interval)),
            );
            loop {
                interval.tick().await;
                if signals.is_destroyed() { break; }
                signals.flush().await;
            }
        });
    }

    // Capture panics as error reports (desktop/mobile only, WASM uses window.error).
    // The panic hook requires Send+Sync, but ForgeSignals uses Rc (single-threaded).
    // We capture just the base URL and send the report directly via reqwest.
    if signals.auto_capture_errors() {
        let base_url = {
            let inner = signals.inner.borrow();
            inner.client.get_url().to_string()
        };
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("panic");
            let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let url = format!("{}/_api/signal", base_url);
            let body = serde_json::json!({
                "type": "report",
                "payload": {
                    "errors": [{
                        "message": msg,
                        "context": { "location": location, "kind": "panic" },
                    }]
                }
            });
            // Fire-and-forget on a background thread since we can't use the
            // single-threaded Dioxus runtime from inside a panic hook.
            let _ = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    let _ = rt.block_on(async {
                        let _ = reqwest::Client::new()
                            .post(&url)
                            .header("x-forge-platform", platform_tag())
                            .json(&body)
                            .send()
                            .await;
                    });
                }
            });
            prev(info);
        }));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn signals_config_defaults() {
        let config = SignalsConfig::default();
        assert!(config.enabled);
        assert!(config.auto_page_views);
        assert!(config.auto_capture_errors);
        assert_eq!(config.flush_interval, 5000);
        assert_eq!(config.max_batch_size, 20);
    }

    #[test]
    fn correlation_id_format() {
        let id = generate_correlation_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.first().copied(), Some("dx"));
        assert!(parts.get(1).unwrap().parse::<u64>().is_ok());
        let hex_part = parts.get(2).unwrap();
        assert_eq!(hex_part.len(), 8);
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn correlation_ids_are_unique() {
        let a = generate_correlation_id();
        let b = generate_correlation_id();
        assert_ne!(a, b);
    }

    #[test]
    fn platform_tag_returns_expected_value() {
        let tag = platform_tag();
        let known = [
            "web",
            "desktop-macos",
            "desktop-linux",
            "desktop-windows",
            "ios",
            "android",
            "desktop",
        ];
        assert!(
            known.contains(&tag),
            "unexpected platform tag: {tag}",
        );
    }

    #[test]
    fn now_iso_produces_valid_format() {
        let ts = now_iso();
        // Expected: "YYYY-MM-DDTHH:MM:SSZ"
        assert_eq!(ts.len(), 20, "unexpected length for timestamp: {ts}");
        assert_eq!(ts.as_bytes().get(4).copied(), Some(b'-'));
        assert_eq!(ts.as_bytes().get(7).copied(), Some(b'-'));
        assert_eq!(ts.as_bytes().get(10).copied(), Some(b'T'));
        assert_eq!(ts.as_bytes().get(13).copied(), Some(b':'));
        assert_eq!(ts.as_bytes().get(16).copied(), Some(b':'));
        assert_eq!(ts.as_bytes().get(19).copied(), Some(b'Z'));
    }

    #[test]
    fn days_to_date_epoch_start() {
        assert_eq!(days_to_date(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_date_known_date() {
        // 2024-03-28 is 19810 days after 1970-01-01
        assert_eq!(days_to_date(19810), (2024, 3, 28));
    }

    #[test]
    fn days_to_date_leap_year() {
        // 2000-02-29 is day 11016
        assert_eq!(days_to_date(11016), (2000, 2, 29));
    }

    #[test]
    fn days_to_date_century_non_leap() {
        // 2100 is not a leap year; 2100-03-01 is day 47541
        assert_eq!(days_to_date(47541), (2100, 3, 1));
    }

    #[test]
    fn now_iso_is_recent() {
        let ts = now_iso();
        let year: u32 = ts.get(..4).unwrap().parse().unwrap();
        assert!(year >= 2025, "expected year >= 2025, got {year}");
    }
}

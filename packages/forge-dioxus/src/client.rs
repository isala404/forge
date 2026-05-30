
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_channel::oneshot;

use dioxus::prelude::{Signal, WritableExt, dioxus_core::Task};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::signals::ForgeSignals;
use crate::types::{
    ConnectionState, ForgeClientError, ForgeError, RpcEnvelopeRaw, StreamEvent,
};

type TokenProvider = Rc<dyn Fn() -> Option<String>>;
type RefreshTokenProvider =
    Rc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<String>>>>>;
type AuthErrorHandler = Rc<dyn Fn(ForgeError)>;
type MutationErrorHandler = Rc<dyn Fn(ForgeClientError)>;
type EventSender = futures_channel::mpsc::UnboundedSender<SseDispatch>;
type ConnectWaiter = futures_channel::oneshot::Sender<Result<(), ForgeClientError>>;

static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

enum SseDispatch {
    Data(serde_json::Value),
    Error { code: String, message: String },
}

struct RegistrationMeta {
    endpoint: &'static str,
    payload: serde_json::Value,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum SseState {
    #[default]
    Idle,
    Connecting,
    Connected,
}

/// Shared SSE connection state, one per ForgeClient.
#[derive(Default)]
struct SseManager {
    session_id: Option<String>,
    session_secret: Option<String>,
    state: SseState,
    ever_connected: bool,
    listeners: HashMap<String, EventSender>,
    registrations: HashMap<String, RegistrationMeta>,
    event_loop_task: Option<Task>,
    reconnect_attempts: u32,
    connect_waiters: Vec<ConnectWaiter>,
}

#[derive(Clone)]
#[non_exhaustive]
pub struct ForgeClientConfig {
    url: String,
    get_token: Option<TokenProvider>,
    refresh_token: Option<RefreshTokenProvider>,
    on_auth_error: Option<AuthErrorHandler>,
    on_mutation_error: Option<MutationErrorHandler>,
    pub(crate) connection_state: Option<Signal<ConnectionState>>,
}

impl ForgeClientConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            get_token: None,
            refresh_token: None,
            on_auth_error: None,
            on_mutation_error: None,
            connection_state: None,
        }
    }

    pub fn with_token_provider(mut self, provider: impl Fn() -> Option<String> + 'static) -> Self {
        self.get_token = Some(Rc::new(provider));
        self
    }

    /// Register an async callback invoked when an RPC call returns UNAUTHORIZED.
    /// The callback must refresh the access token AND persist it where the
    /// `get_token` provider reads from (typically a `Signal`) before
    /// resolving. The returned `Option<String>` is treated as success/failure;
    /// the client does not inject it directly. On `Some` the original call is
    /// retried once (which calls `get_token` again). On `None` the call fails.
    /// Concurrent 401s are coalesced into a single refresh attempt.
    pub fn with_refresh_token_provider<F, Fut>(mut self, provider: F) -> Self
    where
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = Option<String>> + 'static,
    {
        self.refresh_token = Some(Rc::new(move || Box::pin(provider())));
        self
    }

    pub fn with_auth_error_handler(
        mut self,
        handler: impl Fn(ForgeError) + 'static,
    ) -> Self {
        self.on_auth_error = Some(Rc::new(handler));
        self
    }

    /// Register a callback invoked when [`Mutation::fire`] encounters an error.
    pub fn with_mutation_error_handler(
        mut self,
        handler: impl Fn(ForgeClientError) + 'static,
    ) -> Self {
        self.on_mutation_error = Some(Rc::new(handler));
        self
    }

    pub(crate) fn with_connection_state(mut self, state: Signal<ConnectionState>) -> Self {
        self.connection_state = Some(state);
        self
    }
}

#[derive(Clone)]
pub struct ForgeClient {
    inner: Rc<ForgeClientInner>,
}

struct ForgeClientInner {
    url: String,
    get_token: Option<TokenProvider>,
    refresh_token: Option<RefreshTokenProvider>,
    on_auth_error: Option<AuthErrorHandler>,
    on_mutation_error: Option<MutationErrorHandler>,
    connection_state: Option<Signal<ConnectionState>>,
    sse: RefCell<SseManager>,
    signals: RefCell<Option<ForgeSignals>>,
    /// Coalesces concurrent 401 refresh attempts. While `Some`, in-flight
    /// callers should subscribe via oneshot instead of firing another refresh.
    refresh_waiters: RefCell<Option<Vec<oneshot::Sender<bool>>>>,
}

impl ForgeClient {
    pub fn new(config: ForgeClientConfig) -> Self {
        Self {
            inner: Rc::new(ForgeClientInner {
                url: config.url.trim_end_matches('/').to_string(),
                get_token: config.get_token,
                refresh_token: config.refresh_token,
                on_auth_error: config.on_auth_error,
                on_mutation_error: config.on_mutation_error,
                connection_state: config.connection_state,
                sse: RefCell::new(SseManager::default()),
                signals: RefCell::new(None),
                refresh_waiters: RefCell::new(None),
            }),
        }
    }

    /// Wire signals for correlation ID injection on RPC calls.
    pub fn set_signals(&self, signals: ForgeSignals) {
        *self.inner.signals.borrow_mut() = Some(signals);
    }

    pub fn get_url(&self) -> &str {
        &self.inner.url
    }

    /// Notify the registered mutation error handler, if any.
    pub fn notify_mutation_error(&self, error: ForgeClientError) {
        if let Some(handler) = &self.inner.on_mutation_error {
            handler(error);
        }
    }

    /// Generate a correlation ID from the wired signals instance, if any.
    fn correlation_id(&self) -> Option<String> {
        self.inner.signals.borrow().as_ref().map(|s| s.next_correlation_id())
    }

    pub async fn call<TArgs, TResult>(
        &self,
        function_name: &str,
        args: TArgs,
    ) -> Result<TResult, ForgeClientError>
    where
        TArgs: Serialize,
        TResult: DeserializeOwned,
    {
        let body = serde_json::json!({ "args": args });
        let correlation_id = self.correlation_id();
        let url = format!("{}/_api/rpc/{}", self.inner.url, function_name);

        let envelope =
            platform::request_json(self, &url, body.clone(), correlation_id.as_deref()).await?;

        if envelope_is_unauthorized(&envelope) && self.try_refresh().await {
            let retried =
                platform::request_json(self, &url, body, correlation_id.as_deref()).await?;
            return self.decode_envelope(retried);
        }

        self.decode_envelope(envelope)
    }

    async fn try_refresh(&self) -> bool {
        let Some(provider) = self.inner.refresh_token.clone() else {
            return false;
        };

        // Coalesce: if a refresh is already in flight, wait for its result
        // instead of rotating the refresh token again.
        let (rx, leader) = {
            let mut slot = self.inner.refresh_waiters.borrow_mut();
            match slot.as_mut() {
                Some(waiters) => {
                    let (tx, rx) = oneshot::channel();
                    waiters.push(tx);
                    (Some(rx), false)
                }
                None => {
                    *slot = Some(Vec::new());
                    (None, true)
                }
            }
        };

        if !leader {
            return rx
                .expect("follower must have a receiver")
                .await
                .unwrap_or(false);
        }

        // The provider's `Option<String>` return signals success/failure only.
        // The provider closure MUST install the new token via its own state
        // (e.g. updating a Signal that backs `get_token`) before resolving,
        // since `get_token` is read again on the retry. Returning `Some`
        // without persisting the token will cause the retry to use the
        // stale token.
        let success = provider().await.is_some();

        let waiters = self.inner.refresh_waiters.borrow_mut().take().unwrap_or_default();
        for w in waiters {
            let _ = w.send(success);
        }
        success
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn call_multipart<TResult>(
        &self,
        function_name: &str,
        form: web_sys::FormData,
    ) -> Result<TResult, ForgeClientError>
    where
        TResult: DeserializeOwned,
    {
        let correlation_id = self.correlation_id();
        let envelope = platform::request_multipart(
            self,
            &format!("{}/_api/rpc/{}/upload", self.inner.url, function_name),
            form,
            correlation_id.as_deref(),
        )
        .await?;
        self.decode_envelope(envelope)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn call_multipart<TResult>(
        &self,
        function_name: &str,
        form: reqwest::multipart::Form,
    ) -> Result<TResult, ForgeClientError>
    where
        TResult: DeserializeOwned,
    {
        let correlation_id = self.correlation_id();
        let envelope = platform::request_multipart(
            self,
            &format!("{}/_api/rpc/{}/upload", self.inner.url, function_name),
            form,
            correlation_id.as_deref(),
        )
        .await?;
        self.decode_envelope(envelope)
    }

    pub fn subscribe_query<TArgs, TResult, F>(
        &self,
        function_name: &str,
        args: TArgs,
        callback: F,
    ) -> SubscriptionHandle
    where
        TArgs: Serialize + Clone + 'static,
        TResult: DeserializeOwned + Clone + 'static,
        F: FnMut(StreamEvent<TResult>) + 'static,
    {
        let sub_id = self.random_id("sub");
        let target = format!("sub:{sub_id}");

        let (tx, rx) = futures_channel::mpsc::unbounded::<SseDispatch>();
        self.inner.sse.borrow_mut().listeners.insert(target.clone(), tx);

        let args_value = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let reg_payload = serde_json::json!({
            "id": sub_id,
            "function": function_name,
            "args": args_value,
        });
        self.inner.sse.borrow_mut().registrations.insert(
            sub_id.clone(),
            RegistrationMeta {
                endpoint: "/_api/subscribe",
                payload: reg_payload,
            },
        );

        self.spawn_subscription(sub_id, target, rx, callback, |client, envelope, cb| {
            match client.decode_envelope::<TResult>(envelope) {
                Ok(data) => cb(StreamEvent::Data(data)),
                Err(err) => cb(StreamEvent::Error(err)),
            }
        })
    }

    pub fn subscribe_job<TResult, F>(&self, job_id: String, callback: F) -> SubscriptionHandle
    where
        TResult: DeserializeOwned + Clone + 'static,
        F: FnMut(StreamEvent<TResult>) + 'static,
    {
        self.subscribe_tracker("job", serde_json::json!({ "job_id": job_id }), "/_api/subscribe-job", callback)
    }

    pub fn subscribe_workflow<TResult, F>(
        &self,
        workflow_id: String,
        callback: F,
    ) -> SubscriptionHandle
    where
        TResult: DeserializeOwned + Clone + 'static,
        F: FnMut(StreamEvent<TResult>) + 'static,
    {
        self.subscribe_tracker(
            "wf",
            serde_json::json!({ "workflow_id": workflow_id }),
            "/_api/subscribe-workflow",
            callback,
        )
    }

    fn subscribe_tracker<TResult, F>(
        &self,
        prefix: &str,
        payload: serde_json::Value,
        endpoint: &'static str,
        callback: F,
    ) -> SubscriptionHandle
    where
        TResult: DeserializeOwned + Clone + 'static,
        F: FnMut(StreamEvent<TResult>) + 'static,
    {
        let sub_id = self.random_id(prefix);
        let target = format!("{prefix}:{sub_id}");

        let (tx, rx) = futures_channel::mpsc::unbounded::<SseDispatch>();
        self.inner.sse.borrow_mut().listeners.insert(target.clone(), tx);

        let mut reg_payload = payload;
        reg_payload
            .as_object_mut()
            .expect("tracker payload must be an object")
            .insert("id".to_string(), serde_json::Value::String(sub_id.clone()));
        self.inner.sse.borrow_mut().registrations.insert(
            sub_id.clone(),
            RegistrationMeta {
                endpoint,
                payload: reg_payload,
            },
        );

        self.spawn_subscription(sub_id, target, rx, callback, |_client, envelope, cb| {
            if envelope.success {
                if let Some(data) = envelope.data {
                    match serde_json::from_value::<TResult>(data) {
                        Ok(parsed) => cb(StreamEvent::Data(parsed)),
                        Err(e) => cb(StreamEvent::Error(ForgeClientError::new(
                            "DESERIALIZATION_ERROR",
                            e.to_string(),
                            None,
                        ))),
                    }
                }
            }
        })
    }

    fn spawn_subscription<TResult, F>(
        &self,
        sub_id: String,
        target: String,
        mut rx: futures_channel::mpsc::UnboundedReceiver<SseDispatch>,
        mut callback: F,
        on_initial: impl FnOnce(&ForgeClient, RpcEnvelopeRaw, &mut F) + 'static,
    ) -> SubscriptionHandle
    where
        TResult: DeserializeOwned + Clone + 'static,
        F: FnMut(StreamEvent<TResult>) + 'static,
    {
        let client = self.clone();
        let handle = SubscriptionHandle::new(sub_id.clone(), target, self.clone());
        let handle_task = handle.clone();

        let task = dioxus::prelude::spawn(async move {
            callback(StreamEvent::Connection(ConnectionState::Connecting));

            if let Err(e) = client.ensure_connected().await {
                callback(StreamEvent::Error(e));
                callback(StreamEvent::Connection(ConnectionState::Disconnected));
                handle_task.finish();
                return;
            }

            match client.register_subscription(&sub_id).await {
                Ok(envelope) => {
                    callback(StreamEvent::Connection(ConnectionState::Connected));
                    on_initial(&client, envelope, &mut callback);
                }
                Err(err) => {
                    callback(StreamEvent::Error(err));
                    callback(StreamEvent::Connection(ConnectionState::Disconnected));
                    handle_task.finish();
                    return;
                }
            }

            while let Some(event) = futures_util::StreamExt::next(&mut rx).await {
                Self::deliver_event::<TResult, F>(&mut callback, &client, event);
            }

            handle_task.finish();
        });

        handle.set_task(task);
        handle
    }

    fn deliver_event<TResult, F>(
        callback: &mut F,
        client: &ForgeClient,
        event: SseDispatch,
    ) where
        TResult: DeserializeOwned,
        F: FnMut(StreamEvent<TResult>),
    {
        match event {
            SseDispatch::Data(value) => match serde_json::from_value::<TResult>(value) {
                Ok(data) => callback(StreamEvent::Data(data)),
                Err(e) => {
                    callback(StreamEvent::Error(ForgeClientError::new(
                        "DESERIALIZATION_ERROR",
                        e.to_string(),
                        None,
                    )));
                }
            },
            SseDispatch::Error { code, message } => {
                let err = ForgeClientError::new(&code, &message, None);
                if code == "UNAUTHORIZED" {
                    if let Some(handler) = &client.inner.on_auth_error {
                        handler(err.as_forge_error());
                    }
                }
                callback(StreamEvent::Error(err));
            }
        }
    }

    /// Ensure the shared SSE connection is established. Spawns the event loop on first call.
    async fn ensure_connected(&self) -> Result<(), ForgeClientError> {
        let rx = {
            let mut sse = self.inner.sse.borrow_mut();
            if sse.state == SseState::Connected {
                return Ok(());
            }

            let (tx, rx) = futures_channel::oneshot::channel();
            sse.connect_waiters.push(tx);

            if sse.state == SseState::Idle {
                sse.state = SseState::Connecting;
                drop(sse);
                platform::start_event_loop(self.clone());
            }

            rx
        };

        rx.await.unwrap_or_else(|_| {
            Err(ForgeClientError::new(
                "SSE_CONNECTION_FAILED",
                "Connection attempt cancelled",
                None,
            ))
        })
    }

    /// Register a subscription with the server via POST.
    /// If the server returns SESSION_NOT_FOUND, forces a reconnect and retries once.
    async fn register_subscription(
        &self,
        sub_id: &str,
    ) -> Result<RpcEnvelopeRaw, ForgeClientError> {
        let envelope = self.try_register_subscription(sub_id).await?;

        // Stale session: force reconnect and retry once
        let needs_retry = !envelope.success
            && envelope
                .error
                .as_ref()
                .is_some_and(|e| e.code == "SESSION_NOT_FOUND" || e.code == "SESSION_PRINCIPAL_MISMATCH");

        if needs_retry {
            self.force_reconnect().await;
            self.ensure_connected().await?;
            let retried = self.try_register_subscription(sub_id).await?;
            self.notify_auth_error_if_needed(&retried);
            return Ok(retried);
        }

        self.notify_auth_error_if_needed(&envelope);
        Ok(envelope)
    }

    fn notify_auth_error_if_needed(&self, envelope: &RpcEnvelopeRaw) {
        if let Some(err) = envelope.error.as_ref().filter(|_| !envelope.success) {
            if (err.code == "UNAUTHORIZED" || err.code == "FORBIDDEN")
                && let Some(handler) = &self.inner.on_auth_error
            {
                handler(err.clone());
            }
        }
    }

    async fn try_register_subscription(
        &self,
        sub_id: &str,
    ) -> Result<RpcEnvelopeRaw, ForgeClientError> {
        let (endpoint, payload) = {
            let sse = self.inner.sse.borrow();
            let meta = sse
                .registrations
                .get(sub_id)
                .ok_or_else(|| {
                    ForgeClientError::new("INTERNAL_ERROR", "Registration metadata not found", None)
                })?;
            let session_id = sse.session_id.clone().unwrap_or_default();
            let session_secret = sse.session_secret.clone().unwrap_or_default();
            let mut payload = meta.payload.clone();
            let obj = payload
                .as_object_mut()
                .expect("registration payload must be an object");
            obj.insert("session_id".into(), serde_json::Value::String(session_id));
            obj.insert("session_secret".into(), serde_json::Value::String(session_secret));
            (meta.endpoint, payload)
        };

        let url = format!("{}{}", self.inner.url, endpoint);
        platform::request_json(self, &url, payload, None).await
    }

    async fn force_reconnect(&self) {
        let task = {
            let mut sse = self.inner.sse.borrow_mut();
            sse.session_id = None;
            sse.session_secret = None;
            sse.state = SseState::Idle;
            // No need to drain waiters; force_reconnect is only called mid-registration
            // where the caller already passed ensure_connected
            sse.event_loop_task.take()
        };
        if let Some(task) = task {
            task.cancel();
        }
        sleep(Duration::from_millis(10)).await;
    }

    /// Tear down the current SSE connection and start a fresh one.
    ///
    /// The new connection calls `get_token()` again, picking up any tokens
    /// that were updated since the original connection was established.
    /// Existing subscriptions are automatically re-registered once the new
    /// connection's "connected" handshake completes.
    pub fn reconnect_sse(&self) {
        let has_listeners = {
            let mut sse = self.inner.sse.borrow_mut();
            if sse.state == SseState::Idle && sse.event_loop_task.is_none() && sse.listeners.is_empty() {
                return;
            }
            // Already tearing down and reconnecting, don't stack another one
            if sse.state == SseState::Connecting && sse.event_loop_task.is_some() {
                return;
            }
            if let Some(task) = sse.event_loop_task.take() {
                task.cancel();
            }
            sse.session_id = None;
            sse.session_secret = None;
            sse.reconnect_attempts = 0;
            let has_listeners = !sse.listeners.is_empty();
            sse.state = if has_listeners {
                SseState::Connecting
            } else {
                SseState::Idle
            };
            has_listeners
        };
        if has_listeners {
            platform::start_event_loop(self.clone());
        }
    }

    async fn reregister_all(&self) {
        let sub_ids: Vec<String> = {
            let sse = self.inner.sse.borrow();
            sse.registrations.keys().cloned().collect()
        };

        for sub_id in sub_ids {
            let _ = self.register_subscription(&sub_id).await;
        }
    }

    fn dispatch_event(&self, target: &str, event: SseDispatch) {
        let tx = {
            let sse = self.inner.sse.borrow();
            sse.listeners.get(target).cloned()
        };
        if let Some(tx) = tx {
            let _ = tx.unbounded_send(event);
        }
    }

    fn broadcast_connection(&self, state: ConnectionState) {
        if let Some(mut signal) = self.inner.connection_state {
            signal.set(state);
        }
    }

    fn mark_connected(&self, session_id: String, session_secret: String) -> bool {
        let mut sse = self.inner.sse.borrow_mut();
        let is_reconnect = sse.ever_connected;
        sse.session_id = Some(session_id);
        sse.session_secret = Some(session_secret);
        sse.state = SseState::Connected;
        sse.reconnect_attempts = 0;
        sse.ever_connected = true;
        for waiter in sse.connect_waiters.drain(..) {
            let _ = waiter.send(Ok(()));
        }
        is_reconnect
    }

    fn mark_disconnected(&self) {
        let mut sse = self.inner.sse.borrow_mut();
        sse.session_id = None;
        sse.session_secret = None;
        sse.state = SseState::Idle;
        sse.event_loop_task = None;
        let err = || ForgeClientError::new("SSE_CONNECTION_FAILED", "SSE connection lost", None);
        for waiter in sse.connect_waiters.drain(..) {
            let _ = waiter.send(Err(err()));
        }
    }

    /// Returns the current attempt count for backoff calculation. Retries
    /// indefinitely while there are listeners — long-lived apps need an
    /// always-on SSE pipe, not a hard 10-attempt giveup. Backoff is capped
    /// by the caller via `attempts.min(N)`.
    fn should_reconnect(&self) -> Option<u32> {
        let mut sse = self.inner.sse.borrow_mut();
        if sse.listeners.is_empty() {
            return None;
        }
        let attempts = sse.reconnect_attempts;
        sse.reconnect_attempts = attempts.saturating_add(1);
        Some(attempts)
    }

    fn get_token(&self) -> Option<String> {
        self.inner
            .get_token
            .as_ref()
            .and_then(|provider| provider())
            .filter(|t| !t.is_empty())
    }

    /// Crate-internal accessor for the current access token, used by signals
    /// so analytics calls carry the user's identity.
    pub(crate) fn auth_token(&self) -> Option<String> {
        self.get_token()
    }

    fn decode_envelope<TResult>(
        &self,
        envelope: RpcEnvelopeRaw,
    ) -> Result<TResult, ForgeClientError>
    where
        TResult: DeserializeOwned,
    {
        if !envelope.success {
            let error = envelope.error.unwrap_or(ForgeError {
                code: "UNKNOWN".to_string(),
                message: "Unknown error".to_string(),
                retry_after_secs: None,
                details: None,
            });
            if error.code == "UNAUTHORIZED" || error.code == "FORBIDDEN" {
                if let Some(handler) = &self.inner.on_auth_error {
                    handler(error.clone());
                }
            }
            return Err(ForgeClientError::from_forge_error(error));
        }

        let data = envelope.data.ok_or_else(|| {
            ForgeClientError::new("EMPTY_RESPONSE", "Server returned no data", None)
        })?;
        serde_json::from_value(data)
            .map_err(|err| ForgeClientError::new("DESERIALIZATION_ERROR", err.to_string(), None))
    }

    fn random_id(&self, prefix: &str) -> String {
        let id = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{id}")
    }
}

fn envelope_is_unauthorized(envelope: &RpcEnvelopeRaw) -> bool {
    !envelope.success
        && envelope
            .error
            .as_ref()
            .is_some_and(|e| e.code == "UNAUTHORIZED")
}

async fn sleep(duration: Duration) {
    #[cfg(target_arch = "wasm32")]
    {
        gloo_timers::future::sleep(duration).await;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::time::sleep(duration).await;
    }
}

#[derive(Clone)]
pub struct SubscriptionHandle {
    closed: Rc<Cell<bool>>,
    task: Rc<RefCell<Option<Task>>>,
    cleanup: Rc<RefCell<Option<Box<dyn FnOnce()>>>>,
}

impl SubscriptionHandle {
    fn new(sub_id: String, target: String, client: ForgeClient) -> Self {
        let cleanup: Box<dyn FnOnce()> = Box::new(move || {
            let mut sse = client.inner.sse.borrow_mut();
            sse.listeners.remove(&target);
            sse.registrations.remove(&sub_id);
            // Jobs/workflows have server-managed lifecycles; only query subs need explicit unsubscribe
            if target.starts_with("sub:") {
                let session_id = sse.session_id.clone();
                let session_secret = sse.session_secret.clone();
                drop(sse);
                if let (Some(sid), Some(ss)) = (session_id, session_secret) {
                    let url = format!("{}/_api/unsubscribe", client.inner.url);
                    let payload = serde_json::json!({
                        "session_id": sid,
                        "session_secret": ss,
                        "id": sub_id,
                    });
                    let client = client.clone();
                    dioxus::prelude::spawn(async move {
                        let _ = platform::request_json(&client, &url, payload, None).await;
                    });
                }
            }
        });

        Self {
            closed: Rc::new(Cell::new(false)),
            task: Rc::new(RefCell::new(None)),
            cleanup: Rc::new(RefCell::new(Some(cleanup))),
        }
    }

    fn set_task(&self, task: Task) {
        *self.task.borrow_mut() = Some(task);
    }

    pub(crate) fn finish(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Some(cleanup) = self.cleanup.borrow_mut().take() {
            cleanup();
        }
        self.task.borrow_mut().take();
    }

    pub fn close(&self) {
        let task = { self.task.borrow_mut().clone() };
        self.finish();
        if let Some(task) = task {
            task.cancel();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(target_arch = "wasm32")]
mod platform {
    use dioxus::prelude::spawn;
    use futures_util::{StreamExt, stream};
    use gloo_net::eventsource::futures::EventSource;
    use gloo_net::http::Request;
    use js_sys::{JSON, encode_uri_component};

    use super::{ForgeClient, SseDispatch, sleep};
    use crate::signals::platform_tag;
    use crate::types::{
        ConnectedEvent, ConnectionState, ForgeClientError, RpcEnvelopeRaw, SseEnvelopeRaw,
    };

    pub(super) async fn request_json(
        client: &ForgeClient,
        url: &str,
        body: serde_json::Value,
        correlation_id: Option<&str>,
    ) -> Result<RpcEnvelopeRaw, ForgeClientError> {
        // X-Forge-CSRF: custom header forces a CORS preflight on cross-origin
        // POSTs so the server's CORS allowlist gates cross-site requests
        // despite `credentials: include`.
        let mut request = Request::post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/vnd.forge.v1+json")
            .header("x-forge-platform", platform_tag())
            .header("X-Forge-CSRF", "1")
            .credentials(web_sys::RequestCredentials::Include);
        if let Some(token) = client.get_token() {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        if let Some(cid) = correlation_id {
            request = request.header("x-correlation-id", cid);
        }

        let request = request.body(body.to_string()).map_err(request_error)?;
        request
            .send()
            .await
            .map_err(request_error)?
            .json()
            .await
            .map_err(request_error)
    }

    pub(super) async fn request_multipart(
        client: &ForgeClient,
        url: &str,
        form: web_sys::FormData,
        correlation_id: Option<&str>,
    ) -> Result<RpcEnvelopeRaw, ForgeClientError> {
        let mut request = Request::post(url)
            .header("x-forge-platform", platform_tag())
            .header("X-Forge-CSRF", "1")
            .credentials(web_sys::RequestCredentials::Include);
        if let Some(token) = client.get_token() {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        if let Some(cid) = correlation_id {
            request = request.header("x-correlation-id", cid);
        }

        let response = request.body(form).map_err(request_error)?;
        response
            .send()
            .await
            .map_err(request_error)?
            .json()
            .await
            .map_err(request_error)
    }

    fn message_data_as_string(message: &web_sys::MessageEvent) -> Option<String> {
        let data = message.data();
        data.as_string().or_else(|| {
            JSON::stringify(&data)
                .ok()
                .and_then(|value| value.as_string())
                .map(|raw| serde_json::from_str::<String>(&raw).unwrap_or(raw))
        })
    }

    /// Build the SSE URL. When a bearer token is present, mint a short-lived
    /// single-use ticket via `POST /_api/events/ticket` and put it in the
    /// query string. The JWT itself never appears in the URL — query
    /// strings leak into access logs, browser history, and Referer headers.
    /// Anonymous connections skip the ticket fetch.
    async fn events_url(client: &ForgeClient) -> String {
        let base = format!("{}/_api/events", client.inner.url);
        let Some(token) = client.get_token() else {
            return base;
        };
        let ticket_url = format!("{}/ticket", base);
        let request = Request::post(&ticket_url).header("Authorization", &format!("Bearer {token}"));
        let response = match request.send().await {
            Ok(r) => r,
            Err(_) => return base,
        };
        if !response.ok() {
            return base;
        }
        #[derive(serde::Deserialize)]
        struct TicketResponse {
            ticket: String,
        }
        match response.json::<TicketResponse>().await {
            Ok(body) => format!("{}?ticket={}", base, encode_uri_component(&body.ticket)),
            Err(_) => base,
        }
    }

    /// Start the single shared SSE event loop.
    pub(super) fn start_event_loop(client: ForgeClient) {
        let client_for_task = client.clone();
        let task = spawn(async move {
            let was_connected = run_event_loop(&client_for_task).await;

            client_for_task.mark_disconnected();
            client_for_task.broadcast_connection(ConnectionState::Disconnected);

            // Never reached "connected" handshake while holding a token:
            // almost certainly a 401. Trigger refresh instead of retrying
            // with the same expired token.
            if !was_connected && client_for_task.get_token().is_some() {
                if let Some(handler) = &client_for_task.inner.on_auth_error {
                    handler(crate::types::ForgeError {
                        code: "UNAUTHORIZED".into(),
                        message: "SSE authentication failed".into(),
                        retry_after_secs: None,
                        details: None,
                    });
                }
                return;
            }

            if let Some(attempts) = client_for_task.should_reconnect() {
                let delay = 1000 * (1u64 << attempts.min(4));
                let jitter = (js_sys::Math::random() * 500.0) as u64;
                sleep(std::time::Duration::from_millis(delay + jitter)).await;

                client_for_task.inner.sse.borrow_mut().state = super::SseState::Connecting;
                start_event_loop(client_for_task);
            }
        });

        client.inner.sse.borrow_mut().event_loop_task = Some(task);
    }

    /// Returns `true` if the connection was established at some point.
    async fn run_event_loop(client: &ForgeClient) -> bool {
        let url = events_url(client).await;
        let mut event_source = match EventSource::new(&url) {
            Ok(source) => source,
            Err(_) => {
                return false;
            }
        };

        let mut connected_stream = match event_source.subscribe("connected") {
            Ok(stream) => stream,
            Err(_) => return false,
        };
        let update_stream = match event_source.subscribe("update") {
            Ok(stream) => stream,
            Err(_) => return false,
        };
        let error_stream = match event_source.subscribe("error") {
            Ok(stream) => stream,
            Err(_) => return false,
        };
        let gap_stream = match event_source.subscribe("gap") {
            Ok(stream) => stream,
            Err(_) => return false,
        };
        let _channel_stream = match event_source.subscribe("channel") {
            Ok(stream) => stream,
            Err(_) => return false,
        };

        let connected_event = match connected_stream.next().await {
            Some(Ok((_kind, message))) => {
                let Some(raw) = message_data_as_string(&message) else {
                    return false;
                };
                match serde_json::from_str::<ConnectedEvent>(&raw) {
                    Ok(event) => event,
                    Err(_) => return false,
                }
            }
            _ => return false,
        };

        let session_id = connected_event.session_id.unwrap_or_default();
        let session_secret = connected_event.session_secret.unwrap_or_default();

        if session_id.is_empty() || session_secret.is_empty() {
            return false;
        }

        let is_reconnect = client.mark_connected(session_id, session_secret);
        client.broadcast_connection(ConnectionState::Connected);

        if is_reconnect {
            client.reregister_all().await;
        }

        let mut events = stream::select(stream::select(update_stream, error_stream), gap_stream);
        while let Some(event) = events.next().await {
            match event {
                Ok((kind, message)) => {
                    let Some(raw) = message_data_as_string(&message) else {
                        continue;
                    };
                    let Ok(envelope) = serde_json::from_str::<SseEnvelopeRaw>(&raw) else {
                        continue;
                    };

                    let Some(target) = envelope.target else {
                        continue;
                    };

                    if kind == "update" || kind == "gap" {
                        if let Some(payload) = envelope.payload {
                            client.dispatch_event(&target, SseDispatch::Data(payload));
                        }
                    } else if kind == "error" {
                        let code = envelope.code.unwrap_or_else(|| "SSE_ERROR".to_string());
                        let message = envelope.message.unwrap_or_else(|| "Subscription error".to_string());
                        client.dispatch_event(&target, SseDispatch::Error { code, message });
                    }
                }
                Err(_) => break,
            }
        }

        event_source.close();
        true
    }

    fn request_error(err: gloo_net::Error) -> ForgeClientError {
        ForgeClientError::new("REQUEST_FAILED", err.to_string(), None)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use dioxus::prelude::spawn;
    use futures_util::StreamExt;
    use reqwest::Client;
    use reqwest_eventsource::{Event, EventSource};

    use super::{ForgeClient, SseDispatch, sleep};
    use crate::signals::platform_tag;
    use crate::types::{
        ConnectedEvent, ConnectionState, ForgeClientError, RpcEnvelopeRaw, SseEnvelopeRaw,
    };

    pub(super) async fn request_json(
        client: &ForgeClient,
        url: &str,
        body: serde_json::Value,
        correlation_id: Option<&str>,
    ) -> Result<RpcEnvelopeRaw, ForgeClientError> {
        let mut request = Client::new()
            .post(url)
            .header("Accept", "application/vnd.forge.v1+json")
            .header("x-forge-platform", platform_tag())
            .header("X-Forge-CSRF", "1")
            .json(&body);
        if let Some(token) = client.get_token() {
            request = request.bearer_auth(token);
        }
        if let Some(cid) = correlation_id {
            request = request.header("x-correlation-id", cid);
        }

        request
            .send()
            .await
            .map_err(request_error)?
            .json()
            .await
            .map_err(request_error)
    }

    pub(super) async fn request_multipart(
        client: &ForgeClient,
        url: &str,
        form: reqwest::multipart::Form,
        correlation_id: Option<&str>,
    ) -> Result<RpcEnvelopeRaw, ForgeClientError> {
        let mut request = Client::new()
            .post(url)
            .header("x-forge-platform", platform_tag())
            .header("X-Forge-CSRF", "1")
            .multipart(form);
        if let Some(token) = client.get_token() {
            request = request.bearer_auth(token);
        }
        if let Some(cid) = correlation_id {
            request = request.header("x-correlation-id", cid);
        }

        request
            .send()
            .await
            .map_err(request_error)?
            .json()
            .await
            .map_err(request_error)
    }

    /// Start the single shared SSE event loop.
    pub(super) fn start_event_loop(client: ForgeClient) {
        let client_for_task = client.clone();
        let task = spawn(async move {
            let was_connected = run_event_loop(&client_for_task).await;

            client_for_task.mark_disconnected();
            client_for_task.broadcast_connection(ConnectionState::Disconnected);

            if !was_connected && client_for_task.get_token().is_some() {
                if let Some(handler) = &client_for_task.inner.on_auth_error {
                    handler(crate::types::ForgeError {
                        code: "UNAUTHORIZED".into(),
                        message: "SSE authentication failed".into(),
                        retry_after_secs: None,
                        details: None,
                    });
                }
                return;
            }

            if let Some(attempts) = client_for_task.should_reconnect() {
                let delay = 1000 * (1u64 << attempts.min(4));
                // Cheap jitter from wall-clock subnanos so two desktop apps
                // started off the same restart cycle don't synchronize retries.
                let jitter = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| (d.subsec_nanos() as u64) % 500)
                    .unwrap_or(0);
                sleep(std::time::Duration::from_millis(delay + jitter)).await;

                client_for_task.inner.sse.borrow_mut().state = super::SseState::Connecting;
                start_event_loop(client_for_task);
            }
        });

        client.inner.sse.borrow_mut().event_loop_task = Some(task);
    }

    /// Returns `true` if the connection was established at some point.
    async fn run_event_loop(client: &ForgeClient) -> bool {
        let mut request = Client::new().get(format!("{}/_api/events", client.inner.url));
        if let Some(token) = client.get_token() {
            request = request.bearer_auth(token);
        }

        let mut event_source = match EventSource::new(request) {
            Ok(source) => source,
            Err(_) => return false,
        };

        let connected_event = loop {
            let Some(event) = event_source.next().await else {
                return false;
            };
            match event {
                Ok(Event::Open) => continue,
                Ok(Event::Message(msg)) if msg.event == "connected" => {
                    match serde_json::from_str::<ConnectedEvent>(&msg.data) {
                        Ok(event) => break event,
                        Err(_) => return false,
                    }
                }
                Ok(Event::Message(_)) => continue,
                Err(_) => return false,
            }
        };

        let session_id = connected_event.session_id.unwrap_or_default();
        let session_secret = connected_event.session_secret.unwrap_or_default();

        if session_id.is_empty() || session_secret.is_empty() {
            return false;
        }

        let is_reconnect = client.mark_connected(session_id, session_secret);
        client.broadcast_connection(ConnectionState::Connected);

        if is_reconnect {
            client.reregister_all().await;
        }

        while let Some(event) = event_source.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(msg))
                    if msg.event == "update"
                        || msg.event == "error"
                        || msg.event == "gap"
                        || msg.event == "channel" =>
                {
                    let Ok(envelope) = serde_json::from_str::<SseEnvelopeRaw>(&msg.data) else {
                        continue;
                    };
                    let Some(target) = envelope.target else {
                        continue;
                    };

                    if msg.event == "update" || msg.event == "gap" {
                        if let Some(payload) = envelope.payload {
                            client.dispatch_event(&target, SseDispatch::Data(payload));
                        }
                    } else if msg.event == "error" {
                        let code = envelope.code.unwrap_or_else(|| "SSE_ERROR".to_string());
                        let message =
                            envelope.message.unwrap_or_else(|| "Subscription error".to_string());
                        client.dispatch_event(&target, SseDispatch::Error { code, message });
                    }
                }
                Ok(Event::Message(_)) => {}
                Err(_) => break,
            }
        }

        event_source.close();
        true
    }

    fn request_error(err: reqwest::Error) -> ForgeClientError {
        ForgeClientError::new("REQUEST_FAILED", err.to_string(), None)
    }
}

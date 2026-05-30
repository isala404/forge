use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::error::HarnessError;

/// A typed SSE event from the gateway's `GET /_api/events` stream.
#[derive(Debug, Clone)]
pub enum SseEvent {
    /// Initial event sent immediately after connecting. Carries the
    /// `session_id` + `session_secret` pair the client uses to claim
    /// subscriptions.
    Connected {
        session_id: String,
        session_secret: String,
    },
    /// A reactor push for a specific subscription target.
    Update {
        target: String,
        payload: serde_json::Value,
    },
    /// An error attached to a subscription target.
    Error {
        target: String,
        code: String,
        message: String,
    },
    /// Any event whose `event:` name didn't match the four known ones above
    /// (keepalives, future event kinds, …). Tests usually skip these.
    Other { name: String, data: String },
}

/// A long-lived SSE session against the gateway. Wraps the EventSource stream
/// and exposes `subscribe` / `next_event` / `next_update_for` for the patterns
/// scenario tests use most.
pub struct HarnessSession {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
    session_id: String,
    session_secret: String,
    events: Mutex<EventStream>,
    /// Per-target backlog so events the test doesn't currently care about
    /// aren't silently dropped. Keys are the wire-level targets ("sub:foo",
    /// "job:abc", "wf:xyz"). A test that waits on "job:a" first and then
    /// "wf:b" sees the "wf:b" push even if it arrived during the "job:a" wait.
    buffered: Mutex<HashMap<String, VecDeque<BufferedEvent>>>,
}

#[derive(Debug, Clone)]
enum BufferedEvent {
    Update(serde_json::Value),
    Error { code: String, message: String },
}

type EventStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<SseEvent, HarnessError>> + Send>>;

impl HarnessSession {
    /// Open the SSE channel and read the initial `connected` event so the
    /// session_id/secret are available to subscribe immediately.
    pub(crate) async fn open(
        http: reqwest::Client,
        base_url: String,
        token: Option<String>,
    ) -> Result<Self, HarnessError> {
        // Auth via Authorization header rather than ?token=…, so a regression
        // that closes the query-string loophole on the server side doesn't
        // falsely fail every harness session test.
        let url = format!("{base_url}/_api/events");
        let mut req = http.get(&url).header("Accept", "text/event-stream");
        if let Some(t) = &token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(HarnessError::sse(format!(
                "SSE connect failed: status={}",
                resp.status()
            )));
        }

        let mut stream = resp.bytes_stream().eventsource();

        let first = timeout(Duration::from_secs(5), stream.next())
            .await
            .map_err(|_| HarnessError::timeout("connected"))?
            .ok_or_else(|| HarnessError::sse("SSE stream ended before connected event"))?
            .map_err(|e| HarnessError::sse(e.to_string()))?;

        let (session_id, session_secret) = match parse_sse_event(&first) {
            SseEvent::Connected {
                session_id,
                session_secret,
            } => (session_id, session_secret),
            other => {
                return Err(HarnessError::sse(format!(
                    "expected `connected`, got {other:?}",
                )));
            }
        };

        let events: EventStream = Box::pin(stream.map(|res| {
            res.map(|ev| parse_sse_event(&ev))
                .map_err(|e| HarnessError::sse(e.to_string()))
        }));

        Ok(Self {
            http,
            base_url,
            token,
            session_id,
            session_secret,
            events: Mutex::new(events),
            buffered: Mutex::new(HashMap::new()),
        })
    }

    /// Session ID issued by the gateway.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Session secret. Required for subscribe/unsubscribe RPCs.
    pub fn session_secret(&self) -> &str {
        &self.session_secret
    }

    /// Subscribe this session to a query. The returned JSON value is the
    /// first snapshot the gateway computed synchronously inside the subscribe
    /// call — equivalent to `data` in the SvelteKit response.
    pub async fn subscribe(
        &self,
        id: &str,
        function: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, HarnessError> {
        let mut req = self
            .http
            .post(format!("{}/_api/subscribe", self.base_url))
            .header("Content-Type", "application/json")
            .json(&json!({
                "session_id": &self.session_id,
                "session_secret": &self.session_secret,
                "id": id,
                "function": function,
                "args": args,
            }));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            return Err(HarnessError::sse(format!(
                "subscribe failed: status={status} body={body}",
            )));
        }
        let success = body
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !success {
            return Err(HarnessError::sse(format!("subscribe rejected: {body}")));
        }
        Ok(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Unsubscribe from a single target.
    pub async fn unsubscribe(&self, id: &str) -> Result<(), HarnessError> {
        let mut req = self
            .http
            .post(format!("{}/_api/unsubscribe", self.base_url))
            .header("Content-Type", "application/json")
            .json(&json!({
                "session_id": &self.session_id,
                "session_secret": &self.session_secret,
                "id": id,
            }));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
            return Err(HarnessError::sse(format!(
                "unsubscribe failed: status={status} body={body}"
            )));
        }
        Ok(())
    }

    /// Subscribe to a running job for progress + completion events. Returns
    /// the snapshot the gateway captured at subscribe time.
    pub async fn subscribe_job(
        &self,
        id: &str,
        job_id: &str,
    ) -> Result<serde_json::Value, HarnessError> {
        let mut req = self
            .http
            .post(format!("{}/_api/subscribe-job", self.base_url))
            .header("Content-Type", "application/json")
            .json(&json!({
                "session_id": &self.session_id,
                "session_secret": &self.session_secret,
                "id": id,
                "job_id": job_id,
            }));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            return Err(HarnessError::sse(format!(
                "subscribe-job failed: status={status} body={body}",
            )));
        }
        Ok(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Subscribe to a running workflow.
    pub async fn subscribe_workflow(
        &self,
        id: &str,
        workflow_id: &str,
    ) -> Result<serde_json::Value, HarnessError> {
        let mut req = self
            .http
            .post(format!("{}/_api/subscribe-workflow", self.base_url))
            .header("Content-Type", "application/json")
            .json(&json!({
                "session_id": &self.session_id,
                "session_secret": &self.session_secret,
                "id": id,
                "workflow_id": workflow_id,
            }));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            return Err(HarnessError::sse(format!(
                "subscribe-workflow failed: status={status} body={body}",
            )));
        }
        Ok(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Explicitly close the SSE stream, releasing the reqwest connection and
    /// signaling the gateway to drop the session.
    ///
    /// Called automatically by `Drop`, but tests that open many sessions per
    /// test can invoke this proactively to keep the gateway's session table
    /// small. Idempotent — safe to call multiple times.
    pub async fn close(&self) {
        // Replacing the stream with an empty one drops the underlying reqwest
        // body and the gateway sees the TCP connection close. We don't await
        // the gateway's cleanup; the SessionServer evicts the row on disconnect.
        let mut events = self.events.lock().await;
        *events = Box::pin(futures_util::stream::empty());
    }

    /// Read the next SSE event from the stream within the given budget.
    ///
    /// The lock is released between events rather than held for the full
    /// timeout, so concurrent tasks sharing a session can make progress.
    pub async fn next_event(&self, within: Duration) -> Result<SseEvent, HarnessError> {
        // Lock just long enough to poll the stream once; releasing it
        // between polls lets another task interleave.
        let poll = {
            let mut events = self.events.lock().await;
            timeout(within, events.next()).await
        };
        match poll {
            Ok(Some(Ok(ev))) => Ok(ev),
            Ok(Some(Err(e))) => Err(e),
            Ok(None) => Err(HarnessError::sse("SSE stream ended")),
            Err(_) => Err(HarnessError::timeout("sse event")),
        }
    }

    /// Read events until we see an `Update` for the given target. Events for
    /// other targets are buffered (per-target FIFO) so a subsequent call for
    /// a different target still sees pushes that arrived during this wait.
    pub async fn next_update_for(
        &self,
        target: &str,
        within: Duration,
    ) -> Result<serde_json::Value, HarnessError> {
        // First, drain any buffered event for this target.
        if let Some(ev) = self.pop_buffered(target).await {
            return match ev {
                BufferedEvent::Update(p) => Ok(p),
                BufferedEvent::Error { code, message } => Err(HarnessError::sse(format!(
                    "error for target {target}: {code} {message}"
                ))),
            };
        }

        let deadline = tokio::time::Instant::now() + within;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .ok_or_else(|| HarnessError::timeout(format!("update for {target}")))?;
            match self.next_event(remaining).await? {
                SseEvent::Update { target: t, payload } => {
                    if t == target {
                        return Ok(payload);
                    }
                    self.push_buffered(t, BufferedEvent::Update(payload)).await;
                }
                SseEvent::Error {
                    target: t,
                    code,
                    message,
                } => {
                    if t == target {
                        return Err(HarnessError::sse(format!(
                            "error for target {t}: {code} {message}"
                        )));
                    }
                    self.push_buffered(t, BufferedEvent::Error { code, message })
                        .await;
                }
                _ => continue,
            }
        }
    }

    async fn pop_buffered(&self, target: &str) -> Option<BufferedEvent> {
        let mut buf = self.buffered.lock().await;
        let q = buf.get_mut(target)?;
        q.pop_front()
    }

    async fn push_buffered(&self, target: String, ev: BufferedEvent) {
        let mut buf = self.buffered.lock().await;
        buf.entry(target).or_default().push_back(ev);
    }

    /// Wait for a reactor push to the query subscription `id` — the id passed
    /// to [`HarnessSession::subscribe`]. Hides the wire-level `sub:` target
    /// prefix the gateway adds to query updates.
    pub async fn next_query_update(
        &self,
        id: &str,
        within: Duration,
    ) -> Result<serde_json::Value, HarnessError> {
        self.next_update_for(&format!("sub:{id}"), within).await
    }

    /// Wait for a job progress or terminal event for the job subscription
    /// `id` — the id passed to [`HarnessSession::subscribe_job`].
    pub async fn next_job_update(
        &self,
        id: &str,
        within: Duration,
    ) -> Result<serde_json::Value, HarnessError> {
        self.next_update_for(&format!("job:{id}"), within).await
    }

    /// Wait for a workflow state event for the workflow subscription `id` —
    /// the id passed to [`HarnessSession::subscribe_workflow`].
    pub async fn next_workflow_update(
        &self,
        id: &str,
        within: Duration,
    ) -> Result<serde_json::Value, HarnessError> {
        self.next_update_for(&format!("wf:{id}"), within).await
    }
}

impl Drop for HarnessSession {
    /// Best-effort close: replace the stream with an empty one so the
    /// reqwest body and underlying TCP connection are dropped synchronously.
    /// The gateway's SessionServer reaps the row on the next cleanup pass.
    fn drop(&mut self) {
        // Drain a blocking try_lock if available; if a task still holds the
        // events mutex, the stream will be dropped when that task releases
        // it. We don't .await here — Drop is sync.
        if let Ok(mut events) = self.events.try_lock() {
            *events = Box::pin(futures_util::stream::empty());
        }
    }
}

fn parse_sse_event(ev: &eventsource_stream::Event) -> SseEvent {
    match ev.event.as_str() {
        "connected" => {
            #[derive(serde::Deserialize)]
            struct Connected {
                session_id: String,
                session_secret: String,
            }
            if let Ok(c) = serde_json::from_str::<Connected>(&ev.data) {
                return SseEvent::Connected {
                    session_id: c.session_id,
                    session_secret: c.session_secret,
                };
            }
        }
        "update" => {
            #[derive(serde::Deserialize)]
            struct Update {
                target: String,
                payload: serde_json::Value,
            }
            if let Ok(u) = serde_json::from_str::<Update>(&ev.data) {
                return SseEvent::Update {
                    target: u.target,
                    payload: u.payload,
                };
            }
        }
        "error" => {
            #[derive(serde::Deserialize)]
            struct Error {
                target: String,
                code: String,
                message: String,
            }
            if let Ok(e) = serde_json::from_str::<Error>(&ev.data) {
                return SseEvent::Error {
                    target: e.target,
                    code: e.code,
                    message: e.message,
                };
            }
        }
        _ => {}
    }
    SseEvent::Other {
        name: ev.event.clone(),
        data: ev.data.clone(),
    }
}

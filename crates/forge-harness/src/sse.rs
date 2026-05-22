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
        let url = if let Some(t) = &token {
            format!("{base_url}/_api/events?token={t}")
        } else {
            format!("{base_url}/_api/events")
        };

        let resp = http
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
            .await?;
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

    /// Read the next SSE event from the stream within the given budget.
    pub async fn next_event(&self, within: Duration) -> Result<SseEvent, HarnessError> {
        let mut events = self.events.lock().await;
        match timeout(within, events.next()).await {
            Ok(Some(Ok(ev))) => Ok(ev),
            Ok(Some(Err(e))) => Err(e),
            Ok(None) => Err(HarnessError::sse("SSE stream ended")),
            Err(_) => Err(HarnessError::timeout("sse event")),
        }
    }

    /// Read events until we see an `Update` for the given target. Other
    /// events are buffered in the stream order is preserved on the next
    /// `next_event` call (we drop them). Use this in tests that only care
    /// about a specific subscription's payload.
    pub async fn next_update_for(
        &self,
        target: &str,
        within: Duration,
    ) -> Result<serde_json::Value, HarnessError> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .ok_or_else(|| HarnessError::timeout(format!("update for {target}")))?;
            match self.next_event(remaining).await? {
                SseEvent::Update { target: t, payload } if t == target => return Ok(payload),
                SseEvent::Error {
                    target: t,
                    code,
                    message,
                } if t == target => {
                    return Err(HarnessError::sse(format!(
                        "error for target {t}: {code} {message}"
                    )));
                }
                _ => continue,
            }
        }
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

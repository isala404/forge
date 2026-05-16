
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use dioxus::prelude::{ReadableExt, Signal, WritableExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ForgeClient;

/// Wire-format error from the Forge RPC layer.
/// Shape: `{ code, message, retry_after_secs?, details? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ForgeError {
    pub code: String,
    pub message: String,
    /// Seconds to wait before retrying. Set for `RATE_LIMITED` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ForgeError {
    pub fn is_rate_limited(&self) -> bool { self.code == "RATE_LIMITED" }
    pub fn is_unauthorized(&self) -> bool { self.code == "UNAUTHORIZED" }
    pub fn is_validation(&self) -> bool { self.code == "VALIDATION_ERROR" }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgeClientError {
    pub code: String,
    pub message: String,
    /// Seconds to wait before retrying. Set for `RATE_LIMITED` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ForgeClientError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<serde_json::Value>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retry_after_secs: None,
            details,
        }
    }

    pub(crate) fn from_forge_error(e: ForgeError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            retry_after_secs: e.retry_after_secs,
            details: e.details,
        }
    }

    pub fn is_rate_limited(&self) -> bool { self.code == "RATE_LIMITED" }
    pub fn is_unauthorized(&self) -> bool { self.code == "UNAUTHORIZED" }
    pub fn is_validation(&self) -> bool { self.code == "VALIDATION_ERROR" }

    pub fn as_forge_error(&self) -> ForgeError {
        ForgeError {
            code: self.code.clone(),
            message: self.message.clone(),
            retry_after_secs: self.retry_after_secs,
            details: self.details.clone(),
        }
    }

    /// Build a rate-limited error with the retry delay.
    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self {
            code: "RATE_LIMITED".into(),
            message: "Rate limit exceeded".into(),
            retry_after_secs: Some(retry_after_secs),
            details: None,
        }
    }
}

impl std::fmt::Display for ForgeClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ForgeClientError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryState<T> {
    pub loading: bool,
    pub data: Option<T>,
    pub error: Option<ForgeError>,
}

impl<T> Default for QueryState<T> {
    fn default() -> Self {
        Self {
            loading: true,
            data: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionState<T> {
    pub loading: bool,
    pub data: Option<T>,
    pub error: Option<ForgeError>,
    pub stale: bool,
    pub connection_state: ConnectionState,
}

impl<T> Default for SubscriptionState<T> {
    fn default() -> Self {
        Self {
            loading: true,
            data: None,
            error: None,
            stale: false,
            connection_state: ConnectionState::Disconnected,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Claimed,
    Running,
    Completed,
    Retry,
    Failed,
    DeadLetter,
    CancelRequested,
    Cancelled,
}

impl Default for JobStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobState<TOutput> {
    pub job_id: String,
    pub status: JobStatus,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub output: Option<TOutput>,
    pub error: Option<String>,
}

impl<TOutput> Default for JobState<TOutput> {
    fn default() -> Self {
        Self {
            job_id: String::new(),
            status: JobStatus::Pending,
            progress: None,
            message: None,
            output: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobExecutionState<TOutput> {
    pub loading: bool,
    pub connection_state: ConnectionState,
    pub state: JobState<TOutput>,
}

impl<TOutput> Default for JobExecutionState<TOutput> {
    fn default() -> Self {
        Self {
            loading: true,
            connection_state: ConnectionState::Disconnected,
            state: JobState::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Sleeping,
    Waiting,
    Completed,
    Failed,
}

impl Default for WorkflowStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowStepState {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowState<TOutput> {
    pub workflow_id: String,
    pub status: WorkflowStatus,
    pub step: Option<String>,
    pub waiting_for: Option<String>,
    pub steps: Vec<WorkflowStepState>,
    pub output: Option<TOutput>,
    pub error: Option<String>,
}

impl<TOutput> Default for WorkflowState<TOutput> {
    fn default() -> Self {
        Self {
            workflow_id: String::new(),
            status: WorkflowStatus::Pending,
            step: None,
            waiting_for: None,
            steps: Vec::new(),
            output: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowExecutionState<TOutput> {
    pub loading: bool,
    pub connection_state: ConnectionState,
    pub state: WorkflowState<TOutput>,
}

impl<TOutput> Default for WorkflowExecutionState<TOutput> {
    fn default() -> Self {
        Self {
            loading: true,
            connection_state: ConnectionState::Disconnected,
            state: WorkflowState::default(),
        }
    }
}

/// An access token + refresh token pair returned by auth endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
}

/// Mutation handle returned by `use_forge_mutation`. Clone into event handlers,
/// call `.call(args)` to execute.
#[derive(Clone)]
pub struct Mutation<A, R> {
    client: ForgeClient,
    function_name: &'static str,
    _phantom: PhantomData<fn(A) -> R>,
}

impl<A, R> Mutation<A, R>
where
    A: Serialize + 'static,
    R: DeserializeOwned + 'static,
{
    pub(crate) fn new(client: ForgeClient, function_name: &'static str) -> Self {
        Self {
            client,
            function_name,
            _phantom: PhantomData,
        }
    }

    pub async fn call(&self, args: A) -> Result<R, ForgeClientError> {
        self.client.call(self.function_name, args).await
    }

    /// Fire-and-forget: spawns the mutation and routes errors to the global
    /// mutation error handler registered on [`ForgeClient`].
    pub fn fire(&self, args: A) {
        let client = self.client.clone();
        let function_name = self.function_name;
        dioxus::prelude::spawn(async move {
            if let Err(err) = client.call::<A, R>(function_name, args).await {
                client.notify_mutation_error(err);
            }
        });
    }

    /// Fire-and-forget with a one-off error callback that overrides the global
    /// handler for this invocation.
    pub fn fire_with(&self, args: A, on_error: impl FnOnce(ForgeClientError) + 'static) {
        let client = self.client.clone();
        let function_name = self.function_name;
        dioxus::prelude::spawn(async move {
            if let Err(err) = client.call::<A, R>(function_name, args).await {
                on_error(err);
            }
        });
    }
}

pub(crate) struct PendingOptimistic<D> {
    pub(crate) snapshot: Option<D>,
    pub(crate) generation: u64,
}

type ApplyFn<D, A> = Rc<dyn Fn(&D, &A) -> D>;

/// Handle returned by [`use_optimistic`](crate::use_optimistic). Provides
/// `.fire()` that applies an optimistic transform immediately and `.data()`
/// that returns the derived view layering local patches over subscription data.
pub struct OptimisticMutation<A: 'static, R: 'static, D: 'static> {
    pub(crate) mutation: Mutation<A, R>,
    pub(crate) view: Signal<Option<D>>,
    pub(crate) apply: ApplyFn<D, A>,
    pub(crate) subscription: Signal<SubscriptionState<D>>,
    pub(crate) pending: Signal<Option<PendingOptimistic<D>>>,
}

impl<A, R, D> OptimisticMutation<A, R, D>
where
    A: Serialize + Clone + 'static,
    R: DeserializeOwned + 'static,
    D: Clone + 'static,
{
    /// The current data, with any pending optimistic patches applied.
    pub fn data(&self) -> Option<D> {
        self.view.read().clone()
    }

    /// Signal accessor for use in RSX.
    pub fn data_signal(&self) -> Signal<Option<D>> {
        self.view
    }

    /// Fire the mutation with the optimistic transform applied immediately.
    /// On SSE update the server data replaces the optimistic patch. On error
    /// the view reverts to the pre-mutation snapshot.
    pub fn fire(&self, args: A) {
        let mut view = self.view;
        let mut pending = self.pending;
        let subscription = self.subscription;

        let current_data = subscription.read().data.clone();
        let generation = pending
            .read()
            .as_ref()
            .map(|p| p.generation + 1)
            .unwrap_or(1);

        if let Some(ref data) = current_data {
            let optimistic = (self.apply)(data, &args);
            view.set(Some(optimistic));
        }

        pending.set(Some(PendingOptimistic {
            snapshot: current_data,
            generation,
        }));

        // TTL safety net: revert if SSE hasn't confirmed within 3 seconds
        let ttl_generation = generation;
        let mut ttl_pending = pending;
        let mut ttl_view = view;
        let ttl_subscription = subscription;
        dioxus::prelude::spawn(async move {
            crate::hooks::sleep(Duration::from_secs(3)).await;
            let still_pending = ttl_pending
                .read()
                .as_ref()
                .is_some_and(|p| p.generation == ttl_generation);
            if still_pending {
                ttl_view.set(ttl_subscription.read().data.clone());
                ttl_pending.set(None);
            }
        });

        // Send the actual mutation
        let client = self.mutation.client.clone();
        let function_name = self.mutation.function_name;
        dioxus::prelude::spawn(async move {
            if let Err(err) = client.call::<A, R>(function_name, args).await {
                let should_rollback = pending
                    .read()
                    .as_ref()
                    .is_some_and(|p| p.generation == generation);
                if should_rollback
                    && let Some(p) = pending.write().take()
                {
                    view.set(p.snapshot);
                }
                client.notify_mutation_error(err);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_error_as_forge_error_preserves_code_message_and_details() {
        let err = ForgeClientError::new(
            "VALIDATION",
            "Name is required",
            Some(json!({"field": "name"})),
        );

        assert_eq!(
            err.as_forge_error(),
            ForgeError {
                code: "VALIDATION".into(),
                message: "Name is required".into(),
                retry_after_secs: None,
                details: Some(json!({"field": "name"})),
            }
        );
    }

    #[test]
    fn subscription_state_default_is_loading_and_disconnected() {
        let state = SubscriptionState::<Vec<String>>::default();

        assert!(state.loading);
        assert_eq!(state.data, None);
        assert_eq!(state.error, None);
        assert!(!state.stale);
        assert_eq!(state.connection_state, ConnectionState::Disconnected);
    }

    #[test]
    fn job_and_workflow_status_serialize_in_snake_case() {
        assert_eq!(serde_json::to_string(&JobStatus::CancelRequested).unwrap(), "\"cancel_requested\"");
        assert_eq!(
            serde_json::to_string(&WorkflowStatus::Sleeping).unwrap(),
            "\"sleeping\""
        );
        assert_eq!(
            serde_json::to_string(&WorkflowStatus::Pending).unwrap(),
            "\"pending\""
        );
    }

    #[test]
    fn query_and_subscription_state_defaults_are_safe_for_initial_render() {
        let query = QueryState::<Vec<String>>::default();
        let subscription = SubscriptionState::<Vec<String>>::default();

        assert!(query.loading);
        assert!(query.data.is_none());
        assert!(query.error.is_none());

        assert!(subscription.loading);
        assert!(subscription.data.is_none());
        assert!(subscription.error.is_none());
        assert!(!subscription.stale);
        assert_eq!(subscription.connection_state, ConnectionState::Disconnected);
    }

    #[test]
    fn job_and_workflow_execution_state_defaults_start_disconnected() {
        let job = JobExecutionState::<serde_json::Value>::default();
        let workflow = WorkflowExecutionState::<serde_json::Value>::default();

        assert!(job.loading);
        assert_eq!(job.connection_state, ConnectionState::Disconnected);
        assert_eq!(job.state.status, JobStatus::Pending);

        assert!(workflow.loading);
        assert_eq!(workflow.connection_state, ConnectionState::Disconnected);
        assert_eq!(workflow.state.status, WorkflowStatus::Pending);
    }
}

#[derive(Debug, Clone)]
pub enum StreamEvent<T> {
    Connection(ConnectionState),
    Data(T),
    Error(ForgeClientError),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RpcEnvelopeRaw {
    pub success: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<ForgeError>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ConnectedEvent {
    pub session_id: Option<String>,
    pub session_secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SseEnvelopeRaw {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use dioxus::dioxus_core::use_drop;
use dioxus::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{
    ConnectionState, JobExecutionState, Mutation, QueryState, StreamEvent, SubscriptionHandle,
    SubscriptionState, WorkflowExecutionState, use_forge_client,
};
use crate::types::{OptimisticMutation, PendingOptimistic};

pub(crate) async fn sleep(duration: Duration) {
    #[cfg(target_arch = "wasm32")]
    {
        gloo_timers::future::sleep(duration).await;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::time::sleep(duration).await;
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct JobStartResponse {
    job_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct WorkflowStartResponse {
    workflow_id: String,
}

pub fn use_forge_query_signal<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> Signal<QueryState<TResult>>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    let client = use_forge_client();
    let state = use_signal(QueryState::<TResult>::default);
    let request_id = use_hook(|| Rc::new(Cell::new(0_u64)));

    use_effect(use_reactive!(|(args,)| {
        let client = client.clone();
        let mut state = state;
        let request_id = request_id.clone();
        let current_id = request_id.get() + 1;
        request_id.set(current_id);

        state.set(QueryState {
            loading: true,
            data: None,
            error: None,
        });

        spawn(async move {
            match client.call::<_, TResult>(function_name, args).await {
                Ok(data) if request_id.get() == current_id => {
                    state.set(QueryState {
                        loading: false,
                        data: Some(data),
                        error: None,
                    });
                }
                Err(err) if request_id.get() == current_id => {
                    state.set(QueryState {
                        loading: false,
                        data: None,
                        error: Some(err.as_forge_error()),
                    });
                }
                _ => {}
            }
        });
    }));

    state
}

pub fn use_forge_query<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> QueryState<TResult>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    use_forge_query_signal(function_name, args)()
}

pub fn use_forge_subscription_signal<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> Signal<SubscriptionState<TResult>>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    let client = use_forge_client();
    let state = use_signal(SubscriptionState::<TResult>::default);
    let handle = use_hook(|| Rc::new(RefCell::new(None::<SubscriptionHandle>)));
    let generation = use_hook(|| Rc::new(Cell::new(0_u64)));
    let pending_data = use_hook(|| Rc::new(RefCell::new(None::<TResult>)));
    let flush_scheduled = use_hook(|| Rc::new(Cell::new(false)));
    let has_received_data = use_hook(|| Rc::new(Cell::new(false)));
    let reconnect_attempts = use_hook(|| Rc::new(Cell::new(0_u32)));
    let reconnect_nonce = use_signal(|| 0_u64);
    let effect_handle = handle.clone();
    let reconnect_key = reconnect_nonce();

    use_effect(use_reactive!(|(args, reconnect_key)| {
        let is_reconnect = reconnect_key > 0;
        let current_generation = generation.get() + 1;
        generation.set(current_generation);

        if let Some(existing) = effect_handle.borrow_mut().take() {
            existing.close();
        }

        let mut state = state;
        let previous = state.peek().clone();
        pending_data.borrow_mut().take();
        flush_scheduled.set(false);
        let had_data = previous.data.is_some();
        has_received_data.set(had_data);

        // Retries happen silently to avoid re-render storms in desktop WebViews.
        if !is_reconnect {
            state.set(SubscriptionState {
                loading: !had_data,
                data: previous.data,
                error: None,
                stale: had_data,
                connection_state: ConnectionState::Connecting,
            });
        }

        let reconnect_generation = generation.clone();
        let reconnect_attempts = reconnect_attempts.clone();
        if !is_reconnect {
            reconnect_attempts.set(0);
        }
        let pending_data = pending_data.clone();
        let flush_scheduled = flush_scheduled.clone();
        let has_received_data = has_received_data.clone();

        let subscription = client.subscribe_query(function_name, args, move |event| match event {
            StreamEvent::Connection(connection_state) => {
                if connection_state == ConnectionState::Connected {
                    reconnect_attempts.set(0);
                    let mut next = state.peek().clone();
                    next.connection_state = ConnectionState::Connected;
                    next.stale = false;
                    state.set(next);
                }

                if connection_state == ConnectionState::Disconnected
                    && reconnect_generation.get() == current_generation
                {
                    let attempts = reconnect_attempts.get();
                    if attempts >= 10 {
                        let mut next = state.peek().clone();
                        next.loading = false;
                        next.connection_state = ConnectionState::Disconnected;
                        next.stale = true;
                        state.set(next);
                        return;
                    }
                    reconnect_attempts.set(attempts + 1);
                    let delay = 1000 * (1 << attempts.min(4));
                    let mut reconnect_nonce = reconnect_nonce;
                    spawn(async move {
                        sleep(Duration::from_millis(delay as u64)).await;
                        reconnect_nonce += 1;
                    });
                }
            }
            StreamEvent::Data(data) => {
                if !has_received_data.replace(true) {
                    let conn = state.peek().connection_state;
                    state.set(SubscriptionState {
                        loading: false,
                        data: Some(data),
                        error: None,
                        stale: false,
                        connection_state: conn,
                    });
                    return;
                }

                *pending_data.borrow_mut() = Some(data);
                if flush_scheduled.replace(true) {
                    return;
                }

                let pending_data = pending_data.clone();
                let flush_scheduled = flush_scheduled.clone();
                let mut state = state;
                spawn(async move {
                    sleep(Duration::from_millis(120)).await;
                    flush_scheduled.set(false);

                    let Some(data) = pending_data.borrow_mut().take() else {
                        return;
                    };

                    let conn = state.peek().connection_state;
                    state.set(SubscriptionState {
                        loading: false,
                        data: Some(data),
                        error: None,
                        stale: false,
                        connection_state: conn,
                    });
                });
            }
            StreamEvent::Error(err) => {
                // Suppress errors during reconnect attempts to avoid UI churn.
                let attempts = reconnect_attempts.get();
                if attempts > 0 && attempts < 10 {
                    return;
                }
                let mut next = state.peek().clone();
                next.loading = false;
                next.error = Some(err.as_forge_error());
                next.stale = true;
                state.set(next);
            }
        });

        *effect_handle.borrow_mut() = Some(subscription);
    }));

    use_drop({
        let handle = handle.clone();
        move || {
            if let Some(existing) = handle.borrow_mut().take() {
                existing.close();
            }
        }
    });

    state
}

pub fn use_forge_subscription<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> SubscriptionState<TResult>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    use_forge_subscription_signal(function_name, args)()
}

pub fn use_forge_mutation<TArgs, TResult>(
    function_name: &'static str,
) -> Mutation<TArgs, TResult>
where
    TArgs: Serialize + 'static,
    TResult: DeserializeOwned + 'static,
{
    let client = use_forge_client();
    Mutation::new(client, function_name)
}

pub fn use_forge_job_signal<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> Signal<JobExecutionState<TResult>>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    let client = use_forge_client();
    let state = use_signal(JobExecutionState::<TResult>::default);
    let handle = use_hook(|| Rc::new(RefCell::new(None::<SubscriptionHandle>)));
    let effect_handle = handle.clone();

    use_effect(use_reactive!(|(args,)| {
        if let Some(existing) = effect_handle.borrow_mut().take() {
            existing.close();
        }

        let client = client.clone();
        let handle = effect_handle.clone();
        let mut state = state;
        state.set(JobExecutionState::default());

        spawn(async move {
            match client
                .call::<_, JobStartResponse>(function_name, args)
                .await
            {
                Ok(started) => {
                    let subscription =
                        client.subscribe_job(started.job_id.clone(), move |event| match event {
                            StreamEvent::Connection(connection_state) => {
                                let mut next = state.peek().clone();
                                next.connection_state = connection_state;
                                state.set(next);
                            }
                            StreamEvent::Data(job_state) => {
                                let conn = state.peek().connection_state;
                                state.set(JobExecutionState {
                                    loading: false,
                                    connection_state: conn,
                                    state: job_state,
                                });
                            }
                            StreamEvent::Error(err) => {
                                let mut next = state.peek().clone();
                                next.loading = false;
                                next.state.error = Some(err.message);
                                state.set(next);
                            }
                        });
                    *handle.borrow_mut() = Some(subscription);
                }
                Err(err) => {
                    let mut next = state.peek().clone();
                    next.loading = false;
                    next.state.error = Some(err.message);
                    state.set(next);
                }
            }
        });
    }));

    use_drop({
        let handle = handle.clone();
        move || {
            if let Some(existing) = handle.borrow_mut().take() {
                existing.close();
            }
        }
    });

    state
}

pub fn use_forge_job<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> JobExecutionState<TResult>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    use_forge_job_signal(function_name, args)()
}

pub fn use_forge_workflow_signal<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> Signal<WorkflowExecutionState<TResult>>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    let client = use_forge_client();
    let state = use_signal(WorkflowExecutionState::<TResult>::default);
    let handle = use_hook(|| Rc::new(RefCell::new(None::<SubscriptionHandle>)));
    let effect_handle = handle.clone();

    use_effect(use_reactive!(|(args,)| {
        if let Some(existing) = effect_handle.borrow_mut().take() {
            existing.close();
        }

        let client = client.clone();
        let handle = effect_handle.clone();
        let mut state = state;
        state.set(WorkflowExecutionState::default());

        spawn(async move {
            match client
                .call::<_, WorkflowStartResponse>(function_name, args)
                .await
            {
                Ok(started) => {
                    let subscription =
                        client.subscribe_workflow(started.workflow_id.clone(), move |event| {
                            match event {
                                StreamEvent::Connection(connection_state) => {
                                    let mut next = state.peek().clone();
                                    next.connection_state = connection_state;
                                    state.set(next);
                                }
                                StreamEvent::Data(workflow_state) => {
                                    let conn = state.peek().connection_state;
                                    state.set(WorkflowExecutionState {
                                        loading: false,
                                        connection_state: conn,
                                        state: workflow_state,
                                    });
                                }
                                StreamEvent::Error(err) => {
                                    let mut next = state.peek().clone();
                                    next.loading = false;
                                    next.state.error = Some(err.message);
                                    state.set(next);
                                }
                            }
                        });
                    *handle.borrow_mut() = Some(subscription);
                }
                Err(err) => {
                    let mut next = state.peek().clone();
                    next.loading = false;
                    next.state.error = Some(err.message);
                    state.set(next);
                }
            }
        });
    }));

    use_drop({
        let handle = handle.clone();
        move || {
            if let Some(existing) = handle.borrow_mut().take() {
                existing.close();
            }
        }
    });

    state
}

pub fn use_forge_workflow<TArgs, TResult>(
    function_name: &'static str,
    args: TArgs,
) -> WorkflowExecutionState<TResult>
where
    TArgs: Serialize + Clone + PartialEq + 'static,
    TResult: DeserializeOwned + Clone + 'static,
{
    use_forge_workflow_signal(function_name, args)()
}

/// Create an optimistic mutation that layers local patches over a live
/// subscription. Returns an [`OptimisticMutation`] whose `.data()` reflects
/// the optimistic state and whose `.fire()` applies the transform, sends the
/// mutation, and auto-reverts on error or TTL expiry.
///
/// ```ignore
/// let reorder = use_optimistic(
///     use_reorder_task(),
///     use_list_tasks_subscription_signal(),
///     |tasks, args: &ReorderTaskInput| {
///         tasks.iter().map(|t| {
///             if t.id == args.id { Task { status: args.status, position: args.position, ..t.clone() } }
///             else { t.clone() }
///         }).collect()
///     },
/// );
/// // Read from reorder.data() instead of the raw subscription
/// // Call reorder.fire(args) for optimistic + server mutation
/// ```
pub fn use_optimistic<A, R, D>(
    mutation: Mutation<A, R>,
    subscription: Signal<SubscriptionState<D>>,
    apply: impl Fn(&D, &A) -> D + 'static,
) -> OptimisticMutation<A, R, D>
where
    A: Serialize + Clone + 'static,
    R: DeserializeOwned + 'static,
    D: Clone + PartialEq + 'static,
{
    let mut view: Signal<Option<D>> = use_signal(|| subscription.read().data.clone());
    let mut pending: Signal<Option<PendingOptimistic<D>>> = use_signal(|| None);
    let apply = use_hook(|| Rc::new(apply));

    // An incoming SSE push while a pending optimistic update exists is treated
    // as server confirmation: adopt server state and clear pending.
    use_effect(move || {
        let sub_data = subscription.read().data.clone();
        if pending.read().is_some() {
            pending.set(None);
        }
        view.set(sub_data);
    });

    OptimisticMutation {
        mutation,
        view,
        apply,
        subscription,
        pending,
    }
}


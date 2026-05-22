#![cfg(feature = "testcontainers")]
//! Workflow scenarios.
//!
//! A workflow dispatched over RPC must run on the worker, stream its state
//! transitions to every SSE subscriber, and — for a durable workflow — block
//! on an external event, survive the suspension, then resume to completion
//! once the event fires. This is the browserless proxy for "kick off a
//! multi-step pipeline, watch it advance, unblock it from another tab, see it
//! finish": same gateway, same executor, same scheduler, same `wf:` SSE wire
//! frames a browser client would consume.

mod common;

use std::time::Duration;

use common::{PipelineInput, PipelineOutput, WorkflowHandle, await_workflow_status, start_app};

/// Worker poll (50ms) + workflow scheduler poll (100ms) + step persistence, a
/// 400ms durable sleep, and reactor round-trips. Generous enough to absorb CI
/// scheduling noise without masking a hang.
const WF_BUDGET: Duration = Duration::from_secs(15);

/// The straight-line case: dispatch a three-step workflow, subscribe, and watch
/// the executor carry it to `completed` — with every step recorded, in
/// declaration order, and the input label round-tripped onto the output.
#[tokio::test]
async fn linear_workflow_runs_all_steps_to_completion() {
    let app = start_app("workflows_linear").await;
    let session = app.open_session(None).await.expect("open sse session");

    let handle: WorkflowHandle = app
        .client()
        .call(
            "harness_linear",
            PipelineInput {
                label: "alpha".into(),
            },
        )
        .await
        .expect("dispatch linear workflow");

    let snapshot = session
        .subscribe_workflow("wf", &handle.workflow_id)
        .await
        .expect("subscribe to workflow");

    let terminal =
        await_workflow_status(&session, "wf", &snapshot, &["completed"], WF_BUDGET).await;
    assert_eq!(
        terminal.get("status").and_then(serde_json::Value::as_str),
        Some("completed"),
        "linear workflow must finish in the completed state, saw: {terminal}",
    );

    // The output proves the input survived dispatch, persistence, and
    // execution — and that the handler body actually ran to its return.
    let output_raw = terminal
        .get("output")
        .cloned()
        .expect("a completed workflow must carry an output");
    let output: PipelineOutput =
        serde_json::from_value(output_raw).expect("decode workflow output");
    assert_eq!(
        output.label, "alpha",
        "input label must round-trip to output"
    );
    assert_eq!(output.steps_run, 3);

    // Every declared step must be recorded, in declaration order, completed.
    let steps = terminal
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .expect("workflow data must carry a steps array");
    let recorded: Vec<(&str, &str)> = steps
        .iter()
        .map(|step| {
            (
                step.get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                step.get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        recorded,
        vec![
            ("prepare", "completed"),
            ("process", "completed"),
            ("finalize", "completed"),
        ],
        "all three steps must run in order and complete, saw: {steps:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// The durable case: a workflow that blocks on `wait_for_event` must suspend in
/// `waiting`, naming the event it needs — and stay there until something fires
/// that event. Firing it from a separate RPC must resume the run to completion.
#[tokio::test]
async fn gated_workflow_blocks_on_event_then_resumes() {
    let app = start_app("workflows_gated").await;
    let session = app.open_session(None).await.expect("open sse session");

    let handle: WorkflowHandle = app
        .client()
        .call(
            "harness_gated",
            PipelineInput {
                label: "beta".into(),
            },
        )
        .await
        .expect("dispatch gated workflow");

    let snapshot = session
        .subscribe_workflow("wf", &handle.workflow_id)
        .await
        .expect("subscribe to workflow");

    // The workflow records one step, then blocks. `completed` is in the wanted
    // set only to fail fast: if the gate were ignored the run would sail to
    // completion, and the assert below would catch that regression.
    let waiting = await_workflow_status(
        &session,
        "wf",
        &snapshot,
        &["waiting", "completed"],
        WF_BUDGET,
    )
    .await;
    assert_eq!(
        waiting.get("status").and_then(serde_json::Value::as_str),
        Some("waiting"),
        "a gated workflow must suspend in `waiting`, not run straight through: {waiting}",
    );
    assert_eq!(
        waiting
            .get("waiting_for")
            .and_then(serde_json::Value::as_str),
        Some("harness_gate_opened"),
        "the suspended run must name the event it is blocked on, saw: {waiting}",
    );

    // Fire the gate from a separate RPC — the "unblock it from another tab"
    // step. The waiting run must then wake and finish.
    let opened: bool = app
        .client()
        .call(
            "harness_open_gate",
            serde_json::json!({ "workflow_id": handle.workflow_id }),
        )
        .await
        .expect("fire gate event");
    assert!(
        opened,
        "harness_open_gate must report the event was written"
    );

    let terminal = await_workflow_status(&session, "wf", &waiting, &["completed"], WF_BUDGET).await;
    assert_eq!(
        terminal.get("status").and_then(serde_json::Value::as_str),
        Some("completed"),
        "a gated workflow must finish once its gate opens, saw: {terminal}",
    );
    let output_raw = terminal
        .get("output")
        .cloned()
        .expect("a completed workflow must carry an output");
    let output: PipelineOutput =
        serde_json::from_value(output_raw).expect("decode workflow output");
    assert_eq!(
        output.label, "beta",
        "input label must survive a suspend/resume cycle",
    );
    assert_eq!(output.steps_run, 3);

    app.shutdown().await.expect("shutdown");
}

/// `subscribe-workflow` hands back the workflow's current state synchronously,
/// at subscribe time — the equivalent of a store's first value.
#[tokio::test]
async fn subscribe_workflow_returns_initial_snapshot() {
    let app = start_app("workflows_snapshot").await;
    let session = app.open_session(None).await.expect("open sse session");

    let handle: WorkflowHandle = app
        .client()
        .call(
            "harness_linear",
            PipelineInput {
                label: "snap".into(),
            },
        )
        .await
        .expect("dispatch workflow");

    let snapshot = session
        .subscribe_workflow("wf", &handle.workflow_id)
        .await
        .expect("subscribe to workflow");

    assert_eq!(
        snapshot
            .get("workflow_id")
            .and_then(serde_json::Value::as_str),
        Some(handle.workflow_id.as_str()),
        "snapshot must identify the workflow it describes",
    );
    assert!(
        snapshot
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "snapshot must carry a status, got: {snapshot}",
    );

    app.shutdown().await.expect("shutdown");
}

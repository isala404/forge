use dioxus::prelude::*;
use forge_dioxus::{WorkflowStatus, use_signals};
use serde_json::json;

use crate::forge::{
    ConfirmVerificationInput, User, VerificationInput, confirm_verification,
    use_account_verification, use_forge_client,
};

#[component]
pub fn VerificationCard(selected_user: Signal<Option<User>>) -> Element {
    let signals = use_signals();
    let mut run_request = use_signal(|| None::<(u64, String, String)>);

    let start = {
        let signals = signals.clone();
        move |_| {
            signals.track_with_properties("workflow_started", json!({"type": "verification"}));
            let nonce = run_request().as_ref().map(|(n, _, _)| n + 1).unwrap_or(1);
            let (account_id, email) = match selected_user() {
                Some(u) => (u.id.clone(), u.email.clone()),
                None => ("demo-user".into(), "demo@example.com".into()),
            };
            run_request.set(Some((nonce, account_id, email)));
        }
    };

    rsx! {
        section { class: "card",
            h2 { "Verification " span { class: "badge purple", "workflow" } }
            if let Some((nonce, account_id, email)) = run_request() {
                VerificationRun { key: "{nonce}", account_id, email, on_restart: start }
            } else {
                p { class: "muted small workflow-desc", "Multi-step workflow with event wait" }
                button { onclick: start, "Start Workflow" }
            }
        }
    }
}

#[component]
fn VerificationRun(
    account_id: String,
    email: String,
    on_restart: EventHandler<MouseEvent>,
) -> Element {
    let signals = use_signals();
    let wf = use_account_verification(VerificationInput::new(account_id, email));
    let mut confirm_sent = use_signal(|| false);

    let is_waiting = wf.state.status == WorkflowStatus::Waiting;
    let is_confirming = *confirm_sent.read() && wf.state.status == WorkflowStatus::Running;
    let show_confirm = is_waiting || is_confirming;
    let can_restart = matches!(
        wf.state.status,
        WorkflowStatus::Completed | WorkflowStatus::Failed
    );

    let client = use_forge_client();
    let workflow_id = wf.state.workflow_id.clone();
    let handle_confirm = {
        let signals = signals.clone();
        move |_| {
            if *confirm_sent.read() {
                return;
            }
            signals.track("workflow_confirmed");
            confirm_sent.set(true);
            let wf_id = workflow_id.clone();
            let client = client.clone();
            spawn(async move {
                let input = ConfirmVerificationInput::new(wf_id);
                if confirm_verification(&client, input).await.is_err() {
                    confirm_sent.set(false);
                }
            });
        }
    };

    rsx! {
        div { class: "steps",
            for step in wf.state.steps.iter() {
                div { key: "{step.name}", class: "step {step.status}",
                    span { class: "icon", {step_icon(&step.status)} }
                    span { "{step.name}" }
                }
            }
        }
        if show_confirm {
            p { class: "muted small",
                if *confirm_sent.read() { "Confirmation sent, finishing up..." } else { "Waiting for your confirmation..." }
            }
            button {
                class: "confirm-btn",
                disabled: *confirm_sent.read(),
                onclick: handle_confirm,
                if *confirm_sent.read() { "Confirmed" } else { "Confirm Verification" }
            }
        }
        if can_restart {
            button { onclick: move |e| on_restart.call(e), "Run Again" }
        }
    }
}

fn step_icon(status: &str) -> &'static str {
    match status {
        "completed" => "[=]",
        "running" => "[>]",
        "failed" => "[x]",
        _ => "[ ]",
    }
}

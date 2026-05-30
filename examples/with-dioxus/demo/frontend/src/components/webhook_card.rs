use dioxus::prelude::*;
use forge_dioxus::use_signals;
use serde_json::json;

use super::{format_time, generate_key};
use crate::forge::{
    TriggerDemoWebhookInput, trigger_demo_webhook, use_forge_client,
    use_get_webhook_events_subscription,
};

#[component]
pub fn WebhookCard() -> Element {
    let signals = use_signals();
    let state = use_get_webhook_events_subscription();
    let events = state.data.clone().unwrap_or_default();

    let mut idempotency_key = use_signal(generate_key);
    let mut key_used = use_signal(|| false);
    let mut webhook_error = use_signal(|| None::<String>);

    let client = use_forge_client();

    rsx! {
        section { class: "card",
            h2 { "Webhook " span { class: "badge", "webhook" } }
            label { class: "input-label", "Idempotency Key" }
            div { class: "webhook-row",
                input {
                    r#type: "text",
                    class: if key_used() { "key-input used" } else { "key-input" },
                    value: idempotency_key(),
                    readonly: true,
                }
                button { class: "small",
                    onclick: {
                        let signals = signals.clone();
                        move |_| {
                            signals.track("webhook_key_generated");
                            idempotency_key.set(generate_key());
                            key_used.set(false);
                            webhook_error.set(None);
                        }
                    },
                    "New"
                }
                button { disabled: key_used(),
                    onclick: {
                        let signals = signals.clone();
                        let client = client.clone();
                        move |_| {
                            if key_used() { return; }
                            webhook_error.set(None);
                            let key = idempotency_key();
                            let signals = signals.clone();
                            let client = client.clone();
                            spawn(async move {
                                // The HMAC secret lives on the server. The backend signs
                                // and POSTs the webhook to itself so the WASM bundle
                                // never ships the secret.
                                let input = TriggerDemoWebhookInput::new(key.clone());
                                match trigger_demo_webhook(&client, input).await {
                                    Ok(_) => {
                                        signals.track_with_properties("webhook_sent", json!({"idempotency_key": &key}));
                                        key_used.set(true);
                                    }
                                    Err(e) => {
                                        signals.track("webhook_error");
                                        webhook_error.set(Some(e.to_string()));
                                    }
                                }
                            });
                        }
                    },
                    "Send"
                }
            }
            if key_used() {
                p { class: "hint success", "Webhook processed. Generate a new key to send another." }
            }
            if let Some(msg) = webhook_error() {
                p { class: "hint warning", "{msg}" }
            }
            if !events.is_empty() {
                label { class: "input-label events-label", "Recent Events" }
                div { class: "events",
                    for ev in &events {
                        div { key: "{ev.id}", class: "event",
                            span { class: "mono", "{ev.idempotency_key}" }
                            span { class: "time", {format_time(&ev.processed_at)} }
                        }
                    }
                }
            }
        }
    }
}

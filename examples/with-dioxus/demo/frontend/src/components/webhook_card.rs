use dioxus::prelude::*;
use forge_dioxus::use_signals;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use super::{format_time, generate_key};
use crate::forge::use_get_webhook_events_subscription;

#[component]
pub fn WebhookCard(api_url: String) -> Element {
    let signals = use_signals();
    let state = use_get_webhook_events_subscription();
    let events = state.data.clone().unwrap_or_default();

    let mut idempotency_key = use_signal(generate_key);
    let mut key_used = use_signal(|| false);
    let mut webhook_error = use_signal(|| None::<String>);

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
                        let api_url = api_url.clone();
                        let signals = signals.clone();
                        move |_| {
                            if key_used() { return; }
                            webhook_error.set(None);
                            let key = idempotency_key();
                            let api_url = api_url.clone();
                            let signals = signals.clone();
                            spawn(async move {
                                match trigger_webhook(&api_url, &key).await {
                                    Ok(()) => {
                                        signals.track_with_properties("webhook_sent", json!({"idempotency_key": &key}));
                                        key_used.set(true);
                                    }
                                    Err(msg) => {
                                        signals.track("webhook_error");
                                        webhook_error.set(Some(msg));
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

async fn trigger_webhook(api_url: &str, idempotency_key: &str) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    let now = js_sys::Date::now();
    #[cfg(not(target_arch = "wasm32"))]
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    let payload = serde_json::json!({ "action": "test", "ts": now }).to_string();

    let mut mac = Hmac::<Sha256>::new_from_slice(b"demo-secret").map_err(|e| e.to_string())?;
    mac.update(payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    // HMAC-SHA256 webhooks enforce a replay window: the server rejects any
    // request whose `X-Webhook-Timestamp` (unix seconds) is missing or outside
    // the 300s window. Send it alongside the signature.
    let timestamp = (now / 1000.0) as i64;

    let resp = reqwest::Client::new()
        .post(format!("{api_url}/_api/webhooks/demo"))
        .header("Content-Type", "application/json")
        .header("X-Webhook-Signature", signature)
        .header("X-Webhook-Timestamp", timestamp.to_string())
        .header("X-Idempotency-Key", idempotency_key)
        .body(payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("Error: {}", resp.status().as_u16()))
    }
}

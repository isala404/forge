use dioxus::prelude::*;
use forge_dioxus::use_signals;
use serde_json::json;

use super::format_time;
use crate::forge::{self, DemoStats, use_forge_client};

fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as f64
    }
}

#[component]
pub fn CacheCard() -> Element {
    let client = use_forge_client();
    let signals = use_signals();
    let mut data = use_signal(|| None::<DemoStats>);
    let mut response_ms = use_signal(|| None::<f64>);
    let mut fetch_count = use_signal(|| 0u32);
    let mut loading = use_signal(|| false);

    let handle_fetch = {
        let signals = signals.clone();
        move |_: MouseEvent| {
            let client = client.clone();
            let signals = signals.clone();
            spawn(async move {
                loading.set(true);
                let start = now_ms();
                if let Ok(stats) = forge::get_demo_stats(&client).await {
                    let elapsed = now_ms() - start;
                    signals.track_with_properties("cache_fetch", json!({"response_ms": elapsed, "cache_hit": elapsed < 100.0, "fetch_number": fetch_count() + 1}));
                    data.set(Some(stats));
                    response_ms.set(Some(elapsed));
                    fetch_count.set(fetch_count() + 1);
                }
                loading.set(false);
            });
        }
    };

    let is_cached = response_ms().is_some_and(|ms| ms < 100.0);

    rsx! {
        section { class: "card",
            h2 {
                "Cached Query "
                span { class: "badge", "cache = 10s" }
            }

            p { class: "muted small", style: "margin-bottom: 0.75rem; margin-top: 0;",
                "Server-side query takes ~500ms (simulated). Cache returns instantly."
            }

            button {
                onclick: handle_fetch,
                disabled: loading(),
                if loading() { "Fetching..." } else { "Fetch Stats" }
            }

            if let Some(stats) = data() {
                div { class: "cache-stats", style: "margin-top: 0.75rem;",
                    div { class: "stat-row",
                        span { class: "meta-key", "Users" }
                        span { class: "mono", "{stats.total_users}" }
                    }
                    div { class: "stat-row",
                        span { class: "meta-key", "Trades" }
                        span { class: "mono", "{stats.total_trades}" }
                    }
                    div { class: "stat-row",
                        span { class: "meta-key", "Webhooks" }
                        span { class: "mono", "{stats.total_webhooks}" }
                    }
                    div { class: "stat-row",
                        span { class: "meta-key", "Computed" }
                        span { class: "mono", "{format_time(&stats.computed_at)}" }
                    }
                }
            }

            if let Some(ms) = response_ms() {
                p {
                    class: if is_cached { "hint success" } else { "hint warning" },
                    style: "margin-top: 0.5rem;",
                    "{ms:.0}ms "
                    if is_cached {
                        "· cache hit"
                    } else {
                        "· cache miss"
                    }
                    " · fetch #{fetch_count}"
                }
            }
        }
    }
}

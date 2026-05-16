//! Wires the ForgeSignals SDK into the demo:
//!   - Auto-identifies the signed-in user across web, desktop, and iOS.
//!   - On web only, exposes `window.forgeSignals` so Playwright specs can
//!     drive the SDK from JS (mirrors the Svelte demo's bridge).

use dioxus::prelude::*;

#[component]
pub fn SignalsBridge() -> Element {
    let signals = forge_dioxus::use_signals();
    let auth = forge_dioxus::use_forge_auth();
    let mut last_identified = use_signal(|| None::<String>);

    #[cfg(target_arch = "wasm32")]
    {
        let bridge_signals = signals.clone();
        use_hook(move || install_window_bridge(bridge_signals));
    }

    let effect_signals = signals.clone();
    use_effect(move || {
        if let Some(viewer) = auth.viewer::<crate::forge::UserPublic>() {
            let user_id = viewer.id.to_string();
            if last_identified.read().as_deref() != Some(user_id.as_str()) {
                last_identified.set(Some(user_id.clone()));
                let signals = effect_signals.clone();
                let traits = serde_json::json!({
                    "email": viewer.email,
                    "name": viewer.name,
                });
                signals.identify(&user_id, traits);
            }
        }
    });

    rsx! {}
}

#[cfg(target_arch = "wasm32")]
fn install_window_bridge(signals: forge_dioxus::ForgeSignals) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen_futures::spawn_local;

    fn js_to_value(v: &wasm_bindgen::JsValue) -> serde_json::Value {
        if v.is_undefined() || v.is_null() {
            return serde_json::Value::Null;
        }
        match js_sys::JSON::stringify(v) {
            Ok(s) => s
                .as_string()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null),
            Err(_) => serde_json::Value::Null,
        }
    }

    let track = {
        let signals = signals.clone();
        Closure::<dyn Fn(wasm_bindgen::JsValue, wasm_bindgen::JsValue)>::new(
            move |event: wasm_bindgen::JsValue, props: wasm_bindgen::JsValue| {
                let name = event.as_string().unwrap_or_default();
                let properties = js_to_value(&props);
                if properties.as_object().is_some_and(|o| !o.is_empty()) {
                    signals.track_with_properties(&name, properties);
                } else {
                    signals.track(&name);
                }
            },
        )
    };

    let identify = {
        let signals = signals.clone();
        Closure::<dyn Fn(wasm_bindgen::JsValue, wasm_bindgen::JsValue)>::new(
            move |user: wasm_bindgen::JsValue, traits: wasm_bindgen::JsValue| {
                let user_id = user.as_string().unwrap_or_default();
                let traits = js_to_value(&traits);
                signals.identify(&user_id, traits);
            },
        )
    };

    let breadcrumb = {
        let signals = signals.clone();
        Closure::<dyn Fn(wasm_bindgen::JsValue, wasm_bindgen::JsValue)>::new(
            move |msg: wasm_bindgen::JsValue, data: wasm_bindgen::JsValue| {
                let message = msg.as_string().unwrap_or_default();
                let data = if data.is_undefined() || data.is_null() {
                    None
                } else {
                    Some(js_to_value(&data))
                };
                signals.breadcrumb(&message, data);
            },
        )
    };

    // captureError accepts a JS Error or a plain string. The Dioxus SDK only
    // takes a message + context, so we extract .message and fold .stack into
    // context so the spec can still assert on stack via context.stack.
    let capture_error = {
        let signals = signals.clone();
        Closure::<dyn Fn(wasm_bindgen::JsValue, wasm_bindgen::JsValue)>::new(
            move |err: wasm_bindgen::JsValue, ctx: wasm_bindgen::JsValue| {
                let (message, stack) = if let Some(s) = err.as_string() {
                    (s, None)
                } else {
                    let msg = js_sys::Reflect::get(&err, &"message".into())
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_else(|| "Error".to_string());
                    let stack = js_sys::Reflect::get(&err, &"stack".into())
                        .ok()
                        .and_then(|v| v.as_string());
                    (msg, stack)
                };
                let mut ctx_val = js_to_value(&ctx);
                if let Some(stack) = stack {
                    if !ctx_val.is_object() {
                        ctx_val = serde_json::json!({});
                    }
                    if let Some(obj) = ctx_val.as_object_mut() {
                        obj.insert("stack".to_string(), serde_json::Value::String(stack));
                    }
                }
                let context = if ctx_val.is_object() && ctx_val.as_object().is_some_and(|o| !o.is_empty()) {
                    Some(ctx_val)
                } else {
                    None
                };
                signals.capture_error(&*message, context);
            },
        )
    };

    let vital = {
        let signals = signals.clone();
        Closure::<dyn Fn(wasm_bindgen::JsValue, f64, wasm_bindgen::JsValue)>::new(
            move |name: wasm_bindgen::JsValue, value: f64, meta: wasm_bindgen::JsValue| {
                let name_str = name.as_string().unwrap_or_default();
                let rating = js_sys::Reflect::get(&meta, &"rating".into())
                    .ok()
                    .and_then(|v| v.as_string());
                signals.vital(&name_str, value, rating.as_deref());
            },
        )
    };

    let page = {
        let signals = signals.clone();
        Closure::<dyn Fn()>::new(move || {
            let path = web_sys::window()
                .and_then(|w| w.location().pathname().ok())
                .unwrap_or_else(|| "/".to_string());
            let query = web_sys::window()
                .and_then(|w| w.location().search().ok())
                .unwrap_or_default();
            let full = format!("{path}{query}");
            let signals = signals.clone();
            spawn_local(async move {
                signals.page(&full).await;
            });
        })
    };

    let next_corr = {
        let signals = signals.clone();
        Closure::<dyn Fn() -> wasm_bindgen::JsValue>::new(move || {
            wasm_bindgen::JsValue::from_str(&signals.next_correlation_id())
        })
    };

    let get_session = {
        let signals = signals.clone();
        Closure::<dyn Fn() -> wasm_bindgen::JsValue>::new(move || match signals.get_session_id() {
            Some(sid) => wasm_bindgen::JsValue::from_str(&sid),
            None => wasm_bindgen::JsValue::NULL,
        })
    };

    let factory_js = r#"
        (function(track, identify, breadcrumb, captureError, vital, page, nextCorrelationId, getSessionId) {
            window.forgeSignals = {
                track: function(event, props) { track(String(event), props == null ? {} : props); },
                identify: function(uid, traits) { identify(String(uid), traits == null ? {} : traits); return Promise.resolve(); },
                breadcrumb: function(msg, data) { breadcrumb(String(msg), data == null ? null : data); },
                captureError: function(err, ctx) { captureError(err, ctx == null ? {} : ctx); return Promise.resolve(); },
                vital: function(name, value, meta) { vital(String(name), Number(value), meta == null ? {} : meta); },
                page: function() { page(); return Promise.resolve(); },
                nextCorrelationId: function() { return nextCorrelationId(); },
                getSessionId: function() { return getSessionId(); },
            };
        })
    "#;

    if let Ok(factory) = js_sys::eval(factory_js)
        && let Ok(function) = factory.dyn_into::<js_sys::Function>()
    {
        let args = js_sys::Array::new();
        args.push(track.as_ref());
        args.push(identify.as_ref());
        args.push(breadcrumb.as_ref());
        args.push(capture_error.as_ref());
        args.push(vital.as_ref());
        args.push(page.as_ref());
        args.push(next_corr.as_ref());
        args.push(get_session.as_ref());
        let _ = function.apply(&wasm_bindgen::JsValue::NULL, &args);
    }

    track.forget();
    identify.forget();
    breadcrumb.forget();
    capture_error.forget();
    vital.forget();
    page.forget();
    next_corr.forget();
    get_session.forget();
}

use dioxus::prelude::*;
use forge_dioxus::use_signals;
use serde_json::json;

use crate::forge::{
    AuthResponse, LoginInput, RefreshInput, RegisterInput, UserPublic, use_forge_auth, use_login,
    use_refresh_token, use_register,
};

#[component]
pub fn AuthCard() -> Element {
    let mut auth = use_forge_auth();
    let signals = use_signals();

    let mut mode = use_signal(|| "login".to_string());
    // Prefill credentials only in debug builds. Release WASM ships empty fields so a
    // public demo is not a one-click login when combined with the seeded admin user.
    let mut auth_email = use_signal(|| {
        if cfg!(debug_assertions) {
            "demo@example.com".to_string()
        } else {
            String::new()
        }
    });
    let mut auth_password = use_signal(|| {
        if cfg!(debug_assertions) {
            "password123".to_string()
        } else {
            String::new()
        }
    });
    let mut auth_name = use_signal(String::new);
    let mut auth_error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| false);

    let mut auth_user = use_signal(|| None::<UserPublic>);
    let mut token_claims = use_signal(|| None::<Vec<(String, String)>>);
    let mut refresh_count = use_signal(|| 0u32);

    let login_mut = use_login();
    let register_mut = use_register();
    let refresh_mut = use_refresh_token();

    let handle_auth = {
        let login_mut = login_mut.clone();
        let register_mut = register_mut.clone();
        let signals = signals.clone();
        move |evt: FormEvent| {
            evt.prevent_default();
            let email = auth_email.read().clone();
            let password = auth_password.read().clone();
            let name = auth_name.read().clone();
            let is_register = mode.read().as_str() == "register";
            let login_mut = login_mut.clone();
            let register_mut = register_mut.clone();
            let signals = signals.clone();

            spawn(async move {
                loading.set(true);
                auth_error.set(None);
                signals.track_with_properties("auth_attempt", json!({"mode": is_register}));

                let result: Result<AuthResponse, _> = if is_register {
                    register_mut
                        .call(RegisterInput::new(&email, &name, &password))
                        .await
                } else {
                    login_mut.call(LoginInput::new(&email, &password)).await
                };

                match result {
                    Ok(res) => {
                        signals.track_with_properties("auth_success", json!({"mode": is_register}));
                        signals.identify(
                            &res.user.id,
                            json!({"name": &res.user.name, "email": &res.user.email}),
                        );
                        let claims = parse_jwt_claims(&res.access_token);
                        token_claims.set(Some(claims));
                        // Wire auth into ForgeAuthProvider so the client
                        // sends Bearer tokens on all subsequent API calls,
                        // which triggers the session cookie for OAuth.
                        auth.login_with_viewer(
                            res.access_token.clone(),
                            res.refresh_token.clone(),
                            &res.user,
                        );
                        auth_user.set(Some(res.user));
                        refresh_count.set(0);
                    }
                    Err(e) => {
                        signals.track_with_properties(
                            "auth_error",
                            json!({"mode": is_register, "error": &e.message}),
                        );
                        auth_error.set(Some(e.message));
                    }
                }
                loading.set(false);
            });
        }
    };

    let handle_refresh = {
        let refresh_mut = refresh_mut.clone();
        let signals = signals.clone();
        move |_: MouseEvent| {
            let rt = auth.refresh_token();
            let refresh_mut = refresh_mut.clone();
            let signals = signals.clone();
            if let Some(rt) = rt {
                spawn(async move {
                    auth_error.set(None);
                    match refresh_mut.call(RefreshInput::new(&rt)).await {
                        Ok(pair) => {
                            signals.track_with_properties(
                                "token_refresh",
                                json!({"count": refresh_count() + 1}),
                            );
                            let claims = parse_jwt_claims(&pair.access_token);
                            token_claims.set(Some(claims));
                            auth.update_tokens(
                                pair.access_token.clone(),
                                pair.refresh_token.clone(),
                            );
                            refresh_count.set(refresh_count() + 1);
                        }
                        Err(e) => {
                            signals.track("token_refresh_error");
                            auth_error.set(Some(e.message));
                        }
                    }
                });
            }
        }
    };

    let handle_logout = {
        let signals = signals.clone();
        move |_: MouseEvent| {
            signals.track("logout");
            auth.logout();
            auth_user.set(None);
            token_claims.set(None);
            refresh_count.set(0);
            auth_error.set(None);
        }
    };

    let is_logged_in = auth.is_authenticated();

    // Restore viewer on mount (persisted in localStorage by ForgeAuthProvider)
    use_effect(move || {
        if auth.is_authenticated()
            && auth_user.read().is_none()
            && let Some(viewer) = auth.viewer::<UserPublic>()
        {
            if let Some(token) = auth.access_token() {
                token_claims.set(Some(parse_jwt_claims(&token)));
            }
            auth_user.set(Some(viewer));
        }
    });

    rsx! {
        section { class: "card",
            h2 {
                "Auth "
                span { class: "badge purple", "refresh tokens" }
            }

            if is_logged_in {
                div { class: "auth-user",
                    span { class: "label", "Logged in as" }
                    if let Some(u) = auth_user.read().as_ref() {
                        span { class: "value", "{u.name} ({u.email})" }
                    }
                }

                div { class: "input-label", style: "margin-top: 0.5rem;", "TOKEN METADATA" }
                div { class: "token-meta",
                    if let Some(claims) = token_claims.read().as_ref() {
                        for (key, val) in claims.iter() {
                            div { class: "meta-row",
                                span { class: "meta-key", "{key}" }
                                span { class: "mono", "{val}" }
                            }
                        }
                    }
                }

                div { class: "auth-actions",
                    button { onclick: handle_refresh, "Refresh Token" }
                    button { class: "secondary", onclick: handle_logout, "Logout" }
                }

                if refresh_count() > 0 {
                    {
                        let suffix = if refresh_count() > 1 { "s" } else { "" };
                        rsx! {
                            p { class: "hint success",
                                "Token refreshed {refresh_count()} time{suffix}"
                            }
                        }
                    }
                }
            } else {
                div { class: "auth-tabs",
                    button {
                        class: if mode.read().as_str() == "login" { "tab active" } else { "tab" },
                        onclick: {
                            let signals = signals.clone();
                            move |_| {
                                signals.track_with_properties("auth_tab_switch", json!({"tab": "login"}));
                                mode.set("login".into());
                            }
                        },
                        "Login"
                    }
                    button {
                        class: if mode.read().as_str() == "register" { "tab active" } else { "tab" },
                        onclick: {
                            let signals = signals.clone();
                            move |_| {
                                signals.track_with_properties("auth_tab_switch", json!({"tab": "register"}));
                                mode.set("register".into());
                            }
                        },
                        "Register"
                    }
                }

                form { onsubmit: handle_auth,
                    if mode.read().as_str() == "register" {
                        input {
                            r#type: "text",
                            placeholder: "Name",
                            value: "{auth_name}",
                            oninput: move |e: FormEvent| auth_name.set(e.value()),
                        }
                    }
                    input {
                        r#type: "email",
                        placeholder: "Email",
                        value: "{auth_email}",
                        oninput: move |e: FormEvent| auth_email.set(e.value()),
                    }
                    input {
                        r#type: "password",
                        placeholder: "Password (min 8 chars)",
                        value: "{auth_password}",
                        oninput: move |e: FormEvent| auth_password.set(e.value()),
                    }
                    button {
                        r#type: "submit",
                        disabled: loading(),
                        if loading() { "..." } else if mode.read().as_str() == "login" { "Login" } else { "Register" }
                    }
                }
                p { class: "muted small", "Try demo@example.com / password123" }
            }

            if let Some(err) = auth_error.read().as_ref() {
                p { class: "hint warning", "{err}" }
            }
        }
    }
}

fn parse_jwt_claims(token: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return vec![];
    }

    use base64::Engine;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let decoded = match engine.decode(parts[1]) {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    let json: serde_json::Value = match serde_json::from_slice(&decoded) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let obj = match json.as_object() {
        Some(o) => o,
        None => return vec![],
    };

    let display_keys = ["sub", "roles", "iat", "exp"];
    let mut result = Vec::new();

    for key in display_keys {
        if let Some(val) = obj.get(key) {
            let formatted = match key {
                "iat" | "exp" => {
                    if let Some(ts) = val.as_i64() {
                        format_timestamp(ts)
                    } else {
                        val.to_string()
                    }
                }
                _ => val.to_string(),
            };
            result.push((key.to_string(), formatted));
        }
    }

    result
}

fn format_timestamp(ts: i64) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64 * 1000.0));
        date.to_locale_time_string("en-US")
            .as_string()
            .unwrap_or_else(|| ts.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%I:%M:%S %p").to_string())
            .unwrap_or_else(|| ts.to_string())
    }
}

//! Built-in auth + viewer state management for forge-dioxus.
//!
//! Handles token storage, viewer persistence, refresh loops, and 401
//! recovery. Apps get viewer access for free without writing their own
//! storage layer.

use dioxus::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::signals::{ForgeSignals, SignalsConfig, setup_auto_capture};
use crate::{ConnectionState, ForgeClient, ForgeClientConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAuth {
    access_token: String,
    refresh_token: String,
    viewer: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ForgeAuthState {
    Unauthenticated,
    Authenticated {
        access_token: String,
        refresh_token: String,
        viewer: Option<serde_json::Value>,
    },
}

impl ForgeAuthState {
    pub fn is_authenticated(&self) -> bool {
        matches!(self, Self::Authenticated { .. })
    }

    pub fn access_token(&self) -> Option<String> {
        match self {
            Self::Authenticated { access_token, .. } => Some(access_token.clone()),
            Self::Unauthenticated => None,
        }
    }

    pub fn refresh_token(&self) -> Option<String> {
        match self {
            Self::Authenticated { refresh_token, .. } => Some(refresh_token.clone()),
            Self::Unauthenticated => None,
        }
    }

    fn viewer_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Authenticated { viewer, .. } => viewer.as_ref(),
            Self::Unauthenticated => None,
        }
    }
}

/// Auth handle provided to components via `use_forge_auth()`.
#[derive(Clone, Copy)]
pub struct ForgeAuth {
    state: Signal<ForgeAuthState>,
    app_name: Signal<String>,
    generation: Signal<u64>,
}

impl ForgeAuth {
    pub fn is_authenticated(&self) -> bool {
        self.state.read().is_authenticated()
    }

    pub fn access_token(&self) -> Option<String> {
        self.state.read().access_token()
    }

    pub fn refresh_token(&self) -> Option<String> {
        self.state.read().refresh_token()
    }

    /// Read the stored viewer, deserialized into the app's type.
    pub fn viewer<V: DeserializeOwned>(&self) -> Option<V> {
        let state = self.state.read();
        let json = state.viewer_json()?;
        serde_json::from_value(json.clone()).ok()
    }

    /// Set tokens after login/register (no viewer).
    pub fn login(&mut self, access_token: String, refresh_token: String) {
        self.save_and_set(access_token, refresh_token, None);
    }

    /// Set tokens + viewer after login/register.
    pub fn login_with_viewer<V: Serialize>(
        &mut self,
        access_token: String,
        refresh_token: String,
        viewer: &V,
    ) {
        let viewer_json = serde_json::to_value(viewer).ok();
        self.save_and_set(access_token, refresh_token, viewer_json);
    }

    /// Update tokens (e.g., after a refresh). Preserves existing viewer.
    pub fn update_tokens(&mut self, access_token: String, refresh_token: String) {
        let existing_viewer = self.state.read().viewer_json().cloned();
        self.save_and_set(access_token, refresh_token, existing_viewer);
    }

    /// Update just the viewer without touching tokens.
    pub fn update_viewer<V: Serialize>(&mut self, viewer: &V) {
        let state = self.state.read();
        let (access_token, refresh_token) = match &*state {
            ForgeAuthState::Authenticated {
                access_token,
                refresh_token,
                ..
            } => (access_token.clone(), refresh_token.clone()),
            ForgeAuthState::Unauthenticated => return,
        };
        drop(state);
        let viewer_json = serde_json::to_value(viewer).ok();
        self.save_and_set(access_token, refresh_token, viewer_json);
    }

    /// Clear tokens, viewer, and log out.
    pub fn logout(&mut self) {
        storage::clear(&self.app_name.read());
        self.state.set(ForgeAuthState::Unauthenticated);
        self.generation.with_mut(|g| *g += 1);
    }

    fn save_and_set(
        &mut self,
        access_token: String,
        refresh_token: String,
        viewer: Option<serde_json::Value>,
    ) {
        let stored = StoredAuth {
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            viewer: viewer.clone(),
        };
        storage::save(&self.app_name.read(), &stored);
        let was_authenticated = self.state.read().is_authenticated();
        self.state.set(ForgeAuthState::Authenticated {
            access_token,
            refresh_token,
            viewer,
        });
        if !was_authenticated {
            self.generation.with_mut(|g| *g += 1);
        }
    }
}

pub fn use_forge_auth() -> ForgeAuth {
    use_context::<ForgeAuth>()
}

/// Returns `None` when unauthenticated or if the viewer hasn't been set.
pub fn use_viewer<V: DeserializeOwned + Clone + 'static>() -> Option<V> {
    use_forge_auth().viewer::<V>()
}

/// Returns a string key that changes on login/logout transitions.
/// Use this to key your router or main content area so SSE subscriptions
/// reconnect with fresh auth state.
///
/// ```ignore
/// let auth_key = use_auth_key();
/// rsx! { main { key: "{auth_key}", Router::<Route> {} } }
/// ```
pub fn use_auth_key() -> String {
    let auth = use_forge_auth();
    let generation = auth.generation.read();
    format!("forge-auth-{generation}")
}

/// Guard hook: redirects to `redirect_path` when unauthenticated.
/// Returns `true` if authenticated, `false` during redirect.
///
/// ```ignore
/// fn ProtectedPage() -> Element {
///     if !use_require_auth("/login") { return rsx! {} }
///     // ... render protected content
/// }
/// ```
#[cfg(feature = "router")]
pub fn use_require_auth(redirect_path: &str) -> bool {
    let auth = use_forge_auth();
    let navigator = use_navigator();
    let path = redirect_path.to_string();

    use_effect(move || {
        if !auth.is_authenticated() {
            navigator.replace(NavigationTarget::Internal(path.clone()));
        }
    });

    auth.is_authenticated()
}

/// Provider component that sets up auth state, ForgeClient with auto token wiring,
/// 401 detection, and periodic refresh.
///
/// ```ignore
/// ForgeAuthProvider {
///     url: "http://localhost:9081",
///     app_name: "my-app",
///     children: rsx! { Router::<Route> {} }
/// }
/// ```
/// `refresh_interval_secs`: How often to proactively refresh tokens (default: 2400 = 40 min).
/// Set to roughly 2/3 of your `access_token_ttl` from forge.toml.
#[component]
pub fn ForgeAuthProvider(
    url: String,
    #[props(default = "forge_app".to_string())] app_name: String,
    #[props(default = 2400)] refresh_interval_secs: u64,
    #[props(default)] on_mutation_error: Option<EventHandler<crate::ForgeClientError>>,
    children: Element,
) -> Element {
    let initial = match storage::load(&app_name) {
        Some(stored) => ForgeAuthState::Authenticated {
            access_token: stored.access_token,
            refresh_token: stored.refresh_token,
            viewer: stored.viewer,
        },
        None => ForgeAuthState::Unauthenticated,
    };

    let auth_state = use_context_provider(|| Signal::new(initial));
    let app_name_signal = use_context_provider(|| Signal::new(app_name));
    let generation = use_context_provider(|| Signal::new(0_u64));
    let forge_auth = use_context_provider(|| ForgeAuth {
        state: auth_state,
        app_name: app_name_signal,
        generation,
    });

    let connection_state = use_context_provider(|| Signal::new(ConnectionState::Disconnected));
    let needs_refresh = use_signal(|| false);

    let url_clone = url.clone();
    use_context_provider(move || {
        let auth_for_token = auth_state;
        let needs_refresh_clone = needs_refresh;
        let mut config = ForgeClientConfig::new(url_clone)
            .with_connection_state(connection_state)
            .with_token_provider(move || auth_for_token.read().access_token())
            .with_auth_error_handler(move |_err| {
                let mut sig = needs_refresh_clone;
                sig.set(true);
            });
        if let Some(handler) = on_mutation_error {
            config = config.with_mutation_error_handler(move |err| handler.call(err));
        }
        ForgeClient::new(config)
    });

    // Must follow use_context_provider above so ForgeClient is available
    let client: ForgeClient = use_context();

    let url_for_refresh = url.clone();
    let client_for_refresh = client.clone();
    use_effect(move || {
        if !*needs_refresh.read() {
            return;
        }
        let url = url_for_refresh.clone();
        let mut auth = forge_auth;
        let client = client_for_refresh.clone();
        let mut needs_refresh_sig = needs_refresh;
        spawn(async move {
            try_refresh_tokens(&url, &mut auth, &client).await;
            needs_refresh_sig.set(false);
        });
    });

    // Short sleeps (~30 s) instead of one long sleep so browser background-tab
    // throttling can't push the refresh past token expiry.
    let url_for_periodic = url;
    let client_for_periodic = client.clone();
    use_future(move || {
        let url = url_for_periodic.clone();
        let client = client_for_periodic.clone();
        let mut auth = forge_auth;
        async move {
            let poll_secs: u64 = 30;
            let mut elapsed: u64 = 0;
            loop {
                sleep(poll_secs).await;
                elapsed += poll_secs;
                if elapsed < refresh_interval_secs {
                    continue;
                }
                elapsed = 0;
                if auth.is_authenticated() {
                    try_refresh_tokens(&url, &mut auth, &client).await;
                }
            }
        }
    });
    let signals_instance = use_context_provider(|| {
        let s = ForgeSignals::new(client.clone(), SignalsConfig::default());
        client.set_signals(s.clone());
        s
    });
    use_hook(|| {
        setup_auto_capture(signals_instance);
    });

    rsx! { {children} }
}

/// Ignores network errors so transient connectivity issues (hospital
/// networks, flaky wifi) don't force unnecessary logouts. Only logs out
/// on definitive 401/403. Reconnects SSE on success so the new token
/// takes effect immediately.
async fn try_refresh_tokens(api_url: &str, auth: &mut ForgeAuth, client: &ForgeClient) -> bool {
    let refresh_token = match auth.refresh_token() {
        Some(t) => t,
        None => return false,
    };

    let anon_client = ForgeClient::new(ForgeClientConfig::new(api_url.to_string()));

    #[derive(Serialize)]
    struct RefreshArgs {
        refresh_token: String,
    }

    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: String,
    }

    match anon_client
        .call::<_, RefreshResponse>(
            "refresh",
            RefreshArgs {
                refresh_token,
            },
        )
        .await
    {
        Ok(resp) => {
            auth.update_tokens(resp.access_token, resp.refresh_token);
            client.reconnect_sse();
            true
        }
        Err(ref e)
            if e.code == "UNAUTHORIZED"
                || e.code == "FORBIDDEN"
                || e.code == "NOT_FOUND" =>
        {
            // Definitive auth failure: token is invalid/expired/revoked.
            auth.logout();
            false
        }
        Err(_) => {
            // Network or transient error. Keep current tokens and retry
            // on the next refresh cycle rather than forcing a logout.
            false
        }
    }
}

async fn sleep(secs: u64) {
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new((secs * 1000) as u32).await;

    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
}

#[cfg(target_arch = "wasm32")]
mod storage {
    use super::StoredAuth;

    fn key(app_name: &str) -> String {
        format!("{app_name}_auth")
    }

    pub fn save(app_name: &str, auth: &StoredAuth) {
        if let Ok(json) = serde_json::to_string(auth) {
            if let Some(storage) = web_sys::window()
                .and_then(|w| w.local_storage().ok())
                .flatten()
            {
                let _ = storage.set_item(&key(app_name), &json);
            }
        }
    }

    pub fn load(app_name: &str) -> Option<StoredAuth> {
        let storage = web_sys::window()?.local_storage().ok()??;
        let json = storage.get_item(&key(app_name)).ok()??;
        serde_json::from_str(&json).ok()
    }

    pub fn clear(app_name: &str) {
        if let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
        {
            let _ = storage.remove_item(&key(app_name));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod storage {
    use super::StoredAuth;
    use std::fs;
    use std::path::PathBuf;

    fn storage_path(app_name: &str) -> Option<PathBuf> {
        dirs::data_local_dir().map(|base| base.join(app_name).join("auth.json"))
    }

    pub fn save(app_name: &str, auth: &StoredAuth) {
        let Some(path) = storage_path(app_name) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec(auth) {
            let tmp = path.with_extension("tmp");
            let _ = fs::write(&tmp, json).and_then(|()| fs::rename(tmp, path));
        }
    }

    pub fn load(app_name: &str) -> Option<StoredAuth> {
        let path = storage_path(app_name)?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn clear(app_name: &str) {
        if let Some(path) = storage_path(app_name) {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authenticated_state_exposes_tokens_and_viewer() {
        let viewer = serde_json::json!({"id": "user-1", "role": "admin"});
        let state = ForgeAuthState::Authenticated {
            access_token: "access-token".into(),
            refresh_token: "refresh-token".into(),
            viewer: Some(viewer.clone()),
        };

        assert!(state.is_authenticated());
        assert_eq!(state.access_token().as_deref(), Some("access-token"));
        assert_eq!(state.refresh_token().as_deref(), Some("refresh-token"));
        assert_eq!(state.viewer_json(), Some(&viewer));
    }

    #[test]
    fn test_unauthenticated_state_has_no_auth_material() {
        let state = ForgeAuthState::Unauthenticated;

        assert!(!state.is_authenticated());
        assert!(state.access_token().is_none());
        assert!(state.refresh_token().is_none());
        assert!(state.viewer_json().is_none());
    }

}

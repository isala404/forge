//! Typesafe environment variable access for handler contexts.
//!
//! Use `ctx.env_require()` / `ctx.env_or()` / `ctx.env_parse()` instead of
//! `std::env::var()`. Test contexts inject mock values via `.with_env()` and
//! track which keys were accessed for test assertions.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use crate::{ForgeError, Result};

/// Trait for environment variable access.
pub trait EnvProvider: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;

    fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// Production environment provider that reads from `std::env`.
#[derive(Debug, Clone, Default)]
pub struct RealEnvProvider;

impl RealEnvProvider {
    pub fn new() -> Self {
        Self
    }

    /// Returns a shared `Arc` to the singleton instance.
    pub fn shared() -> Arc<dyn EnvProvider> {
        static INSTANCE: std::sync::OnceLock<Arc<dyn EnvProvider>> = std::sync::OnceLock::new();
        Arc::clone(INSTANCE.get_or_init(|| Arc::new(Self)))
    }
}

impl EnvProvider for RealEnvProvider {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Mock environment provider for testing.
///
/// Tracks which keys were accessed, so tests can assert on that with
/// `assert_accessed` / `assert_not_accessed`.
#[derive(Debug, Clone, Default)]
pub struct MockEnvProvider {
    vars: HashMap<String, String>,
    accessed: Arc<RwLock<Vec<String>>>,
}

impl MockEnvProvider {
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
            accessed: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn with_vars(vars: HashMap<String, String>) -> Self {
        Self {
            vars,
            accessed: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    pub fn remove(&mut self, key: &str) {
        self.vars.remove(key);
    }

    pub fn all(&self) -> &HashMap<String, String> {
        &self.vars
    }

    pub fn accessed_keys(&self) -> Vec<String> {
        self.accessed
            .read()
            .expect("env accessed lock poisoned")
            .clone()
    }

    pub fn was_accessed(&self, key: &str) -> bool {
        self.accessed
            .read()
            .expect("env accessed lock poisoned")
            .contains(&key.to_string())
    }

    pub fn clear_accessed(&self) {
        self.accessed
            .write()
            .expect("env accessed lock poisoned")
            .clear();
    }

    pub fn assert_accessed(&self, key: &str) {
        assert!(
            self.was_accessed(key),
            "Expected env var '{}' to be accessed, but it wasn't. Accessed keys: {:?}",
            key,
            self.accessed_keys()
        );
    }

    pub fn assert_not_accessed(&self, key: &str) {
        assert!(
            !self.was_accessed(key),
            "Expected env var '{}' to NOT be accessed, but it was",
            key
        );
    }
}

impl EnvProvider for MockEnvProvider {
    fn get(&self, key: &str) -> Option<String> {
        self.accessed
            .write()
            .expect("env accessed lock poisoned")
            .push(key.to_string());
        self.vars.get(key).cloned()
    }
}

/// Extension methods for environment variable access on contexts.
pub trait EnvAccess {
    fn env_provider(&self) -> &dyn EnvProvider;

    fn env(&self, key: &str) -> Option<String> {
        self.env_provider().get(key)
    }

    fn env_or(&self, key: &str, default: &str) -> String {
        self.env_provider()
            .get(key)
            .unwrap_or_else(|| default.to_string())
    }

    fn env_require(&self, key: &str) -> Result<String> {
        self.env_provider().get(key).ok_or_else(|| {
            ForgeError::config(format!("Required environment variable '{}' not set", key))
        })
    }

    fn env_parse<T: FromStr>(&self, key: &str) -> Result<T>
    where
        T::Err: std::fmt::Display,
    {
        let value = self.env_require(key)?;
        value.parse().map_err(|e: T::Err| {
            ForgeError::config(format!(
                "Failed to parse env var '{}' value '{}': {}",
                key, value, e
            ))
        })
    }

    /// Returns the default when unset; errors only if the variable is set but unparseable.
    fn env_parse_or<T: FromStr>(&self, key: &str, default: T) -> Result<T>
    where
        T::Err: std::fmt::Display,
    {
        match self.env_provider().get(key) {
            Some(value) => value.parse().map_err(|e: T::Err| {
                ForgeError::config(format!(
                    "Failed to parse env var '{}' value '{}': {}",
                    key, value, e
                ))
            }),
            None => Ok(default),
        }
    }

    fn env_contains(&self, key: &str) -> bool {
        self.env_provider().contains(key)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    unsafe_code
)]
mod tests {
    use super::*;

    #[test]
    fn test_real_env_provider() {
        unsafe {
            std::env::set_var("FORGE_TEST_VAR", "test_value");
        }

        let provider = RealEnvProvider::new();
        assert_eq!(
            provider.get("FORGE_TEST_VAR"),
            Some("test_value".to_string())
        );
        assert!(provider.contains("FORGE_TEST_VAR"));
        assert!(provider.get("FORGE_NONEXISTENT_VAR").is_none());

        unsafe {
            std::env::remove_var("FORGE_TEST_VAR");
        }
    }

    #[test]
    fn test_mock_env_provider() {
        let mut provider = MockEnvProvider::new();
        provider.set("API_KEY", "secret123");
        provider.set("TIMEOUT", "30");

        assert_eq!(provider.get("API_KEY"), Some("secret123".to_string()));
        assert_eq!(provider.get("TIMEOUT"), Some("30".to_string()));
        assert!(provider.get("MISSING").is_none());

        assert!(provider.was_accessed("API_KEY"));
        assert!(provider.was_accessed("TIMEOUT"));
        assert!(provider.was_accessed("MISSING"));

        provider.assert_accessed("API_KEY");
    }

    #[test]
    fn test_mock_provider_with_vars() {
        let vars = HashMap::from([
            ("KEY1".to_string(), "value1".to_string()),
            ("KEY2".to_string(), "value2".to_string()),
        ]);
        let provider = MockEnvProvider::with_vars(vars);

        assert_eq!(provider.get("KEY1"), Some("value1".to_string()));
        assert_eq!(provider.get("KEY2"), Some("value2".to_string()));
    }

    #[test]
    fn test_clear_accessed() {
        let mut provider = MockEnvProvider::new();
        provider.set("KEY", "value");

        provider.get("KEY");
        assert!(!provider.accessed_keys().is_empty());

        provider.clear_accessed();
        assert!(provider.accessed_keys().is_empty());
    }

    struct TestEnvContext {
        provider: MockEnvProvider,
    }

    impl EnvAccess for TestEnvContext {
        fn env_provider(&self) -> &dyn EnvProvider {
            &self.provider
        }
    }

    #[test]
    fn test_env_access_methods() {
        let mut provider = MockEnvProvider::new();
        provider.set("PORT", "8080");
        provider.set("DEBUG", "true");
        provider.set("BAD_NUMBER", "not_a_number");

        let ctx = TestEnvContext { provider };

        assert_eq!(ctx.env("PORT"), Some("8080".to_string()));
        assert!(ctx.env("MISSING").is_none());

        assert_eq!(ctx.env_or("PORT", "3000"), "8080");
        assert_eq!(ctx.env_or("MISSING", "default"), "default");

        assert_eq!(ctx.env_require("PORT").unwrap(), "8080");
        assert!(ctx.env_require("MISSING").is_err());

        let port: u16 = ctx.env_parse("PORT").unwrap();
        assert_eq!(port, 8080);

        let debug: bool = ctx.env_parse("DEBUG").unwrap();
        assert!(debug);

        let bad: Result<u32> = ctx.env_parse("BAD_NUMBER");
        assert!(bad.is_err());

        let port: u16 = ctx.env_parse_or("MISSING", 3000).unwrap();
        assert_eq!(port, 3000);

        assert!(ctx.env_contains("PORT"));
        assert!(!ctx.env_contains("MISSING"));
    }

    #[test]
    fn mock_remove_drops_var_but_does_not_clear_access_history() {
        let mut provider = MockEnvProvider::new();
        provider.set("TOKEN", "abc");
        let _ = provider.get("TOKEN");
        provider.remove("TOKEN");

        // Subsequent reads see the var as absent.
        assert!(provider.get("TOKEN").is_none());
        // Access history is independent of value state — removing the var does
        // not retroactively un-track the earlier read.
        assert!(provider.was_accessed("TOKEN"));
    }

    #[test]
    fn mock_all_returns_currently_configured_vars() {
        let mut provider = MockEnvProvider::new();
        provider.set("A", "1");
        provider.set("B", "2");
        provider.remove("B");

        let all = provider.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("A"), Some(&"1".to_string()));
        assert!(!all.contains_key("B"));
    }

    #[test]
    fn mock_access_log_preserves_duplicate_reads_in_order() {
        let mut provider = MockEnvProvider::new();
        provider.set("X", "1");
        let _ = provider.get("X");
        let _ = provider.get("Y"); // missing
        let _ = provider.get("X");

        // The log is append-only, not deduped — useful for spotting repeated reads
        // that should be cached.
        assert_eq!(
            provider.accessed_keys(),
            vec!["X".to_string(), "Y".to_string(), "X".to_string()]
        );
    }

    #[test]
    fn mock_assert_not_accessed_passes_when_untouched() {
        let provider = MockEnvProvider::new();
        provider.assert_not_accessed("NEVER_READ");
    }

    #[test]
    fn env_require_error_is_config_variant_with_key_name() {
        let provider = MockEnvProvider::new();
        let ctx = TestEnvContext { provider };

        let err = ctx.env_require("STRIPE_API_KEY").unwrap_err();
        match err {
            ForgeError::Config { context: msg, .. } => {
                assert!(
                    msg.contains("STRIPE_API_KEY"),
                    "msg should name the key: {msg}"
                );
                assert!(
                    msg.contains("not set"),
                    "msg should describe failure: {msg}"
                );
            }
            other => panic!("expected ForgeError::Config, got {other:?}"),
        }
    }

    #[test]
    fn env_parse_error_quotes_key_and_value_in_message() {
        let mut provider = MockEnvProvider::new();
        provider.set("PORT", "not_a_port");
        let ctx = TestEnvContext { provider };

        let err: ForgeError = ctx.env_parse::<u16>("PORT").unwrap_err();
        match err {
            ForgeError::Config { context: msg, .. } => {
                assert!(msg.contains("PORT"), "msg should name the key: {msg}");
                assert!(
                    msg.contains("not_a_port"),
                    "msg should show the bad value: {msg}"
                );
            }
            other => panic!("expected ForgeError::Config, got {other:?}"),
        }
    }

    #[test]
    fn env_parse_or_returns_default_when_unset() {
        let provider = MockEnvProvider::new();
        let ctx = TestEnvContext { provider };

        let port: u16 = ctx.env_parse_or("MISSING_PORT", 8080).unwrap();
        assert_eq!(port, 8080);
    }

    #[test]
    fn env_parse_or_propagates_parse_error_when_var_is_set() {
        // env_parse_or only uses the default when the var is *absent*; a set
        // but unparseable value must surface as an error rather than be hidden.
        let mut provider = MockEnvProvider::new();
        provider.set("RETRIES", "lots");
        let ctx = TestEnvContext { provider };

        let err = ctx.env_parse_or::<u32>("RETRIES", 5).unwrap_err();
        match err {
            ForgeError::Config { context: msg, .. } => {
                assert!(msg.contains("RETRIES"));
                assert!(msg.contains("lots"));
            }
            other => panic!("expected ForgeError::Config, got {other:?}"),
        }
    }

    #[test]
    fn real_provider_contains_delegates_to_get() {
        // contains() has a default trait impl that just checks get().is_some();
        // confirm the wrapping by exercising both paths on the real provider.
        unsafe {
            std::env::set_var("FORGE_CONTAINS_PROBE", "x");
        }
        let p = RealEnvProvider::new();
        assert!(p.contains("FORGE_CONTAINS_PROBE"));
        assert!(!p.contains("FORGE_DEFINITELY_NOT_SET_XYZ_42"));
        unsafe {
            std::env::remove_var("FORGE_CONTAINS_PROBE");
        }
    }
}

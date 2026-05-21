//! Typesafe environment variable access for FORGE functions.
//!
//! This module provides a centralized, testable abstraction for reading environment
//! variables. Instead of scattering `std::env::var()` calls throughout your functions,
//! use `ctx.env()` methods which:
//!
//! - Provide type-safe parsing with clear error messages
//! - Are easily mockable in tests via test context builders
//! - Record which variables were accessed (for debugging)
//!
//! # Example
//!
//! ```ignore
//! #[forge::mutation]
//! async fn call_stripe(ctx: &MutationContext, input: ChargeInput) -> Result<Charge> {
//!     // Get required env var (returns error if missing)
//!     let api_key = ctx.env_require("STRIPE_API_KEY")?;
//!
//!     // Get optional env var with default
//!     let timeout = ctx.env_or("STRIPE_TIMEOUT", "30");
//!
//!     // Get and parse to specific type
//!     let max_retries: u32 = ctx.env_parse("STRIPE_MAX_RETRIES")?;
//!
//!     // Make HTTP call
//!     let response = ctx.http().post("https://api.stripe.com/...").send().await?;
//!
//!     // ...
//! }
//! ```
//!
//! # Testing
//!
//! ```ignore
//! #[test]
//! fn test_stripe_mutation() {
//!     let ctx = TestMutationContext::builder()
//!         .with_env("STRIPE_API_KEY", "sk_test_xxx")
//!         .with_env("STRIPE_TIMEOUT", "60")
//!         .mock_http_json("https://api.stripe.com/*", json!({"id": "ch_123"}))
//!         .build();
//!
//!     // Function will use mocked env vars and HTTP responses
//! }
//! ```

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use crate::{ForgeError, Result};

/// Trait for environment variable access.
///
/// This abstraction allows production code to use real environment variables
/// while tests can inject mock values.
pub trait EnvProvider: Send + Sync {
    /// Get an environment variable by name.
    fn get(&self, key: &str) -> Option<String>;

    /// Check if an environment variable is set.
    fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// Production environment provider that reads from `std::env`.
#[derive(Debug, Clone, Default)]
pub struct RealEnvProvider;

impl RealEnvProvider {
    /// Create a new real environment provider.
    pub fn new() -> Self {
        Self
    }

    /// Return a shared `Arc` to the singleton instance.
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
/// Records which variables were accessed, useful for verifying that
/// functions read the expected environment variables.
#[derive(Debug, Clone, Default)]
pub struct MockEnvProvider {
    /// Configured environment variables.
    vars: HashMap<String, String>,
    /// Keys that were accessed (for verification).
    accessed: Arc<RwLock<Vec<String>>>,
}

impl MockEnvProvider {
    /// Create a new mock provider with no variables.
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
            accessed: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create a mock provider with the given variables.
    pub fn with_vars(vars: HashMap<String, String>) -> Self {
        Self {
            vars,
            accessed: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Set an environment variable.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    /// Remove an environment variable.
    pub fn remove(&mut self, key: &str) {
        self.vars.remove(key);
    }

    /// Get all configured variables.
    pub fn all(&self) -> &HashMap<String, String> {
        &self.vars
    }

    /// Get list of accessed variable names.
    pub fn accessed_keys(&self) -> Vec<String> {
        self.accessed
            .read()
            .expect("env accessed lock poisoned")
            .clone()
    }

    /// Check if a specific key was accessed.
    pub fn was_accessed(&self, key: &str) -> bool {
        self.accessed
            .read()
            .expect("env accessed lock poisoned")
            .contains(&key.to_string())
    }

    /// Clear the accessed keys list.
    pub fn clear_accessed(&self) {
        self.accessed
            .write()
            .expect("env accessed lock poisoned")
            .clear();
    }

    /// Assert that a specific key was accessed.
    pub fn assert_accessed(&self, key: &str) {
        assert!(
            self.was_accessed(key),
            "Expected env var '{}' to be accessed, but it wasn't. Accessed keys: {:?}",
            key,
            self.accessed_keys()
        );
    }

    /// Assert that a specific key was NOT accessed.
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
        // Record access
        self.accessed
            .write()
            .expect("env accessed lock poisoned")
            .push(key.to_string());
        self.vars.get(key).cloned()
    }
}

/// Extension methods for environment variable access on contexts.
///
/// This is implemented as a separate trait to avoid code duplication
/// across different context types.
pub trait EnvAccess {
    /// Get the environment provider.
    fn env_provider(&self) -> &dyn EnvProvider;

    /// Get an environment variable.
    ///
    /// Returns `None` if the variable is not set.
    fn env(&self, key: &str) -> Option<String> {
        self.env_provider().get(key)
    }

    /// Get an environment variable with a default value.
    ///
    /// Returns the default if the variable is not set.
    fn env_or(&self, key: &str, default: &str) -> String {
        self.env_provider()
            .get(key)
            .unwrap_or_else(|| default.to_string())
    }

    /// Get a required environment variable.
    ///
    /// Returns an error if the variable is not set.
    fn env_require(&self, key: &str) -> Result<String> {
        self.env_provider().get(key).ok_or_else(|| {
            ForgeError::config(format!("Required environment variable '{}' not set", key))
        })
    }

    /// Get an environment variable and parse it to the specified type.
    ///
    /// Returns an error if:
    /// - The variable is not set
    /// - The value cannot be parsed to the target type
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

    /// Get an environment variable and parse it, with a default.
    ///
    /// Returns the default if the variable is not set.
    /// Returns an error only if the variable IS set but cannot be parsed.
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

    /// Check if an environment variable is set.
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
        // Set a test env var
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

        // Cleanup
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

        // Check access tracking
        assert!(provider.was_accessed("API_KEY"));
        assert!(provider.was_accessed("TIMEOUT"));
        assert!(provider.was_accessed("MISSING")); // Even failed lookups are tracked

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

    // Test EnvAccess trait methods using a simple wrapper
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

        // env()
        assert_eq!(ctx.env("PORT"), Some("8080".to_string()));
        assert!(ctx.env("MISSING").is_none());

        // env_or()
        assert_eq!(ctx.env_or("PORT", "3000"), "8080");
        assert_eq!(ctx.env_or("MISSING", "default"), "default");

        // env_require()
        assert_eq!(ctx.env_require("PORT").unwrap(), "8080");
        assert!(ctx.env_require("MISSING").is_err());

        // env_parse()
        let port: u16 = ctx.env_parse("PORT").unwrap();
        assert_eq!(port, 8080);

        let debug: bool = ctx.env_parse("DEBUG").unwrap();
        assert!(debug);

        // Parse error
        let bad: Result<u32> = ctx.env_parse("BAD_NUMBER");
        assert!(bad.is_err());

        // env_parse_or()
        let port: u16 = ctx.env_parse_or("MISSING", 3000).unwrap();
        assert_eq!(port, 3000);

        // env_contains()
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
        // Must not panic — this is the happy path of assert_not_accessed.
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

//! Signals configuration for product analytics and diagnostics.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::default_true;
use super::types::DurationStr;

/// Signals configuration for built-in product analytics and frontend diagnostics.
///
/// Captures user behavior, acquisition channels, feature usage, and frontend
/// errors without cookies or persistent client-side state (GDPR-compliant).
///
/// ```toml
/// [signals]
/// enabled = true
/// auto_capture = true
/// diagnostics = true
/// session_timeout = "30m"
/// retention_days = 90
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SignalsConfig {
    /// Enable the signals pipeline (event ingestion, auto-capture, dashboards).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Auto-capture RPC calls as events without user code.
    #[serde(default = "default_true")]
    pub auto_capture: bool,

    /// Capture frontend errors, failed RPCs, and breadcrumbs for reproduction.
    #[serde(default = "default_true")]
    pub diagnostics: bool,

    /// Inactivity timeout before closing a session (e.g. "30m").
    #[serde(default = "default_session_timeout")]
    pub session_timeout: DurationStr,

    /// Days to retain event data before partition cleanup.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,

    /// Hash client IP + UA into a daily-rotating visitor ID instead of storing raw IP.
    #[serde(default)]
    pub anonymize_ip: bool,

    /// Max events per batch INSERT.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Max interval between flushes of the event buffer (e.g. "5s").
    #[serde(default = "default_flush_interval")]
    pub flush_interval: DurationStr,

    /// Internal mpsc channel capacity for event buffering.
    #[serde(default = "default_channel_capacity")]
    pub channel_capacity: usize,

    /// Function names to exclude from auto-capture (exact match).
    #[serde(default)]
    pub excluded_functions: Vec<String>,

    /// Tag bot traffic via UA detection (visible in dashboard filter).
    #[serde(default = "default_true")]
    pub bot_detection: bool,

    /// Optional path to a MaxMind MMDB file (e.g. GeoLite2-City.mmdb) for
    /// city-level resolution. When omitted, the embedded DB-IP Country Lite
    /// database provides country-level resolution with zero configuration.
    #[serde(default)]
    pub geoip_db_path: Option<String>,
}

impl Default for SignalsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_capture: true,
            diagnostics: true,
            session_timeout: default_session_timeout(),
            retention_days: default_retention_days(),
            anonymize_ip: false,
            batch_size: default_batch_size(),
            flush_interval: default_flush_interval(),
            channel_capacity: default_channel_capacity(),
            excluded_functions: Vec::new(),
            bot_detection: true,
            geoip_db_path: None,
        }
    }
}

fn default_session_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(1800))
}

fn default_retention_days() -> u32 {
    90
}

fn default_batch_size() -> usize {
    100
}

fn default_flush_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(5))
}

fn default_channel_capacity() -> usize {
    10_000
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_config_has_correct_values() {
        let config = SignalsConfig::default();
        assert!(config.enabled);
        assert!(config.auto_capture);
        assert!(config.diagnostics);
        assert_eq!(config.session_timeout.as_secs(), 1800);
        assert_eq!(config.retention_days, 90);
        assert!(!config.anonymize_ip);
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.flush_interval.as_secs(), 5);
        assert!(config.excluded_functions.is_empty());
        assert!(config.bot_detection);
    }

    #[tokio::test]
    async fn deserializes_empty_toml() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            signals: SignalsConfig,
        }

        let from_empty: SignalsConfig = toml::from_str("").unwrap();
        let from_table: Wrapper = toml::from_str("[signals]").unwrap();

        for config in [from_empty, from_table.signals] {
            assert!(config.enabled);
            assert!(config.auto_capture);
            assert!(config.diagnostics);
            assert_eq!(config.session_timeout.as_secs(), 1800);
            assert_eq!(config.retention_days, 90);
            assert!(!config.anonymize_ip);
            assert_eq!(config.batch_size, 100);
            assert_eq!(config.flush_interval.as_secs(), 5);
            assert!(config.excluded_functions.is_empty());
            assert!(config.bot_detection);
        }
    }

    #[tokio::test]
    async fn partial_override_preserves_defaults() {
        let config: SignalsConfig = toml::from_str("enabled = false").unwrap();
        assert!(!config.enabled);
        assert!(config.auto_capture);
        assert!(config.diagnostics);
        assert_eq!(config.session_timeout.as_secs(), 1800);
        assert_eq!(config.retention_days, 90);
        assert!(!config.anonymize_ip);
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.flush_interval.as_secs(), 5);
        assert!(config.excluded_functions.is_empty());
        assert!(config.bot_detection);
    }

    #[tokio::test]
    async fn excluded_functions_from_toml() {
        let config: SignalsConfig =
            toml::from_str(r#"excluded_functions = ["health_check", "readiness"]"#).unwrap();
        assert_eq!(config.excluded_functions, vec!["health_check", "readiness"]);
    }

    #[tokio::test]
    async fn all_fields_overridden() {
        let toml_str = r#"
            enabled = false
            auto_capture = false
            diagnostics = false
            session_timeout = "60m"
            retention_days = 30
            anonymize_ip = true
            batch_size = 500
            flush_interval = "10s"
            excluded_functions = ["ping"]
            bot_detection = false
        "#;
        let config: SignalsConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.enabled);
        assert!(!config.auto_capture);
        assert!(!config.diagnostics);
        assert_eq!(config.session_timeout.as_secs(), 3600);
        assert_eq!(config.retention_days, 30);
        assert!(config.anonymize_ip);
        assert_eq!(config.batch_size, 500);
        assert_eq!(config.flush_interval.as_secs(), 10);
        assert_eq!(config.excluded_functions, vec!["ping"]);
        assert!(!config.bot_detection);
    }
}

//! Forge configuration. Config lives in code (visible to the agent); secret-
//! bearing fields can be filled from `FORGE_*` env vars via [`ForgeConfig::from_env`].

use crate::error::{ForgeError, Result};
use std::time::Duration;

/// Everything `Forge::init` needs. The only required field is `postgres`.
///
/// `Debug` is hand-written to redact the password-bearing connection string; never derive it.
#[non_exhaustive]
#[derive(Clone)]
pub struct ForgeConfig {
    /// Postgres connection string, e.g. `postgres://user:pw@host/db`.
    pub postgres: String,
    /// Maximum pooled connections. Default 10.
    pub max_connections: u32,
    /// How long to wait for a free connection before erroring. Default 30s.
    pub acquire_timeout: Duration,
    /// Apply embedded migrations at init. Default true. When false, init still verifies
    /// the schema is present and refuses to start if it is not; it never migrates lazily.
    pub run_migrations: bool,
    /// Prefix applied to every kv key, so multiple apps can share one database
    /// without colliding. Empty by default. Must not contain `:`.
    pub kv_namespace: String,
    /// Window within which a repeated `enqueue` `dedup_id` is de-duplicated.
    /// Default 5 minutes (SQS FIFO precedent).
    pub queue_dedup_window: Duration,
    /// How long completed (`done`) jobs are retained before the maintenance
    /// sweep purges them. Default 7 days.
    pub queue_retention: Duration,
    /// Whether `ratelimit().check` fails open (allow + warn) on a backend error.
    /// Default true; set false for abuse- or payment-sensitive buckets.
    pub ratelimit_fail_open: bool,
    /// HMAC secret for presigned blob URLs. `None` leaves presigning unconfigured
    /// (the blob CRUD surface still works); set it to enable `presign_*` and the
    /// `blob-router`. Fill from `FORGE_BLOB_SIGNING_SECRET`.
    pub blob_signing_secret: Option<String>,
    /// URL prefix the blob router is mounted at; presigned URLs point here.
    /// Default `/_forge/blob`.
    pub blob_base_url: String,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            postgres: String::new(),
            max_connections: 10,
            acquire_timeout: Duration::from_secs(30),
            run_migrations: true,
            kv_namespace: String::new(),
            queue_dedup_window: Duration::from_secs(5 * 60),
            queue_retention: Duration::from_secs(7 * 24 * 60 * 60),
            ratelimit_fail_open: true,
            blob_signing_secret: None,
            blob_base_url: "/_forge/blob".to_string(),
        }
    }
}

impl ForgeConfig {
    /// Start from a Postgres connection string with defaults for everything else.
    pub fn new(postgres: impl Into<String>) -> Self {
        Self {
            postgres: postgres.into(),
            ..Default::default()
        }
    }

    /// Build from `FORGE_*` environment variables. `FORGE_POSTGRES_URL` is
    /// required; the rest fall back to defaults. Optional overrides:
    /// `FORGE_MAX_CONNECTIONS`, `FORGE_KV_NAMESPACE`.
    pub fn from_env() -> Result<Self> {
        let postgres = std::env::var("FORGE_POSTGRES_URL").map_err(|_| {
            ForgeError::config("FORGE_POSTGRES_URL is not set (required to connect to Postgres)")
        })?;
        let mut cfg = Self::new(postgres);
        if let Ok(v) = std::env::var("FORGE_MAX_CONNECTIONS") {
            cfg.max_connections = v.parse().map_err(|_| {
                ForgeError::config(format!("FORGE_MAX_CONNECTIONS is not a number: {v:?}"))
            })?;
        }
        if let Ok(v) = std::env::var("FORGE_KV_NAMESPACE") {
            cfg.kv_namespace = v;
        }
        if let Ok(v) = std::env::var("FORGE_BLOB_SIGNING_SECRET") {
            cfg.blob_signing_secret = Some(v);
        }
        if let Ok(v) = std::env::var("FORGE_BLOB_BASE_URL") {
            cfg.blob_base_url = v;
        }
        Ok(cfg)
    }

    /// Set the maximum pool size.
    pub fn with_max_connections(mut self, n: u32) -> Self {
        self.max_connections = n;
        self
    }

    /// Set the connection-acquire timeout.
    pub fn with_acquire_timeout(mut self, d: Duration) -> Self {
        self.acquire_timeout = d;
        self
    }

    /// Disable automatic migration at init (the schema is still verified).
    pub fn without_migrations(mut self) -> Self {
        self.run_migrations = false;
        self
    }

    /// Set the kv key namespace.
    pub fn with_kv_namespace(mut self, ns: impl Into<String>) -> Self {
        self.kv_namespace = ns.into();
        self
    }

    /// Set the enqueue dedup window.
    pub fn with_queue_dedup_window(mut self, d: Duration) -> Self {
        self.queue_dedup_window = d;
        self
    }

    /// Set how long completed jobs are retained before the maintenance sweep purges them.
    pub fn with_queue_retention(mut self, d: Duration) -> Self {
        self.queue_retention = d;
        self
    }

    /// Set the ratelimit failure mode (`true` = fail-open, the default).
    pub fn with_ratelimit_fail_open(mut self, fail_open: bool) -> Self {
        self.ratelimit_fail_open = fail_open;
        self
    }

    /// Set the HMAC secret for presigned blob URLs (enables `presign_*`).
    pub fn with_blob_signing_secret(mut self, secret: impl Into<String>) -> Self {
        self.blob_signing_secret = Some(secret.into());
        self
    }

    /// Set the URL prefix the blob router is mounted at.
    pub fn with_blob_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.blob_base_url = base_url.into();
        self
    }

    /// Validate the statically-checkable fields with a precise message;
    /// connection/migration failures surface later in `Forge::init`.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.postgres.trim().is_empty() {
            return Err(ForgeError::config(
                "postgres connection string is empty (set ForgeConfig.postgres or FORGE_POSTGRES_URL)",
            ));
        }
        if self.max_connections == 0 {
            return Err(ForgeError::config("max_connections must be >= 1"));
        }
        // Migrations hold the advisory-lock connection while drawing a second from the same pool;
        // with only one, that deadlocks until the acquire timeout, so require >= 2.
        if self.run_migrations && self.max_connections < 2 {
            return Err(ForgeError::config(
                "running migrations needs max_connections >= 2 (one holds the migration lock \
                 while migrations run); raise it, or set run_migrations=false and migrate out of band",
            ));
        }
        if self.kv_namespace.contains(':') {
            return Err(ForgeError::config(
                "kv_namespace must not contain ':' (it is the reserved namespace separator)",
            ));
        }
        Ok(())
    }
}

impl std::fmt::Debug for ForgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgeConfig")
            .field("postgres", &"<redacted>")
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("run_migrations", &self.run_migrations)
            .field("kv_namespace", &self.kv_namespace)
            .field("queue_dedup_window", &self.queue_dedup_window)
            .field("queue_retention", &self.queue_retention)
            .field("ratelimit_fail_open", &self.ratelimit_fail_open)
            .field(
                "blob_signing_secret",
                &self.blob_signing_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("blob_base_url", &self.blob_base_url)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_connection_string() {
        let cfg = ForgeConfig::new("postgres://user:supersecret@host/db");
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("supersecret"), "password leaked: {dbg}");
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn validate_rejects_empty_dsn() {
        let cfg = ForgeConfig::new("");
        assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn validate_rejects_namespace_with_colon() {
        let cfg = ForgeConfig::new("postgres://x/y").with_kv_namespace("a:b");
        assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn validate_accepts_sane_config() {
        let cfg = ForgeConfig::new("postgres://x/y").with_max_connections(5);
        assert!(cfg.validate().is_ok());
    }
}

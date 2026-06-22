//! Forge configuration. Config lives in code (visible to the agent); secret-
//! bearing fields can be filled from `FORGE_*` env vars via [`ForgeConfig::from_env`].

use crate::error::{ForgeError, Result};
use std::path::PathBuf;
use std::time::Duration;

/// Which backend stores blob bytes. Metadata always lives in Postgres; this only
/// chooses where the object *body* goes.
///
/// This is an `enum` so a later S3/R2/GCS backend is a non-breaking variant add. v1
/// ships two variants: `BYTEA` in Postgres (the default, atomic with surrounding app
/// SQL) and a local filesystem directory (keeps large objects out of the WAL, at the
/// cost of `put` no longer being atomic with app SQL and needing a shared mount for
/// multi-replica deploys).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BlobBackendConfig {
    /// Object bytes in the `forge_blobs.data` `BYTEA` column.
    #[default]
    Postgres,
    /// Object bytes on a local filesystem directory; metadata stays in Postgres.
    Filesystem {
        /// Directory the bytes are written under. Created if missing.
        root: PathBuf,
    },
}

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
    /// App namespace, so multiple apps can share one database without colliding —
    /// applied across **all** primitives: a key prefix (kv/ratelimit/blob), a name
    /// prefix (queue/config/flags), a channel prefix (pubsub), and an `app` column
    /// (sessions/api keys/schedules). Empty by default. Must not contain `:`. (The
    /// field keeps its `kv_namespace` name for backward compatibility.)
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
    /// Which backend stores blob bytes. Default [`BlobBackendConfig::Postgres`].
    pub blob_backend: BlobBackendConfig,
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
            blob_backend: BlobBackendConfig::Postgres,
        }
    }
}

/// Parse a `FORGE_*_SECS` env value into a `Duration`.
fn env_secs(name: &str, v: &str) -> Result<Duration> {
    let secs: f64 = v
        .parse()
        .map_err(|_| ForgeError::config(format!("{name} is not a number: {v:?}")))?;
    Duration::try_from_secs_f64(secs)
        .map_err(|_| ForgeError::config(format!("{name} must be a non-negative number: {v:?}")))
}

/// Parse a `FORGE_*` boolean env value (`true`/`false`/`1`/`0`, case-insensitive).
fn env_bool(name: &str, v: &str) -> Result<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(ForgeError::config(format!(
            "{name} must be a boolean (true/false), got {other:?}"
        ))),
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
        if let Ok(v) = std::env::var("FORGE_ACQUIRE_TIMEOUT_SECS") {
            cfg.acquire_timeout = env_secs("FORGE_ACQUIRE_TIMEOUT_SECS", &v)?;
        }
        if let Ok(v) = std::env::var("FORGE_QUEUE_DEDUP_WINDOW_SECS") {
            cfg.queue_dedup_window = env_secs("FORGE_QUEUE_DEDUP_WINDOW_SECS", &v)?;
        }
        if let Ok(v) = std::env::var("FORGE_QUEUE_RETENTION_SECS") {
            cfg.queue_retention = env_secs("FORGE_QUEUE_RETENTION_SECS", &v)?;
        }
        if let Ok(v) = std::env::var("FORGE_RUN_MIGRATIONS") {
            cfg.run_migrations = env_bool("FORGE_RUN_MIGRATIONS", &v)?;
        }
        if let Ok(v) = std::env::var("FORGE_RATELIMIT_FAIL_OPEN") {
            cfg.ratelimit_fail_open = env_bool("FORGE_RATELIMIT_FAIL_OPEN", &v)?;
        }
        // Portable, binding-neutral backend selection: the same FORGE_* vars drive
        // Rust, Node, and Python with no per-language API. `filesystem` needs a root.
        if let Ok(v) = std::env::var("FORGE_BLOB_BACKEND") {
            match v.to_ascii_lowercase().as_str() {
                "postgres" | "" => {}
                "filesystem" | "fs" => {
                    let root = std::env::var("FORGE_BLOB_FS_ROOT").map_err(|_| {
                        ForgeError::config(
                            "FORGE_BLOB_BACKEND=filesystem requires FORGE_BLOB_FS_ROOT (the blob directory)",
                        )
                    })?;
                    cfg.blob_backend = BlobBackendConfig::Filesystem { root: root.into() };
                }
                other => {
                    return Err(ForgeError::config(format!(
                        "FORGE_BLOB_BACKEND must be 'postgres' or 'filesystem', got {other:?}"
                    )));
                }
            }
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

    /// Select the blob byte-storage backend.
    pub fn with_blob_backend(mut self, backend: BlobBackendConfig) -> Self {
        self.blob_backend = backend;
        self
    }

    /// Store blob bytes on a local filesystem directory (metadata stays in Postgres).
    pub fn with_filesystem_blob(mut self, root: impl Into<PathBuf>) -> Self {
        self.blob_backend = BlobBackendConfig::Filesystem { root: root.into() };
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
        if let BlobBackendConfig::Filesystem { root } = &self.blob_backend
            && root.as_os_str().is_empty()
        {
            return Err(ForgeError::config(
                "filesystem blob backend requires a non-empty root directory",
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
            .field("blob_backend", &self.blob_backend)
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
    fn env_bool_parses_truthy_and_rejects_garbage() {
        assert!(env_bool("X", "true").unwrap());
        assert!(!env_bool("X", "0").unwrap());
        assert!(env_bool("X", "ON").unwrap());
        assert!(matches!(env_bool("X", "maybe"), Err(ForgeError::Config(_))));
    }

    #[test]
    fn env_secs_parses_and_rejects_negative() {
        assert_eq!(env_secs("X", "1.5").unwrap(), Duration::from_millis(1500));
        assert!(matches!(env_secs("X", "-1"), Err(ForgeError::Config(_))));
        assert!(matches!(env_secs("X", "abc"), Err(ForgeError::Config(_))));
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

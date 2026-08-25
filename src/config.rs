use crate::backend::Primitive;
use crate::error::{ForgeError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Server-side ceilings applied to every runtime connection (see [`DatabaseConfig`]).
/// They bound how long a single statement runs, how long it waits on a lock, and how
/// long it may sit idle inside an open transaction, so one wedged query or lock wait
/// can't pin a pooled connection and drain the pool. `Duration::ZERO` on any of them
/// disables that ceiling (Postgres' unlimited default). Migrations set their own,
/// longer limits inline, so these don't constrain them.
const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IDLE_IN_TX_TIMEOUT: Duration = Duration::from_secs(15);

/// Every primitive, for iterating per-feature config (e.g. `[databases.<feature>]`).
const ALL_PRIMITIVES: [Primitive; 8] = [
    Primitive::Kv,
    Primitive::Queue,
    Primitive::Blob,
    Primitive::Auth,
    Primitive::Config,
    Primitive::RateLimit,
    Primitive::Schedule,
    Primitive::Pubsub,
];

/// A Postgres connection target plus its pool sizing. Used for the shared default
/// database and for per-feature overrides (see [`ForgeConfig::feature_databases`]).
///
/// `Debug` is hand-written to redact the password-bearing connection string.
#[derive(Clone)]
pub(crate) struct DatabaseConfig {
    /// Postgres connection string, e.g. `postgres://user:pw@host/db`.
    pub postgres: String,
    /// Maximum pooled connections. Default 10.
    pub max_connections: u32,
    /// How long to wait for a free connection before erroring. Default 30s.
    pub acquire_timeout: Duration,
    /// Server-side `statement_timeout` for runtime connections. Default 15s;
    /// `Duration::ZERO` disables it (Postgres' unlimited default).
    pub statement_timeout: Duration,
    /// Server-side `lock_timeout` for runtime connections. Default 5s; `ZERO` disables.
    pub lock_timeout: Duration,
    /// Server-side `idle_in_transaction_session_timeout` for runtime connections.
    /// Default 15s; `ZERO` disables.
    pub idle_in_transaction_timeout: Duration,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("postgres", &"<redacted>")
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("statement_timeout", &self.statement_timeout)
            .field("lock_timeout", &self.lock_timeout)
            .field(
                "idle_in_transaction_timeout",
                &self.idle_in_transaction_timeout,
            )
            .finish()
    }
}

/// The complete Forge runtime profile. A profile selects every core primitive at once;
/// Forge does not expose a configuration-driven per-primitive backend matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeMode {
    /// The normal durable, cross-process production profile.
    #[default]
    Postgres,
    /// Explicit database-free state for tests and local development. Process-local and
    /// discarded when the last handle is dropped.
    Memory,
}

/// The declared deployment environment used to guard unsafe profile choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Environment {
    #[default]
    Development,
    Test,
    Production,
}

/// Which backend stores blob objects. Filesystem keeps large bodies out of the WAL but
/// makes `put` non-atomic with app SQL and needs a shared mount for multi-replica deploys.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum BlobBackendConfig {
    /// Object bytes in the `forge_blobs.data` `BYTEA` column.
    #[default]
    Postgres,
    /// Object bytes on a local filesystem directory; metadata stays in Postgres.
    Filesystem {
        /// Directory the bytes are written under. Created if missing.
        root: PathBuf,
    },
    /// Object bytes in process memory. Not durable, not shared across replicas.
    Memory,
    /// Object bytes and metadata in an S3-compatible bucket.
    S3(S3BlobConfig),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct S3BlobConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub prefix: String,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub session_token: Option<String>,
    pub path_style: bool,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_retries: u32,
}

impl std::fmt::Debug for S3BlobConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3BlobConfig")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("prefix", &self.prefix)
            .field(
                "access_key",
                &self.access_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "secret_key",
                &self.secret_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .field("path_style", &self.path_style)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

/// The parsed `forge.toml`, holding every resolved customizable point. Constructed only by
/// the TOML loader ([`from_toml_str`](Self::from_toml_str) / [`from_toml_file`](Self::from_toml_file));
/// there is no public builder. The Postgres URL may be empty only when embedded
/// Postgres is selected.
///
/// `Debug` is hand-written to redact the password-bearing connection string; never derive it.
pub(crate) struct ForgeConfig {
    /// The complete runtime profile. PostgreSQL is the default; memory must be explicit.
    pub mode: RuntimeMode,
    /// Deployment environment used to reject accidental memory state in production.
    pub environment: Environment,
    /// Explicit escape hatch for intentionally process-local production experiments.
    pub allow_memory_in_production: bool,
    /// Whether initialization applies pending migrations. Defaults to true outside
    /// production and false in production; `[postgres].auto_migrate` overrides it.
    pub auto_migrate: bool,
    /// Maximum time an explicit or startup migration waits for the advisory lock.
    pub migration_lock_timeout: Duration,
    /// Connection string for Forge's system database: a Postgres database Forge fully
    /// owns, separate from your application's own. Forge creates and migrates its `forge_*`
    /// tables here at [`Forge::init`](crate::Forge::init); nothing else should write to it.
    /// Give Forge its own database or schema; don't point it at your application tables.
    /// May be empty when `embedded` is selected: the embedded server mints the
    /// DSN at init.
    pub postgres: String,
    /// Provision an embedded Postgres server at init instead of connecting to `postgres`
    /// (requires the `embedded` cargo feature; both language bindings ship with it).
    /// A non-empty `postgres` wins over this flag, so `url = "${VAR:-}"` +
    /// `embedded = true` means "use $VAR when set, else run embedded".
    /// Set via `[postgres] embedded`.
    pub embedded: bool,
    /// Where the embedded server keeps its data directory and password file.
    /// Default `.forge/pg`, relative to the working directory. Set via
    /// `[postgres] embedded_dir`.
    pub embedded_dir: PathBuf,
    /// Maximum pooled connections. Default 10. Must be >= 2: init migrates the system
    /// database at startup, holding one connection for the migration lock while a second
    /// runs the SQL.
    pub max_connections: u32,
    /// How long to wait for a free connection before erroring. Default 30s.
    pub acquire_timeout: Duration,
    /// Server-side `statement_timeout` for runtime connections (the system pool and
    /// every per-feature pool inherit it). Default 15s; `Duration::ZERO` disables it.
    pub statement_timeout: Duration,
    /// Server-side `lock_timeout` for runtime connections. Default 5s; `ZERO` disables.
    pub lock_timeout: Duration,
    /// Server-side `idle_in_transaction_session_timeout` for runtime connections.
    /// Default 15s; `ZERO` disables.
    pub idle_in_transaction_timeout: Duration,
    /// App namespace, so multiple apps can share one database without colliding. Applied
    /// across all primitives: a key prefix (kv/ratelimit/blob), a name prefix
    /// (queue/config/flags), a channel prefix (pubsub), and an `app` column (sessions/api
    /// keys/schedules). Empty by default. Must not contain `:`. (Set via `[forge] namespace`.)
    pub kv_namespace: String,
    /// Window within which a repeated `enqueue` `dedup_id` is de-duplicated.
    /// Default 5 minutes (SQS FIFO precedent).
    pub queue_dedup_window: Duration,
    /// How long terminal status rows remain queryable. Default 7 days.
    pub queue_retention: Duration,
    /// How long succeeded job status remains queryable. Defaults to terminal retention.
    pub queue_succeeded_retention: Duration,
    /// How long dead job status remains queryable. Defaults to terminal retention.
    pub queue_dead_retention: Duration,
    /// How long cancelled job status remains queryable. Defaults to terminal retention.
    pub queue_cancelled_retention: Duration,
    /// How long terminal payload bytes remain before maintenance clears them while
    /// preserving status metadata. Default 24 hours.
    pub queue_payload_retention: Duration,
    /// Whether `ratelimit().check` fails open (allow + warn) on a backend error.
    /// Default true; set false for abuse- or payment-sensitive buckets.
    pub ratelimit_fail_open: bool,
    /// HMAC secret for presigned blob URLs. `None` leaves presigning unconfigured
    /// (the blob CRUD surface still works); set it to enable `presign_*` and
    /// `verify_presigned`. Set via `[blob] signing_secret` (typically `${...}`).
    pub blob_signing_secret: Option<String>,
    /// URL prefix presigned blob URLs point at (where the host app serves them).
    /// Default `/api/files`.
    pub blob_base_url: String,
    /// Which backend stores blob bytes. Default [`BlobBackendConfig::Postgres`].
    pub blob_backend: BlobBackendConfig,
    /// Per-feature database overrides. A primitive listed here gets its own dedicated
    /// connection pool to the configured Postgres database, isolated from the system pool
    /// and every other feature, so one feature exhausting its connections can't starve the
    /// rest and a feature can live on a different server. Each distinct database is migrated
    /// at init like the system database. Absent primitives use the system pool built from
    /// `postgres` / `max_connections` / `acquire_timeout`. Empty by default. Set via
    /// `[databases.<feature>]`.
    pub feature_databases: HashMap<Primitive, DatabaseConfig>,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            mode: RuntimeMode::Postgres,
            environment: Environment::Development,
            allow_memory_in_production: false,
            auto_migrate: true,
            migration_lock_timeout: Duration::from_secs(30),
            postgres: String::new(),
            embedded: false,
            embedded_dir: PathBuf::from(".forge/pg"),
            max_connections: 10,
            acquire_timeout: Duration::from_secs(30),
            statement_timeout: DEFAULT_STATEMENT_TIMEOUT,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            idle_in_transaction_timeout: DEFAULT_IDLE_IN_TX_TIMEOUT,
            kv_namespace: String::new(),
            queue_dedup_window: Duration::from_secs(5 * 60),
            queue_retention: Duration::from_secs(7 * 24 * 60 * 60),
            queue_succeeded_retention: Duration::from_secs(7 * 24 * 60 * 60),
            queue_dead_retention: Duration::from_secs(7 * 24 * 60 * 60),
            queue_cancelled_retention: Duration::from_secs(7 * 24 * 60 * 60),
            queue_payload_retention: Duration::from_secs(24 * 60 * 60),
            ratelimit_fail_open: true,
            blob_signing_secret: None,
            blob_base_url: "/api/files".to_string(),
            blob_backend: BlobBackendConfig::Postgres,
            feature_databases: HashMap::new(),
        }
    }
}

/// Validate one database target. `label` is the field/feature name for the message
/// (`"postgres"` for the system database, the primitive name for an override).
/// `migrates` is true when init runs migrations against this distinct target.
fn validate_database(db: &DatabaseConfig, migrates: bool, label: &str) -> Result<()> {
    if db.postgres.trim().is_empty() {
        return Err(ForgeError::config(format!(
            "{label} connection string is empty (set the connection URL for this database)"
        )));
    }
    validate_pool_shape(db, migrates, label)
}

/// The pool-sizing half of [`validate_database`], usable alone for the embedded
/// system database, whose DSN is minted at init rather than configured.
fn validate_pool_shape(db: &DatabaseConfig, migrates: bool, label: &str) -> Result<()> {
    if db.max_connections == 0 {
        return Err(ForgeError::config(format!(
            "{label} max_connections must be >= 1"
        )));
    }
    // Init migrates this target at startup, holding the advisory-lock connection while
    // drawing a second from the same pool; with only one, that deadlocks until the
    // acquire timeout, so require >= 2.
    if migrates && db.max_connections < 2 {
        return Err(ForgeError::config(format!(
            "{label} max_connections must be >= 2: Forge migrates this database at startup, \
             holding one connection for the migration lock while a second runs the SQL"
        )));
    }
    Ok(())
}

/// A non-negative number of seconds from TOML into a `Duration`.
fn dur_secs(name: &str, secs: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(secs).map_err(|_| {
        ForgeError::config(format!(
            "{name} must be a non-negative number of seconds, got {secs}"
        ))
    })
}

/// Resolve a `[databases.<name>]` key to a primitive, with a precise error listing the
/// valid names rather than silently dropping an unknown table.
fn parse_primitive(name: &str) -> Result<Primitive> {
    ALL_PRIMITIVES
        .into_iter()
        .find(|p| p.as_str().eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            ForgeError::config(format!(
                "unknown primitive {name:?} in [databases]; expected one of \
                 kv, queue, blob, auth, config, ratelimit, schedule, pubsub"
            ))
        })
}

fn parse_mode(name: &str, val: &str) -> Result<RuntimeMode> {
    match val.trim().to_ascii_lowercase().as_str() {
        "postgres" => Ok(RuntimeMode::Postgres),
        "memory" => Ok(RuntimeMode::Memory),
        other => Err(ForgeError::config(format!(
            "{name} must be 'postgres' or 'memory', got {other:?}"
        ))),
    }
}

fn parse_environment(val: &str) -> Result<Environment> {
    match val.trim().to_ascii_lowercase().as_str() {
        "development" => Ok(Environment::Development),
        "test" => Ok(Environment::Test),
        "production" => Ok(Environment::Production),
        other => Err(ForgeError::config(format!(
            "forge.environment must be 'development', 'test', or 'production', got {other:?}"
        ))),
    }
}

/// Substitute `${VAR}` / `${VAR:-default}` references using `lookup`. A missing variable
/// with no default is a hard error: config must fail loud, never resolve to "". `lookup`
/// is injected so substitution is unit-testable without touching the process environment
/// (the crate forbids the `unsafe` that `set_var` now requires).
fn interpolate_with(input: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            ForgeError::config(format!("unterminated `${{` in config value: {input:?}"))
        })?;
        let (name, default) = match after[..end].split_once(":-") {
            Some((n, d)) => (n.trim(), Some(d)),
            None => (after[..end].trim(), None),
        };
        let resolved = match lookup(name) {
            Some(v) => v,
            None => default.map(str::to_string).ok_or_else(|| {
                ForgeError::config(format!(
                    "config references ${{{name}}} but {name} is not set in the environment \
                     (use ${{{name}:-default}} to supply a fallback)"
                ))
            })?,
        };
        out.push_str(&resolved);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Walk a parsed TOML document and interpolate every string leaf from the environment.
/// Interpolation happens on values only, so a variable's contents can never inject TOML
/// structure (a `${VAR}` holding `"] [evil"` stays a plain string).
fn interpolate_value(value: &mut toml::Value) -> Result<()> {
    match value {
        toml::Value::String(s) => *s = interpolate_with(s, |k| std::env::var(k).ok())?,
        toml::Value::Array(items) => {
            for item in items {
                interpolate_value(item)?;
            }
        }
        toml::Value::Table(table) => {
            for (_, v) in table.iter_mut() {
                interpolate_value(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The TOML schema, mirroring [`ForgeConfig`] field-for-field. `deny_unknown_fields` turns
/// a typo'd key into a precise error instead of a silently ignored setting.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlConfig {
    #[serde(default)]
    postgres: TomlPostgres,
    #[serde(default)]
    forge: TomlForge,
    #[serde(default)]
    queue: TomlQueue,
    #[serde(default)]
    ratelimit: TomlRatelimit,
    #[serde(default)]
    blob: TomlBlob,
    #[serde(default)]
    databases: HashMap<String, TomlDatabase>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlPostgres {
    url: Option<String>,
    embedded: Option<bool>,
    embedded_dir: Option<String>,
    max_connections: Option<u32>,
    acquire_timeout_secs: Option<f64>,
    statement_timeout_ms: Option<u64>,
    lock_timeout_ms: Option<u64>,
    idle_in_transaction_timeout_ms: Option<u64>,
    auto_migrate: Option<bool>,
    migration_lock_timeout_secs: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlForge {
    namespace: Option<String>,
    mode: Option<String>,
    environment: Option<String>,
    allow_memory_in_production: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlQueue {
    dedup_window_secs: Option<f64>,
    terminal_retention_secs: Option<f64>,
    succeeded_retention_secs: Option<f64>,
    dead_retention_secs: Option<f64>,
    cancelled_retention_secs: Option<f64>,
    payload_retention_secs: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlRatelimit {
    fail_open: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlBlob {
    backend: Option<String>,
    fs_root: Option<String>,
    signing_secret: Option<String>,
    base_url: Option<String>,
    bucket: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
    prefix: Option<String>,
    access_key: Option<String>,
    secret_key: Option<String>,
    session_token: Option<String>,
    path_style: Option<bool>,
    connect_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    max_retries: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlDatabase {
    url: String,
    max_connections: Option<u32>,
    acquire_timeout_secs: Option<f64>,
    statement_timeout_ms: Option<u64>,
    lock_timeout_ms: Option<u64>,
    idle_in_transaction_timeout_ms: Option<u64>,
}

impl ForgeConfig {
    /// Load configuration from a TOML file. See [`from_toml_str`](Self::from_toml_str)
    /// for the interpolation semantics.
    pub(crate) fn from_toml_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            ForgeError::config(format!(
                "could not read config file {}: {e}",
                path.display()
            ))
        })?;
        Self::from_toml_str(&text)
    }

    /// Parse configuration from TOML text. String values may embed `${VAR}` /
    /// `${VAR:-default}`, interpolated from the environment at load (a missing variable with
    /// no default is a hard error: config fails loud, never resolves to empty). Unknown keys
    /// are rejected with a precise error.
    pub(crate) fn from_toml_str(s: &str) -> Result<Self> {
        let mut value: toml::Value = toml::from_str(s)
            .map_err(|e| ForgeError::config(format!("invalid TOML config: {e}")))?;
        interpolate_value(&mut value)?;
        let parsed = TomlConfig::deserialize(value).map_err(|e| {
            ForgeError::config(format!("config does not match the expected schema: {e}"))
        })?;
        let mut cfg = Self::default();
        cfg.apply_toml(parsed)?;
        Ok(cfg)
    }

    /// Map a parsed TOML document onto the config. Only keys present in the file are
    /// touched; everything else keeps its default.
    fn apply_toml(&mut self, t: TomlConfig) -> Result<()> {
        let forge = t.forge;
        if let Some(mode) = forge.mode {
            self.mode = parse_mode("forge.mode", &mode)?;
        }
        if let Some(environment) = forge.environment {
            self.environment = parse_environment(&environment)?;
        }
        if let Some(allow) = forge.allow_memory_in_production {
            self.allow_memory_in_production = allow;
        }
        if let Some(ns) = forge.namespace {
            self.kv_namespace = ns;
        }

        let p = t.postgres;
        self.auto_migrate = p
            .auto_migrate
            .unwrap_or(self.environment != Environment::Production);
        if let Some(url) = p.url {
            self.postgres = url;
        }
        if let Some(b) = p.embedded {
            self.embedded = b;
        }
        if let Some(dir) = p.embedded_dir {
            self.embedded_dir = PathBuf::from(dir);
        }
        if let Some(n) = p.max_connections {
            self.max_connections = n;
        }
        if let Some(secs) = p.acquire_timeout_secs {
            self.acquire_timeout = dur_secs("postgres.acquire_timeout_secs", secs)?;
        }
        if let Some(ms) = p.statement_timeout_ms {
            self.statement_timeout = Duration::from_millis(ms);
        }
        if let Some(ms) = p.lock_timeout_ms {
            self.lock_timeout = Duration::from_millis(ms);
        }
        if let Some(ms) = p.idle_in_transaction_timeout_ms {
            self.idle_in_transaction_timeout = Duration::from_millis(ms);
        }
        if let Some(secs) = p.migration_lock_timeout_secs {
            self.migration_lock_timeout = dur_secs("postgres.migration_lock_timeout_secs", secs)?;
        }

        if let Some(secs) = t.queue.dedup_window_secs {
            self.queue_dedup_window = dur_secs("queue.dedup_window_secs", secs)?;
        }
        if let Some(secs) = t.queue.terminal_retention_secs {
            self.queue_retention = dur_secs("queue.terminal_retention_secs", secs)?;
            self.queue_succeeded_retention = self.queue_retention;
            self.queue_dead_retention = self.queue_retention;
            self.queue_cancelled_retention = self.queue_retention;
        }
        if let Some(secs) = t.queue.succeeded_retention_secs {
            self.queue_succeeded_retention = dur_secs("queue.succeeded_retention_secs", secs)?;
        }
        if let Some(secs) = t.queue.dead_retention_secs {
            self.queue_dead_retention = dur_secs("queue.dead_retention_secs", secs)?;
        }
        if let Some(secs) = t.queue.cancelled_retention_secs {
            self.queue_cancelled_retention = dur_secs("queue.cancelled_retention_secs", secs)?;
        }
        if let Some(secs) = t.queue.payload_retention_secs {
            self.queue_payload_retention = dur_secs("queue.payload_retention_secs", secs)?;
        }
        if let Some(b) = t.ratelimit.fail_open {
            self.ratelimit_fail_open = b;
        }

        let blob = t.blob;
        if let Some(secret) = blob.signing_secret {
            self.blob_signing_secret = Some(secret);
        }
        if let Some(url) = blob.base_url {
            self.blob_base_url = url;
        }
        if let Some(backend) = blob.backend {
            match backend.to_ascii_lowercase().as_str() {
                "postgres" | "" => self.blob_backend = BlobBackendConfig::Postgres,
                "memory" => self.blob_backend = BlobBackendConfig::Memory,
                "filesystem" | "fs" => {
                    let root = blob.fs_root.ok_or_else(|| {
                        ForgeError::config(
                            "blob.backend = \"fs\" requires blob.fs_root (the blob directory)",
                        )
                    })?;
                    self.blob_backend = BlobBackendConfig::Filesystem { root: root.into() };
                }
                "s3" => {
                    let bucket = blob.bucket.ok_or_else(|| {
                        ForgeError::config("blob.backend = \"s3\" requires blob.bucket")
                    })?;
                    let access_key = blob.access_key;
                    let secret_key = blob.secret_key;
                    if access_key.is_some() != secret_key.is_some() {
                        return Err(ForgeError::config(
                            "blob.access_key and blob.secret_key must be configured together",
                        ));
                    }
                    self.blob_backend = BlobBackendConfig::S3(S3BlobConfig {
                        bucket,
                        region: blob.region.unwrap_or_else(|| "us-east-1".to_string()),
                        endpoint: blob.endpoint,
                        prefix: blob
                            .prefix
                            .unwrap_or_default()
                            .trim_matches('/')
                            .to_string(),
                        access_key,
                        secret_key,
                        session_token: blob.session_token,
                        path_style: blob.path_style.unwrap_or(false),
                        connect_timeout: dur_secs(
                            "blob.connect_timeout_secs",
                            blob.connect_timeout_secs.unwrap_or(3.0),
                        )?,
                        request_timeout: dur_secs(
                            "blob.request_timeout_secs",
                            blob.request_timeout_secs.unwrap_or(30.0),
                        )?,
                        max_retries: blob.max_retries.unwrap_or(3).min(10),
                    });
                }
                other => {
                    return Err(ForgeError::config(format!(
                        "blob.backend must be 'postgres', 'memory', 'fs', or 's3', got {other:?}"
                    )));
                }
            }
        } else if self.mode == RuntimeMode::Memory {
            self.blob_backend = BlobBackendConfig::Memory;
        }

        // [databases.<primitive>] gives one feature its own pool/server. Pool sizing and
        // timeouts inherit the top-level values unless the table sets its own.
        for (name, db) in t.databases {
            let feature = parse_primitive(&name)?;
            let mut dc = DatabaseConfig {
                postgres: db.url,
                max_connections: db.max_connections.unwrap_or(self.max_connections),
                acquire_timeout: self.acquire_timeout,
                statement_timeout: self.statement_timeout,
                lock_timeout: self.lock_timeout,
                idle_in_transaction_timeout: self.idle_in_transaction_timeout,
            };
            if let Some(secs) = db.acquire_timeout_secs {
                dc.acquire_timeout =
                    dur_secs(&format!("databases.{name}.acquire_timeout_secs"), secs)?;
            }
            if let Some(ms) = db.statement_timeout_ms {
                dc.statement_timeout = Duration::from_millis(ms);
            }
            if let Some(ms) = db.lock_timeout_ms {
                dc.lock_timeout = Duration::from_millis(ms);
            }
            if let Some(ms) = db.idle_in_transaction_timeout_ms {
                dc.idle_in_transaction_timeout = Duration::from_millis(ms);
            }
            self.feature_databases.insert(feature, dc);
        }
        Ok(())
    }

    /// The system database: the one Forge owns and every non-overridden feature uses, built
    /// from the top-level `postgres` / `max_connections` / `acquire_timeout` fields.
    pub(crate) fn system_database(&self) -> DatabaseConfig {
        DatabaseConfig {
            postgres: self.postgres.clone(),
            max_connections: self.max_connections,
            acquire_timeout: self.acquire_timeout,
            statement_timeout: self.statement_timeout,
            lock_timeout: self.lock_timeout,
            idle_in_transaction_timeout: self.idle_in_transaction_timeout,
        }
    }

    /// The effective database for a feature: its override if present, else the system one.
    pub(crate) fn database_for(&self, feature: Primitive) -> DatabaseConfig {
        self.feature_databases
            .get(&feature)
            .cloned()
            .unwrap_or_else(|| self.system_database())
    }

    /// Whether init should boot an embedded server: the flag is set and no explicit
    /// connection string outranks it.
    pub(crate) fn use_embedded(&self) -> bool {
        self.embedded && self.postgres.trim().is_empty()
    }

    /// Validate the statically-checkable fields with a precise message;
    /// connection/migration failures surface later in `Forge::init`.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.mode == RuntimeMode::Memory {
            if self.environment == Environment::Production && !self.allow_memory_in_production {
                return Err(ForgeError::config(
                    "forge.mode = \"memory\" is disabled in production; use PostgreSQL or set forge.allow_memory_in_production = true explicitly",
                ));
            }
            if !self.postgres.trim().is_empty() || self.embedded {
                return Err(ForgeError::config(
                    "memory mode cannot configure postgres.url or postgres.embedded",
                ));
            }
            if !self.feature_databases.is_empty() {
                return Err(ForgeError::config(
                    "memory mode cannot configure per-primitive PostgreSQL databases",
                ));
            }
            if !matches!(self.blob_backend, BlobBackendConfig::Memory) {
                return Err(ForgeError::config(
                    "memory mode requires blob.backend = \"memory\" (or omit blob.backend)",
                ));
            }
        } else {
            if matches!(self.blob_backend, BlobBackendConfig::Memory) {
                return Err(ForgeError::config(
                    "PostgreSQL mode cannot select an independently memory-backed blob store; use forge.mode = \"memory\" for the complete process-local profile",
                ));
            }
            // Automatic migration holds one pool connection for the advisory lock while
            // another applies SQL. Explicit migration uses its own two-connection pool.
            // With an embedded server the DSN is minted at init, so only the pool shape
            // is statically checkable.
            if self.use_embedded() {
                validate_pool_shape(&self.system_database(), self.auto_migrate, "postgres")?;
            } else {
                validate_database(&self.system_database(), self.auto_migrate, "postgres")?;
            }
            for (feature, db) in &self.feature_databases {
                // A feature override on the same target as the system database is
                // deduplicated in `init` (the system pool migrates it), so it never holds the
                // migration lock and a size-1 bulkhead pool is legitimate. An override on a
                // distinct target is migrated through its own pool and needs >= 2.
                let migrates = self.auto_migrate && db.postgres != self.postgres;
                validate_database(db, migrates, feature.as_str())?;
            }
        }
        crate::util::validate_namespace(&self.kv_namespace)?;
        if self
            .blob_signing_secret
            .as_deref()
            .is_some_and(|secret| secret.trim().is_empty())
        {
            return Err(ForgeError::config(
                "blob.signing_secret must not be empty or whitespace",
            ));
        }
        if let BlobBackendConfig::Filesystem { root } = &self.blob_backend
            && root.as_os_str().is_empty()
        {
            return Err(ForgeError::config(
                "filesystem blob backend requires a non-empty root directory",
            ));
        }
        if let BlobBackendConfig::S3(config) = &self.blob_backend {
            if config.bucket.trim().is_empty() {
                return Err(ForgeError::config("S3 blob bucket must not be empty"));
            }
            if config.region.trim().is_empty() {
                return Err(ForgeError::config("S3 blob region must not be empty"));
            }
            if config.connect_timeout.is_zero() || config.request_timeout.is_zero() {
                return Err(ForgeError::config("S3 blob timeouts must be positive"));
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ForgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgeConfig")
            .field("mode", &self.mode)
            .field("environment", &self.environment)
            .field(
                "allow_memory_in_production",
                &self.allow_memory_in_production,
            )
            .field("auto_migrate", &self.auto_migrate)
            .field("migration_lock_timeout", &self.migration_lock_timeout)
            .field("postgres", &"<redacted>")
            .field("embedded", &self.embedded)
            .field("embedded_dir", &self.embedded_dir)
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("statement_timeout", &self.statement_timeout)
            .field("lock_timeout", &self.lock_timeout)
            .field(
                "idle_in_transaction_timeout",
                &self.idle_in_transaction_timeout,
            )
            .field("kv_namespace", &self.kv_namespace)
            .field("queue_dedup_window", &self.queue_dedup_window)
            .field("queue_retention", &self.queue_retention)
            .field("queue_payload_retention", &self.queue_payload_retention)
            .field("queue_succeeded_retention", &self.queue_succeeded_retention)
            .field("queue_dead_retention", &self.queue_dead_retention)
            .field("queue_cancelled_retention", &self.queue_cancelled_retention)
            .field("ratelimit_fail_open", &self.ratelimit_fail_open)
            .field(
                "blob_signing_secret",
                &self.blob_signing_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("blob_base_url", &self.blob_base_url)
            .field("blob_backend", &self.blob_backend)
            .field("feature_databases", &self.feature_databases)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Parse a minimal config with just the system database URL set.
    fn cfg(url: &str) -> ForgeConfig {
        ForgeConfig::from_toml_str(&format!("[postgres]\nurl = \"{url}\"\n")).unwrap()
    }

    #[test]
    fn debug_redacts_connection_string() {
        let cfg = cfg("postgres://user:supersecret@host/db");
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("supersecret"), "password leaked: {dbg}");
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn validate_rejects_empty_dsn() {
        let cfg = cfg("");
        assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn validate_rejects_namespace_with_colon() {
        let cfg = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://x/y\"\n[forge]\nnamespace = \"a:b\"\n",
        )
        .unwrap();
        assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn validate_rejects_empty_blob_signing_secret() {
        for secret in ["", "   "] {
            let cfg = ForgeConfig::from_toml_str(&format!(
                "[postgres]\nurl = \"postgres://x/y\"\n[blob]\nsigning_secret = {secret:?}\n"
            ))
            .unwrap();
            assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));
        }
    }

    #[test]
    fn validate_accepts_sane_config() {
        let cfg = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://x/y\"\nmax_connections = 5\n",
        )
        .unwrap();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn feature_database_overrides_default_and_isolates_the_rest() {
        let cfg = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://default/db\"\n\
             [databases.kv]\nurl = \"postgres://kv-server/db\"\nmax_connections = 3\n",
        )
        .unwrap();
        // kv resolves to its override; an un-overridden feature keeps the default.
        assert_eq!(
            cfg.database_for(Primitive::Kv).postgres,
            "postgres://kv-server/db"
        );
        assert_eq!(cfg.database_for(Primitive::Kv).max_connections, 3);
        assert_eq!(
            cfg.database_for(Primitive::Queue).postgres,
            "postgres://default/db"
        );
    }

    #[test]
    fn validate_rejects_bad_feature_database() {
        let cfg = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://default/db\"\n[databases.kv]\nurl = \"   \"\n",
        )
        .unwrap();
        assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));

        let cfg = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://default/db\"\n\
             [databases.kv]\nurl = \"postgres://kv/db\"\nmax_connections = 0\n",
        )
        .unwrap();
        assert!(matches!(cfg.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn system_database_always_requires_two_connections() {
        // Init always migrates the system database, so a size-1 system pool is rejected.
        let one = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://default/db\"\nmax_connections = 1\n",
        )
        .unwrap();
        assert!(matches!(one.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn interpolation_resolves_and_supports_defaults() {
        let env: HashMap<&str, &str> = [("DB_HOST", "db.internal")].into_iter().collect();
        let lookup = |k: &str| env.get(k).map(|s| s.to_string());

        assert_eq!(
            interpolate_with("postgres://${DB_HOST}/app", lookup).unwrap(),
            "postgres://db.internal/app"
        );
        // Missing var falls back to the `:-default`.
        assert_eq!(
            interpolate_with("${MISSING:-/api/files}", lookup).unwrap(),
            "/api/files"
        );
        // A set var wins over its default.
        assert_eq!(
            interpolate_with("${DB_HOST:-fallback}", lookup).unwrap(),
            "db.internal"
        );
    }

    #[test]
    fn interpolation_missing_var_without_default_is_loud_error() {
        let err = interpolate_with("${NOPE}", |_| None);
        assert!(
            matches!(err, Err(ForgeError::Config(_))),
            "missing var must error, not empty: {err:?}"
        );
        // And an unterminated reference is rejected rather than silently passed through.
        assert!(matches!(
            interpolate_with("${UNTERMINATED", |_| Some("x".into())),
            Err(ForgeError::Config(_))
        ));
    }

    #[test]
    fn from_toml_str_maps_fields() {
        let cfg = ForgeConfig::from_toml_str(
            r#"
            [postgres]
            url = "postgres://localhost/forge"
            max_connections = 7
            statement_timeout_ms = 9000

            [forge]
            namespace = "myapp"

            [queue]
            dedup_window_secs = 120
            payload_retention_secs = 3600
            terminal_retention_secs = 7200
            dead_retention_secs = 14400
            cancelled_retention_secs = 21600

            [ratelimit]
            fail_open = false

            [blob]
            backend = "fs"
            fs_root = "/data/blobs"
            base_url = "/files"

            [databases.kv]
            url = "postgres://kv-server/forge"
            max_connections = 3
            "#,
        )
        .unwrap();

        assert_eq!(cfg.postgres, "postgres://localhost/forge");
        assert_eq!(cfg.max_connections, 7);
        assert_eq!(cfg.statement_timeout, Duration::from_millis(9000));
        assert_eq!(cfg.kv_namespace, "myapp");
        assert_eq!(cfg.queue_dedup_window, Duration::from_secs(120));
        assert_eq!(cfg.queue_payload_retention, Duration::from_secs(3600));
        assert_eq!(cfg.queue_retention, Duration::from_secs(7200));
        assert_eq!(cfg.queue_succeeded_retention, Duration::from_secs(7200));
        assert_eq!(cfg.queue_dead_retention, Duration::from_secs(14400));
        assert_eq!(cfg.queue_cancelled_retention, Duration::from_secs(21600));
        assert!(!cfg.ratelimit_fail_open);
        assert_eq!(
            cfg.blob_backend,
            BlobBackendConfig::Filesystem {
                root: "/data/blobs".into()
            }
        );
        assert_eq!(cfg.blob_base_url, "/files");
        let kv = cfg.database_for(Primitive::Kv);
        assert_eq!(kv.postgres, "postgres://kv-server/forge");
        assert_eq!(kv.max_connections, 3);
        // An un-overridden feature still resolves to the system database.
        assert_eq!(
            cfg.database_for(Primitive::Queue).postgres,
            "postgres://localhost/forge"
        );
    }

    #[test]
    fn from_toml_str_rejects_unknown_key() {
        let err = ForgeConfig::from_toml_str("[postgres]\nurl = \"x\"\nbogus = 1\n");
        assert!(
            matches!(err, Err(ForgeError::Config(_))),
            "typo'd key must be rejected: {err:?}"
        );
    }

    #[test]
    fn from_toml_str_fs_blob_requires_root() {
        let err = ForgeConfig::from_toml_str("[blob]\nbackend = \"fs\"\n");
        assert!(
            matches!(err, Err(ForgeError::Config(_))),
            "fs backend without root must error: {err:?}"
        );
    }

    #[test]
    fn from_toml_str_rejects_unknown_database_primitive() {
        let err = ForgeConfig::from_toml_str("[databases.nope]\nurl = \"postgres://x/y\"\n");
        assert!(
            matches!(err, Err(ForgeError::Config(_))),
            "unknown primitive must error: {err:?}"
        );
    }

    #[test]
    fn memory_mode_is_one_explicit_profile() {
        let cfg =
            ForgeConfig::from_toml_str("[forge]\nmode = \"memory\"\nenvironment = \"test\"\n")
                .unwrap();
        assert_eq!(cfg.mode, RuntimeMode::Memory);
        assert_eq!(cfg.blob_backend, BlobBackendConfig::Memory);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn memory_mode_rejects_production_and_database_combinations() {
        let production = ForgeConfig::from_toml_str(
            "[forge]\nmode = \"memory\"\nenvironment = \"production\"\n",
        )
        .unwrap();
        assert!(matches!(production.validate(), Err(ForgeError::Config(_))));

        let mixed = ForgeConfig::from_toml_str(
            "[forge]\nmode = \"memory\"\n[postgres]\nurl = \"postgres://x/y\"\n",
        )
        .unwrap();
        assert!(matches!(mixed.validate(), Err(ForgeError::Config(_))));
    }

    #[test]
    fn from_toml_str_with_interpolated_secret_parses() {
        // No env set, so rely on a literal default to keep the test hermetic.
        let cfg = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://localhost/forge\"\n\
             [blob]\nsigning_secret = \"${FORGE_TEST_UNSET_SECRET:-devsecret}\"\n",
        )
        .unwrap();
        assert_eq!(cfg.blob_signing_secret.as_deref(), Some("devsecret"));
    }

    #[test]
    fn feature_two_connection_floor_only_applies_to_distinct_targets() {
        // A size-1 feature override on the SAME target as the system database is migrated
        // by the system pool, not its own, so a bulkhead pool of one is legitimate.
        let same_server = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://default/db\"\n\
             [databases.kv]\nurl = \"postgres://default/db\"\nmax_connections = 1\n",
        )
        .unwrap();
        assert!(same_server.validate().is_ok());

        // A size-1 override on a DISTINCT target is migrated through its own pool, needs >= 2.
        let other_server = ForgeConfig::from_toml_str(
            "[postgres]\nurl = \"postgres://default/db\"\n\
             [databases.kv]\nurl = \"postgres://other/db\"\nmax_connections = 1\n",
        )
        .unwrap();
        assert!(matches!(
            other_server.validate(),
            Err(ForgeError::Config(_))
        ));
    }
}

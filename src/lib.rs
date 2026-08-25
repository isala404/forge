#![forbid(unsafe_code)]

pub mod auth;
pub mod backend;
pub mod blob;
pub mod config_store;
pub mod error;
pub mod interop;
pub mod invalidation;
pub mod kv;
#[cfg(feature = "openfeature")]
pub mod openfeature;
mod outbox;
pub mod pubsub;
pub mod queue;
pub mod ratelimit;
pub mod schedule;
pub mod scoping;
pub mod trace_context;
pub mod typed;
pub mod types;

mod clock;
mod config;
mod lifecycle;
mod obs;
mod util;

mod pg;

#[cfg(test)]
mod property_tests;

pub mod testing;

#[cfg(any(feature = "conformance", test))]
pub mod conformance;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use config::{BlobBackendConfig, ForgeConfig};
pub use config::{Environment, RuntimeMode};

pub use auth::{
    ApiKey, ApiKeyInfo, ApiKeyOpts, ApiKeySecret, Auth, OneTimeToken, PhcString, Session,
    SessionOpts, SessionToken, TokenConsumption,
};
pub use backend::{
    AuthBackend, BackendCapabilities, BackendInfo, BackendLifecycle, BlobBackend,
    ConfigStoreBackend, KvBackend, Primitive, PubsubBackend, QueueBackend, RateLimitBackend,
    ScheduleBackend,
};
pub use blob::{
    Blob, BlobInfo, BlobReader, BlobSummary, ConditionalGet, ListPage, MultipartPart,
    MultipartUpload, NativePresign, ProxyPresign, PutOpts, PutPrecondition, S3Encryption,
};
pub use config_store::{
    ConfigEntry, ConfigExt, ConfigSnapshot, ConfigStore, EvalCtx, FlagEvaluation,
    FlagEvaluationEntry, FlagEvaluationRequest, FlagRule, SnapshotSecretHandling,
};
pub use error::{ForgeError, Result};
pub use interop::{
    CloudEvent, EnvConfigMapping, decode_cloud_event, encode_cloud_event, export_env_config,
    import_env_config,
};
pub use invalidation::{INVALIDATION_SCHEMA_JSON, InvalidationEvent};
pub use kv::{Kv, SetMode, SetOpts};
pub use obs::{
    BackendHealth, DiagnosticCheck, DiagnosticsReport, HealthReport, MetricSample, ProbeOptions,
};
pub use outbox::{OUTBOX_SCHEMA_SQL, OUTBOX_TABLE, OutboxRelayOpts, OutboxRelayReport};
pub use pg::{MigrationReport, MigrationState};
pub use pubsub::{Pubsub, Subscription};
pub use queue::worker::{WorkerBuilder, WorkerFailure, WorkerFailureKind};
pub use queue::{
    ArtifactRef, BatchEnqueueItem, BatchEnqueueResult, DeadLetterInfo, DeadLetterPage, DequeueOpts,
    EnqueueOpts, Job, JobCancellation, JobId, JobState, JobStatus, JobStatusFilter, JobStatusPage,
    MAX_DEQUEUE_BATCH, MAX_ENQUEUE_BATCH, NackOpts, Priority, Queue, QueueDepth, QueueEnvelope,
    QueueStats, RedriveBatchResult, RedriveDedupPolicy, RedriveOpts,
};
pub use ratelimit::{
    Algo, Decision, FailMode, Limit, RateLimit, Reservation, ReservationState, parse_reservation_id,
};
pub use schedule::{
    MisfirePolicy, Schedule, ScheduleInfo, ScheduleKind, ScheduleOpts, SchedulerDiagnostics,
};
pub use scoping::{
    ParsedScope, parse_scoped_name, scope_blob_key, scope_kv_key, scope_rate_limit_subject,
    scope_topic,
};
pub use trace_context::TraceContext;
pub use typed::{
    BlobKey, ConfigKey, ConfigTyped, KvKey, KvTyped, PubsubTyped, QueueName, QueuePayload,
    QueueTyped, RateBucket, RateSubject, Topic, TypedJob, TypedSubscription,
};
pub use types::Cursor;

// Re-exported so callers needn't depend on `bytes` directly.
pub use bytes::Bytes;

#[cfg(feature = "otel")]
pub use obs::install_otlp;

/// The single handle an application holds. Cheap to clone (`Arc` inside), `Send + Sync`.
/// Construct once with [`Forge::init`]; it owns the pool and every primitive.
#[derive(Clone)]
pub struct Forge {
    inner: Arc<ForgeInner>,
}

struct ForgeInner {
    kv: Arc<dyn Kv>,
    queue: Arc<dyn Queue>,
    config: Arc<dyn ConfigStore>,
    ratelimit: Arc<dyn RateLimit>,
    blob: Arc<dyn Blob>,
    auth: Arc<dyn Auth>,
    schedule: Arc<dyn Schedule>,
    pubsub: Arc<dyn Pubsub>,
    /// One lifecycle handle per primitive, driven by maintenance and readiness checks.
    lifecycle: Vec<Arc<dyn BackendLifecycle>>,
    runtime: Runtime,
    closing: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Sender<bool>,
    workers: Arc<queue::worker::WorkerTracker>,
    namespace: String,
    obs: Arc<obs::Observability>,
    test_clock: Option<Arc<clock::ManualClock>>,
    environment: Environment,
    allow_memory_in_production: bool,
}

pub(crate) struct BuildDependencies {
    pub(crate) clock: Arc<dyn clock::Clock>,
    pub(crate) random: Arc<dyn clock::RandomSource>,
    pub(crate) test_clock: Option<Arc<clock::ManualClock>>,
}

impl Default for BuildDependencies {
    fn default() -> Self {
        Self {
            clock: Arc::new(clock::SystemClock::new()),
            random: Arc::new(clock::SystemRandom),
            test_clock: None,
        }
    }
}

enum Runtime {
    Memory,
    Postgres(Box<PostgresRuntime>),
}

#[derive(Clone, Copy)]
enum MigrationOperation {
    Migrate,
    Status,
    Validate,
}

struct PostgresRuntime {
    pool: sqlx::PgPool,
    url: String,
    #[cfg(feature = "embedded")]
    embedded: std::sync::Mutex<Option<pg::embedded::EmbeddedPg>>,
}

/// Externally-supplied backends, one optional slot per primitive. A present slot serves as
/// both the operation and lifecycle handle and suppresses Postgres connect/migrate for that
/// primitive. [`Forge::init`] passes all-`None`; [`ForgeBuilder`] fills the slots.
#[derive(Default)]
struct Injected {
    kv: Option<Arc<dyn KvBackend>>,
    queue: Option<Arc<dyn QueueBackend>>,
    config: Option<Arc<dyn ConfigStoreBackend>>,
    ratelimit: Option<Arc<dyn RateLimitBackend>>,
    blob: Option<Arc<dyn BlobBackend>>,
    auth: Option<Arc<dyn AuthBackend>>,
    schedule: Option<Arc<dyn ScheduleBackend>>,
    pubsub: Option<Arc<dyn PubsubBackend>>,
}

impl Forge {
    /// Read `forge.toml` from the current directory and instantiate the runtime from it.
    /// The file is the single source of configuration; its string values may reference the
    /// environment as `${VAR}` / `${VAR:-default}`. See [`init_from`](Self::init_from) for an
    /// explicit path and [`init_from_str`](Self::init_from_str) for an in-memory config.
    pub async fn init() -> Result<Self> {
        Self::init_from("forge.toml").await
    }

    /// Like [`init`](Self::init), but reads the `forge.toml` at `path` instead of the one in
    /// the current directory.
    pub async fn init_from(path: impl AsRef<Path>) -> Result<Self> {
        Self::build_from(ForgeConfig::from_toml_file(path)?, Injected::default()).await
    }

    /// Like [`init`](Self::init), but parses the `forge.toml` schema from an in-memory string
    /// rather than a file. For embedding the config or constructing it in tests.
    pub async fn init_from_str(toml: &str) -> Result<Self> {
        Self::build_from(ForgeConfig::from_toml_str(toml)?, Injected::default()).await
    }

    /// Create a memory client with a manual clock and deterministic token entropy for tests.
    pub async fn init_memory_for_testing(
        toml: &str,
        start: std::time::SystemTime,
        seed: u64,
    ) -> Result<Self> {
        let config = ForgeConfig::from_toml_str(toml)?;
        let clock = Arc::new(clock::ManualClock::new(start));
        Self::build_from_with_dependencies(
            config,
            Injected::default(),
            BuildDependencies {
                clock: clock.clone(),
                random: Arc::new(clock::SeededRandom::new(seed)),
                test_clock: Some(clock),
            },
        )
        .await
    }

    /// Advance the manual clock on a client created by [`Forge::init_memory_for_testing`].
    ///
    /// This fails for ordinary clients so production code cannot accidentally change time.
    pub fn advance_test_clock(&self, duration: Duration) -> Result<()> {
        let clock = self.inner.test_clock.as_ref().ok_or_else(|| {
            ForgeError::precondition("client was not created by the memory test factory")
        })?;
        clock.advance(duration);
        Ok(())
    }

    /// Apply pending migrations from `./forge.toml` and return one structured result per
    /// distinct PostgreSQL target. This constructs no application runtime handle.
    pub async fn migrate() -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_file("forge.toml")?,
            MigrationOperation::Migrate,
        )
        .await
    }

    /// Apply pending migrations using the configuration at `path`.
    pub async fn migrate_from(path: impl AsRef<Path>) -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_file(path)?,
            MigrationOperation::Migrate,
        )
        .await
    }

    /// Apply pending migrations using in-memory TOML configuration.
    pub async fn migrate_from_str(toml: &str) -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_str(toml)?,
            MigrationOperation::Migrate,
        )
        .await
    }

    /// Inspect migration state from `./forge.toml` without changing the database.
    pub async fn migration_status() -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_file("forge.toml")?,
            MigrationOperation::Status,
        )
        .await
    }

    /// Inspect migration state using the configuration at `path`.
    pub async fn migration_status_from(path: impl AsRef<Path>) -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_file(path)?,
            MigrationOperation::Status,
        )
        .await
    }

    /// Inspect migration state using in-memory TOML configuration.
    pub async fn migration_status_from_str(toml: &str) -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_str(toml)?,
            MigrationOperation::Status,
        )
        .await
    }

    /// Validate the schema from `./forge.toml` without locking or changing it.
    pub async fn validate_schema() -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_file("forge.toml")?,
            MigrationOperation::Validate,
        )
        .await
    }

    /// Validate the schema using the configuration at `path`.
    pub async fn validate_schema_from(path: impl AsRef<Path>) -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_file(path)?,
            MigrationOperation::Validate,
        )
        .await
    }

    /// Validate the schema using in-memory TOML configuration.
    pub async fn validate_schema_from_str(toml: &str) -> Result<Vec<MigrationReport>> {
        Self::migration_operation(
            ForgeConfig::from_toml_str(toml)?,
            MigrationOperation::Validate,
        )
        .await
    }

    async fn migration_operation(
        cfg: ForgeConfig,
        operation: MigrationOperation,
    ) -> Result<Vec<MigrationReport>> {
        cfg.validate()?;
        if cfg.mode == RuntimeMode::Memory {
            return Err(ForgeError::not_configured(
                "PostgreSQL migrations are unavailable in memory mode",
            ));
        }

        #[cfg(not(feature = "embedded"))]
        if cfg.use_embedded() {
            return Err(ForgeError::config(
                "[postgres] embedded = true requires forgelib's `embedded` cargo feature",
            ));
        }
        #[cfg(feature = "embedded")]
        let (cfg, _embedded) = if cfg.use_embedded() {
            let (server, url) = pg::embedded::start(&cfg.embedded_dir).await?;
            let mut cfg = cfg;
            cfg.postgres = url;
            (cfg, Some(server))
        } else {
            (cfg, None)
        };

        let mut targets: HashMap<String, (Vec<String>, config::DatabaseConfig)> = HashMap::new();
        let mut system = cfg.system_database();
        system.max_connections = system.max_connections.max(2);
        targets.insert(
            system.postgres.clone(),
            (vec!["system".to_string()], system),
        );
        for (primitive, database) in &cfg.feature_databases {
            let mut database = database.clone();
            database.max_connections = database.max_connections.max(2);
            match targets.entry(database.postgres.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().0.push(primitive.as_str().to_string());
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert((vec![primitive.as_str().to_string()], database));
                }
            }
        }

        let mut reports = Vec::with_capacity(targets.len());
        for (_, (mut labels, database)) in targets {
            labels.sort();
            let target = if labels.iter().any(|label| label == "system") {
                "system".to_string()
            } else {
                labels.join("+")
            };
            let pool = pg::connect(&database).await?;
            let runner = pg::MigrationRunner::new(pool.clone(), target, cfg.migration_lock_timeout);
            let report = match operation {
                MigrationOperation::Migrate => runner.run().await?,
                MigrationOperation::Status => runner.status().await?,
                MigrationOperation::Validate => runner.validate().await?,
            };
            pool.close().await;
            reports.push(report);
        }
        reports.sort_by(|left, right| left.target.cmp(&right.target));
        Ok(reports)
    }

    /// Builder for swapping in externally-implemented backends per primitive while leaving
    /// the rest on their config-selected built-in. See [`ForgeBuilder`].
    pub fn builder() -> ForgeBuilder {
        ForgeBuilder {
            cfg: None,
            injected: Injected::default(),
        }
    }

    /// The single construction path behind [`Forge::init`] and [`Forge::builder`]. Each
    /// primitive uses its injected backend if present, else its config-selected built-in;
    /// the pool/migration plan covers only the Postgres-backed built-ins.
    async fn build_from(cfg: ForgeConfig, injected: Injected) -> Result<Self> {
        Self::build_from_with_dependencies(cfg, injected, BuildDependencies::default()).await
    }

    pub(crate) async fn build_from_with_dependencies(
        cfg: ForgeConfig,
        injected: Injected,
        dependencies: BuildDependencies,
    ) -> Result<Self> {
        cfg.validate()?;

        // An embedded server boots first and mints the system DSN; everything after
        // this point (pools, feature overrides, migrations) is identical to an
        // externally-provided Postgres. An explicit connection string outranks the
        // flag, so `url = "${VAR:-}"` + `embedded = true` deploys against $VAR.
        if cfg.mode == RuntimeMode::Postgres && cfg.embedded && !cfg.use_embedded() {
            tracing::info!(
                "a [postgres] url is set; connecting to it instead of the embedded server"
            );
        }
        #[cfg(not(feature = "embedded"))]
        if cfg.mode == RuntimeMode::Postgres && cfg.use_embedded() {
            return Err(crate::error::ForgeError::config(
                "[postgres] embedded = true requires forgelib's `embedded` cargo feature \
                 (the Node and Python packages ship with it; in Rust enable it explicitly)",
            ));
        }
        #[cfg(feature = "embedded")]
        let (embedded, cfg) = if cfg.mode == RuntimeMode::Postgres && cfg.use_embedded() {
            let (server, url) = pg::embedded::start(&cfg.embedded_dir).await?;
            let mut cfg = cfg;
            cfg.postgres = url;
            (Some(server), cfg)
        } else {
            (None, cfg)
        };

        // Memory mode exits this construction phase without touching PostgreSQL. The
        // PostgreSQL profile may give measured hot primitives their own target, but every
        // target uses the same coordinated migration history.
        let system_pool = if cfg.mode == RuntimeMode::Postgres {
            Some(pg::connect(&cfg.system_database()).await?)
        } else {
            None
        };
        let mut feature_pools: HashMap<Primitive, sqlx::PgPool> = HashMap::new();
        if cfg.mode == RuntimeMode::Postgres {
            for (feature, db) in &cfg.feature_databases {
                feature_pools.insert(*feature, pg::connect(db).await?);
            }
        }
        let pool_for = |feature: Primitive| -> Result<sqlx::PgPool> {
            feature_pools
                .get(&feature)
                .cloned()
                .or_else(|| system_pool.clone())
                .ok_or_else(|| {
                    ForgeError::NotConfigured(
                        "PostgreSQL is not configured in memory mode".to_string(),
                    )
                })
        };

        // Migrate each distinct Postgres target once: the system database plus the distinct
        // targets of Postgres-backed feature overrides.
        if cfg.mode == RuntimeMode::Postgres {
            let mut by_target: HashMap<String, (Vec<String>, sqlx::PgPool)> = HashMap::new();
            by_target.insert(
                cfg.system_database().postgres,
                (vec!["system".to_string()], pool_for(Primitive::Kv)?),
            );
            for (feature, db) in &cfg.feature_databases {
                if let Some(pool) = feature_pools.get(feature) {
                    match by_target.entry(db.postgres.clone()) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            entry.get_mut().0.push(feature.as_str().to_string());
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert((vec![feature.as_str().to_string()], pool.clone()));
                        }
                    }
                }
            }
            for (_, (mut labels, pool)) in by_target {
                labels.sort();
                let target = if labels.iter().any(|label| label == "system") {
                    "system".to_string()
                } else {
                    labels.join("+")
                };
                let runner = pg::MigrationRunner::new(pool, target, cfg.migration_lock_timeout);
                let report = if cfg.auto_migrate {
                    runner.run().await?
                } else {
                    // Validation-only startup does not mutate schema and must not contend on
                    // the migration advisory lock. It still rejects pending or incompatible
                    // schema from the inspected migration history.
                    runner.validate().await?
                };
                if !report.is_compatible() {
                    return Err(ForgeError::config(format!(
                        "Forge schema {}: {} (run the migration API before initialization)",
                        report.state.as_str(),
                        report.message
                    )));
                }
            }
        }

        let secret = cfg.blob_signing_secret.clone().map(String::into_bytes);
        let ns = &cfg.kv_namespace;

        // Each primitive resolves to its operation handle and a lifecycle handle off the same
        // object: an injected backend if present (upcast to both halves), else the
        // config-selected built-in, which draws its pool only here.
        macro_rules! resolve {
            ($op:path, $inj:expr, $prim:expr, $pg:expr, $mem:expr) => {{
                let pair: (Arc<dyn $op>, Arc<dyn BackendLifecycle>) = match $inj {
                    Some(b) => (b.clone(), b),
                    None => match cfg.mode {
                        RuntimeMode::Postgres => {
                            let v = Arc::new($pg);
                            (v.clone(), v)
                        }
                        RuntimeMode::Memory => {
                            let v = Arc::new($mem);
                            (v.clone(), v)
                        }
                    },
                };
                pair
            }};
        }

        let (kv, kv_life) = resolve!(
            Kv,
            injected.kv,
            Primitive::Kv,
            kv::PgKv::new(pool_for(Primitive::Kv)?, ns.clone()),
            kv::MemKv::with_clock(ns.clone(), dependencies.clock.clone())
        );
        let (queue, queue_life) = resolve!(
            Queue,
            injected.queue,
            Primitive::Queue,
            queue::PgQueue::new(
                pool_for(Primitive::Queue)?,
                cfg.queue_dedup_window,
                cfg.queue_payload_retention,
                queue::TerminalRetention {
                    succeeded: cfg.queue_succeeded_retention,
                    dead: cfg.queue_dead_retention,
                    cancelled: cfg.queue_cancelled_retention,
                },
                ns.clone()
            ),
            queue::MemQueue::with_retention(
                cfg.queue_dedup_window,
                cfg.queue_payload_retention,
                queue::TerminalRetention {
                    succeeded: cfg.queue_succeeded_retention,
                    dead: cfg.queue_dead_retention,
                    cancelled: cfg.queue_cancelled_retention,
                },
                ns.clone(),
                dependencies.clock.clone()
            )
        );
        let (config, config_life) = resolve!(
            ConfigStore,
            injected.config,
            Primitive::Config,
            config_store::PgConfig::new(pool_for(Primitive::Config)?, ns.clone()),
            config_store::MemConfig::new(ns.clone())
        );
        let (ratelimit, ratelimit_life) = resolve!(
            RateLimit,
            injected.ratelimit,
            Primitive::RateLimit,
            ratelimit::PgRateLimit::new(
                pool_for(Primitive::RateLimit)?,
                ns.clone(),
                cfg.ratelimit_fail_open
            ),
            ratelimit::MemRateLimit::with_clock(
                ns.clone(),
                cfg.ratelimit_fail_open,
                dependencies.clock.clone()
            )
        );
        let (auth, auth_life) = resolve!(
            Auth,
            injected.auth,
            Primitive::Auth,
            auth::PgAuth::new(pool_for(Primitive::Auth)?, ns.clone()),
            auth::MemAuth::with_dependencies(
                ns.clone(),
                dependencies.clock.clone(),
                dependencies.random.clone()
            )
        );
        // Postgres pubsub needs the connection URL, not just the pool, for LISTEN/NOTIFY.
        let (pubsub, pubsub_life) = resolve!(
            Pubsub,
            injected.pubsub,
            Primitive::Pubsub,
            pubsub::PgPubsub::new(
                pool_for(Primitive::Pubsub)?,
                cfg.database_for(Primitive::Pubsub).postgres,
                ns.clone()
            ),
            pubsub::MemPubsub::new(ns.clone())
        );
        // Schedule delivers through the resolved queue, so a memory-backed schedule still
        // enqueues real work; built after queue for that reason.
        let (schedule, schedule_life) = resolve!(
            Schedule,
            injected.schedule,
            Primitive::Schedule,
            schedule::PgSchedule::new(pool_for(Primitive::Schedule)?, ns.clone(), queue.clone()),
            schedule::MemSchedule::with_clock(
                ns.clone(),
                queue.clone(),
                dependencies.clock.clone()
            )
        );

        // Blob has three built-ins (BYTEA, filesystem, memory) instead of the Postgres/Memory
        // pair, so it resolves on its own. Filesystem still keeps its metadata in Postgres.
        let (blob, blob_life): (Arc<dyn Blob>, Arc<dyn BackendLifecycle>) = match injected.blob {
            Some(b) => (b.clone(), b),
            None => match &cfg.blob_backend {
                BlobBackendConfig::Postgres => {
                    let b = Arc::new(blob::PgBlob::new(
                        pool_for(Primitive::Blob)?,
                        ns.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                    ));
                    (b.clone(), b)
                }
                BlobBackendConfig::Filesystem { root } => {
                    let b = Arc::new(blob::FsBlob::new(
                        pool_for(Primitive::Blob)?,
                        ns.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                        root.clone(),
                    )?);
                    (b.clone(), b)
                }
                BlobBackendConfig::Memory => {
                    let b = Arc::new(blob::MemBlob::new(
                        ns.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                    ));
                    (b.clone(), b)
                }
                BlobBackendConfig::S3(config) => {
                    let b = Arc::new(
                        blob::S3Blob::new(
                            config.clone(),
                            ns.clone(),
                            secret.clone(),
                            cfg.blob_base_url.clone(),
                        )
                        .await?,
                    );
                    (b.clone(), b)
                }
            },
        };

        // Lifecycle handles in Primitive order, for maintenance and capabilities. Each is the
        // object the primitive resolved to, so the report reflects the live choice.
        let lifecycle: Vec<Arc<dyn BackendLifecycle>> = vec![
            kv_life,
            queue_life,
            blob_life,
            auth_life,
            config_life,
            ratelimit_life,
            schedule_life,
            pubsub_life,
        ];
        let closing = Arc::new(AtomicBool::new(false));
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let workers = Arc::new(queue::worker::WorkerTracker::new());
        let obs = Arc::new(obs::Observability::new());
        let kv = Arc::new(lifecycle::GatedKv::new(kv, closing.clone(), obs.clone()));
        let queue = Arc::new(lifecycle::GatedQueue::new(
            queue,
            closing.clone(),
            obs.clone(),
        ));
        let config = Arc::new(lifecycle::GatedConfig::new(
            config,
            closing.clone(),
            obs.clone(),
        ));
        let ratelimit = Arc::new(lifecycle::GatedRateLimit::new(
            ratelimit,
            closing.clone(),
            obs.clone(),
        ));
        let blob = Arc::new(lifecycle::GatedBlob::new(
            blob,
            closing.clone(),
            obs.clone(),
        ));
        let auth = Arc::new(lifecycle::GatedAuth::new(
            auth,
            closing.clone(),
            obs.clone(),
        ));
        let schedule = Arc::new(lifecycle::GatedSchedule::new(
            schedule,
            closing.clone(),
            obs.clone(),
        ));
        let pubsub = Arc::new(lifecycle::GatedPubsub::new(
            pubsub,
            closing.clone(),
            shutdown.clone(),
            obs.clone(),
        ));

        let runtime = match cfg.mode {
            RuntimeMode::Memory => Runtime::Memory,
            RuntimeMode::Postgres => Runtime::Postgres(Box::new(PostgresRuntime {
                pool: system_pool
                    .ok_or_else(|| ForgeError::backend("PostgreSQL runtime was not constructed"))?,
                url: cfg.postgres.clone(),
                #[cfg(feature = "embedded")]
                embedded: std::sync::Mutex::new(embedded),
            })),
        };

        Ok(Self {
            inner: Arc::new(ForgeInner {
                kv,
                queue,
                config,
                ratelimit,
                blob,
                auth,
                schedule,
                pubsub,
                lifecycle,
                runtime,
                closing,
                shutdown,
                workers,
                namespace: ns.clone(),
                obs,
                test_clock: dependencies.test_clock,
                environment: cfg.environment,
                allow_memory_in_production: cfg.allow_memory_in_production,
            }),
        })
    }

    /// Static provider capabilities. This performs no I/O and is not a health check.
    pub fn backend_capabilities(&self) -> BackendCapabilities {
        let backends = self
            .inner
            .lifecycle
            .iter()
            .map(|b| BackendInfo::new(b.primitive(), b.name(), b.durable(), b.caveats()))
            .collect();
        BackendCapabilities::new(backends)
    }

    /// Process liveness only. Dependency failure does not make the process dead.
    pub fn is_live(&self) -> bool {
        !self.is_closing()
    }

    /// Run one real, bounded operation against every enabled backend. All probes start
    /// together, so `deadline` bounds the complete pass rather than each backend in series.
    pub async fn probe(&self, opts: ProbeOptions) -> Result<HealthReport> {
        if opts.deadline.is_zero() || opts.deadline > Duration::from_secs(30) {
            return Err(ForgeError::invalid(
                "health probe deadline must be between 1ms and 30s",
            ));
        }
        let started = std::time::Instant::now();
        let checked_at = std::time::SystemTime::now();
        let required: std::collections::HashSet<Primitive> = if opts.readiness_backends.is_empty() {
            self.inner
                .lifecycle
                .iter()
                .map(|backend| backend.primitive())
                .collect()
        } else {
            opts.readiness_backends.iter().copied().collect()
        };
        let probes = self.inner.lifecycle.iter().map(|backend| {
            let primitive = backend.primitive();
            let provider = backend.name().to_string();
            async move {
                let backend_started = std::time::Instant::now();
                let result =
                    tokio::time::timeout(opts.deadline, self.probe_backend(primitive)).await;
                let latency_ms = backend_started.elapsed().as_secs_f64() * 1000.0;
                match result {
                    Ok(Ok(())) => {
                        let success_at = std::time::SystemTime::now();
                        self.inner.obs.mark_probe_success(primitive, success_at);
                        BackendHealth {
                            primitive,
                            provider,
                            status: "healthy".to_string(),
                            latency_ms,
                            error_category: None,
                            last_success_ms: Some(obs::unix_ms(success_at)),
                            message: "backend probe succeeded".to_string(),
                        }
                    }
                    Ok(Err(error)) => BackendHealth {
                        primitive,
                        provider,
                        status: "unhealthy".to_string(),
                        latency_ms,
                        error_category: Some(obs::error_variant(&error).to_string()),
                        last_success_ms: self
                            .inner
                            .obs
                            .last_probe_success(primitive)
                            .map(obs::unix_ms),
                        message: obs::safe_probe_message(&error).to_string(),
                    },
                    Err(_) => BackendHealth {
                        primitive,
                        provider,
                        status: "unhealthy".to_string(),
                        latency_ms,
                        error_category: Some("timeout".to_string()),
                        last_success_ms: self
                            .inner
                            .obs
                            .last_probe_success(primitive)
                            .map(obs::unix_ms),
                        message: "backend probe exceeded the total deadline".to_string(),
                    },
                }
            }
        });
        let backends = futures_util::future::join_all(probes).await;
        let live = self.is_live();
        let ready = live
            && backends.iter().all(|backend| {
                !required.contains(&backend.primitive) || backend.status == "healthy"
            });
        self.inner
            .obs
            .gauge("forge_health_ready", &[], if ready { 1.0 } else { 0.0 });
        Ok(HealthReport {
            live,
            ready,
            checked_at_ms: obs::unix_ms(checked_at),
            duration_ms: started.elapsed().as_secs_f64() * 1000.0,
            backends,
        })
    }

    /// Run bounded deployment diagnostics. Applications decide where to
    /// expose this report; Forge does not install an admin server.
    pub async fn diagnostics(&self, deadline: Duration) -> Result<DiagnosticsReport> {
        if deadline.is_zero() || deadline > Duration::from_secs(30) {
            return Err(ForgeError::invalid(
                "diagnostics deadline must be between 1ms and 30s",
            ));
        }
        let checked_at = std::time::SystemTime::now();
        let mut checks = vec![DiagnosticCheck {
            name: "configuration".to_string(),
            status: "pass".to_string(),
            message: format!(
                "resolved {} profile with namespace isolation {}",
                match self.mode() {
                    RuntimeMode::Memory => "memory",
                    RuntimeMode::Postgres => "postgres",
                },
                if self.inner.namespace.is_empty() {
                    "disabled"
                } else {
                    "enabled"
                }
            ),
        }];

        match &self.inner.runtime {
            Runtime::Memory => {
                checks.push(DiagnosticCheck {
                    name: "database_version".to_string(),
                    status: "pass".to_string(),
                    message: "not applicable to the memory profile".to_string(),
                });
                checks.push(DiagnosticCheck {
                    name: "schema_state".to_string(),
                    status: "pass".to_string(),
                    message: "memory profile has no persistent schema".to_string(),
                });
                checks.push(DiagnosticCheck {
                    name: "permissions".to_string(),
                    status: "pass".to_string(),
                    message: "memory profile requires no external permissions".to_string(),
                });
                checks.push(DiagnosticCheck {
                    name: "clock_skew".to_string(),
                    status: "pass".to_string(),
                    message: "memory profile uses the application clock".to_string(),
                });
            }
            Runtime::Postgres(runtime) => {
                let version = sqlx::query_scalar!(
                    "SELECT current_setting('server_version_num') AS \"version!\""
                )
                .fetch_one(&runtime.pool)
                .await;
                checks.push(match version {
                    Ok(version)
                        if version.parse::<i32>().unwrap_or(0) >= pg::MIN_SERVER_VERSION_NUM =>
                    {
                        DiagnosticCheck {
                            name: "database_version".to_string(),
                            status: "pass".to_string(),
                            message: format!(
                                "PostgreSQL server_version_num {version} is supported"
                            ),
                        }
                    }
                    Ok(version) => DiagnosticCheck {
                        name: "database_version".to_string(),
                        status: "fail".to_string(),
                        message: format!(
                            "PostgreSQL server_version_num {version} is below the supported minimum"
                        ),
                    },
                    Err(_) => DiagnosticCheck {
                        name: "database_version".to_string(),
                        status: "fail".to_string(),
                        message: "could not read the PostgreSQL server version".to_string(),
                    },
                });

                let schema =
                    pg::MigrationRunner::new(runtime.pool.clone(), "system", Duration::ZERO)
                        .validate()
                        .await;
                checks.push(match schema {
                    Ok(report) if report.state == MigrationState::Applied => DiagnosticCheck {
                        name: "schema_state".to_string(),
                        status: "pass".to_string(),
                        message: format!("Forge schema {} is current", report.target_version),
                    },
                    Ok(report) => DiagnosticCheck {
                        name: "schema_state".to_string(),
                        status: "fail".to_string(),
                        message: format!(
                            "Forge schema is {}: {}",
                            report.state.as_str(),
                            report.message
                        ),
                    },
                    Err(_) => DiagnosticCheck {
                        name: "schema_state".to_string(),
                        status: "fail".to_string(),
                        message: "could not inspect Forge migration history".to_string(),
                    },
                });

                let permissions = sqlx::query_scalar!(
                    "SELECT COALESCE(has_table_privilege(current_user, 'forge_jobs', 'SELECT,INSERT,UPDATE,DELETE'), FALSE) AS \"permitted!\"",
                )
                .fetch_one(&runtime.pool)
                .await;
                let permissions_ok = matches!(permissions, Ok(true));
                checks.push(DiagnosticCheck {
                    name: "permissions".to_string(),
                    status: if permissions_ok { "pass" } else { "fail" }.to_string(),
                    message: if permissions_ok {
                        "runtime role can read and mutate Forge queue tables"
                    } else {
                        "runtime role lacks required Forge queue table permissions"
                    }
                    .to_string(),
                });

                let server_epoch = sqlx::query_scalar!(
                    "SELECT EXTRACT(EPOCH FROM clock_timestamp())::double precision AS \"epoch!\"",
                )
                .fetch_one(&runtime.pool)
                .await;
                let local_epoch = checked_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                checks.push(match server_epoch {
                    Ok(server_epoch) => {
                        let skew = (server_epoch - local_epoch).abs();
                        DiagnosticCheck {
                            name: "clock_skew".to_string(),
                            status: if skew > 30.0 {
                                "fail"
                            } else if skew > 5.0 {
                                "warn"
                            } else {
                                "pass"
                            }
                            .to_string(),
                            message: format!(
                                "database clock differs from the application by {skew:.3}s"
                            ),
                        }
                    }
                    Err(_) => DiagnosticCheck {
                        name: "clock_skew".to_string(),
                        status: "fail".to_string(),
                        message: "could not compare the database and application clocks"
                            .to_string(),
                    },
                });
            }
        }

        let health = self
            .probe(ProbeOptions::new().with_deadline(deadline))
            .await?;
        checks.push(DiagnosticCheck {
            name: "backend_reachability".to_string(),
            status: if health.ready { "pass" } else { "fail" }.to_string(),
            message: if health.ready {
                "all required backend probes succeeded"
            } else {
                "one or more required backend probes failed"
            }
            .to_string(),
        });
        let unsafe_memory = self.inner.environment == Environment::Production
            && self.mode() == RuntimeMode::Memory
            && self.inner.allow_memory_in_production;
        checks.push(DiagnosticCheck {
            name: "unsafe_production_settings".to_string(),
            status: if unsafe_memory { "fail" } else { "pass" }.to_string(),
            message: if unsafe_memory {
                "production explicitly permits the non-durable memory profile"
            } else {
                "no unsafe production override is active"
            }
            .to_string(),
        });
        let ready = checks.iter().all(|check| check.status != "fail");
        Ok(DiagnosticsReport {
            ready,
            checked_at_ms: checked_at
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            checks,
        })
    }

    async fn probe_backend(&self, primitive: Primitive) -> Result<()> {
        const PROBE_KEY: &str = "__forge_probe__";
        match primitive {
            Primitive::Kv => self.inner.kv.exists(PROBE_KEY).await.map(|_| ()),
            Primitive::Queue => self.inner.queue.depth(PROBE_KEY).await.map(|_| ()),
            Primitive::Blob => self.inner.blob.head(PROBE_KEY).await.map(|_| ()),
            Primitive::Auth => self
                .inner
                .auth
                .verify_api_key("fk_forge_probe")
                .await
                .map(|_| ()),
            Primitive::Config => self.inner.config.get_raw(PROBE_KEY).await.map(|_| ()),
            Primitive::RateLimit => self
                .inner
                .ratelimit
                .check_with(
                    PROBE_KEY,
                    PROBE_KEY,
                    Limit::per_duration(ratelimit::MAX_UNITS, Duration::from_secs(1)),
                    FailMode::Closed,
                )
                .await
                .map(|_| ()),
            Primitive::Schedule => self.inner.schedule.list(None, 1).await.map(|_| ()),
            Primitive::Pubsub => self.inner.pubsub.publish(PROBE_KEY, Bytes::new()).await,
        }
    }

    /// Point-in-time metrics owned only by this Forge handle.
    pub fn metrics_snapshot(&self) -> Vec<MetricSample> {
        self.refresh_runtime_gauges();
        self.inner.obs.snapshot()
    }

    /// Prometheus 0.0.4 text for this Forge handle. No global recorder or HTTP server
    /// is installed; applications expose the returned UTF-8 text on their own route.
    pub fn render_prometheus(&self) -> String {
        self.refresh_runtime_gauges();
        self.inner.obs.render_prometheus()
    }

    fn refresh_runtime_gauges(&self) {
        if let Runtime::Postgres(runtime) = &self.inner.runtime {
            self.inner.obs.gauge(
                "forge_pool_connections",
                &[("state", "open")],
                runtime.pool.size() as f64,
            );
            self.inner.obs.gauge(
                "forge_pool_connections",
                &[("state", "idle")],
                runtime.pool.num_idle() as f64,
            );
        }
        self.inner.obs.gauge(
            "forge_workers_active",
            &[],
            self.inner.workers.active() as f64,
        );
    }

    /// The pool to Forge's system database (the `forge_*` tables, not your application's). An
    /// escape hatch for Forge-adjacent SQL, not a home for your domain tables; features with
    /// their own database run on separate pools not reachable here. Using it ties the caller
    /// to Forge's `sqlx` major version.
    pub fn pool(&self) -> Result<&sqlx::PgPool> {
        match &self.inner.runtime {
            Runtime::Postgres(runtime) => Ok(&runtime.pool),
            Runtime::Memory => Err(ForgeError::NotConfigured(
                "PostgreSQL is not configured in memory mode".to_string(),
            )),
        }
    }

    /// The resolved connection string of the system database — the configured
    /// `[postgres] url`, or the DSN an embedded server minted at init. Contains
    /// credentials; intended for wiring an application's own pool/tables onto the
    /// same database (the only way to reach an embedded server from outside Forge).
    pub fn postgres_url(&self) -> Result<&str> {
        match &self.inner.runtime {
            Runtime::Postgres(runtime) => Ok(&runtime.url),
            Runtime::Memory => Err(ForgeError::NotConfigured(
                "PostgreSQL is not configured in memory mode".to_string(),
            )),
        }
    }

    /// The resolved complete runtime profile.
    pub fn mode(&self) -> RuntimeMode {
        match &self.inner.runtime {
            Runtime::Memory => RuntimeMode::Memory,
            Runtime::Postgres(_) => RuntimeMode::Postgres,
        }
    }

    /// Begin graceful shutdown. Idempotent. New facade work is rejected immediately;
    /// PostgreSQL pool closure is bounded by `deadline`.
    pub async fn close(&self, deadline: Duration) -> Result<()> {
        let started = tokio::time::Instant::now();
        if !self.inner.closing.swap(true, Ordering::AcqRel) {
            let _ = self.inner.shutdown.send(true);
        }
        tokio::time::timeout(deadline, self.inner.workers.drained())
            .await
            .map_err(|_| {
                ForgeError::unavailable("shutdown deadline elapsed while draining workers")
            })?;
        for backend in &self.inner.lifecycle {
            let remaining = deadline.saturating_sub(started.elapsed());
            tokio::time::timeout(remaining, backend.close())
                .await
                .map_err(|_| {
                    ForgeError::unavailable("shutdown deadline elapsed while closing a backend")
                })??;
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if let Runtime::Postgres(runtime) = &self.inner.runtime {
            tokio::time::timeout(remaining, runtime.pool.close())
                .await
                .map_err(|_| ForgeError::unavailable("shutdown deadline elapsed"))?;
            #[cfg(feature = "embedded")]
            if let Ok(mut embedded) = runtime.embedded.lock() {
                drop(embedded.take());
            }
        }
        Ok(())
    }

    /// Whether graceful shutdown has started.
    pub fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::Acquire)
    }

    fn ensure_open(&self) -> Result<()> {
        if self.is_closing() {
            Err(ForgeError::precondition("Forge is shutting down"))
        } else {
            Ok(())
        }
    }

    /// The key/value store. Lineage: Redis. See <https://tryforge.dev/primitives/#key-value>.
    pub fn kv(&self) -> &dyn Kv {
        self.inner.kv.as_ref()
    }

    /// The job queue. Lineage: AWS SQS. See <https://tryforge.dev/primitives/#queue>.
    pub fn queue(&self) -> &dyn Queue {
        self.inner.queue.as_ref()
    }

    /// Live publish/subscribe for realtime fan-out (subscriptions, presence). Lineage:
    /// Postgres LISTEN/NOTIFY + Redis pub/sub. See <https://tryforge.dev/primitives/#pubsub>. Not durable;
    /// use [`Forge::queue`] when a message must not be lost.
    pub fn pubsub(&self) -> &dyn Pubsub {
        self.inner.pubsub.as_ref()
    }

    /// Runtime config + feature flags. Lineage: 12-factor + OpenFeature. See
    /// <https://tryforge.dev/primitives/#config-and-flags>.
    pub fn config(&self) -> &dyn ConfigStore {
        self.inner.config.as_ref()
    }

    /// Rate limiter. Lineage: token bucket / GCRA + IETF RateLimit headers. See
    /// <https://tryforge.dev/primitives/#rate-limit>.
    pub fn ratelimit(&self) -> &dyn RateLimit {
        self.inner.ratelimit.as_ref()
    }

    /// Object storage. Lineage: AWS S3. See <https://tryforge.dev/primitives/#blob>.
    pub fn blob(&self) -> &dyn Blob {
        self.inner.blob.as_ref()
    }

    /// Auth primitives: passwords, sessions, API keys. Lineage: OWASP + PHC + Stripe/GitHub
    /// keys. See <https://tryforge.dev/primitives/#auth>.
    pub fn auth(&self) -> &dyn Auth {
        self.inner.auth.as_ref()
    }

    /// Recurring + one-shot scheduling. Lineage: cron + Unix `at` + k8s CronJob. See
    /// <https://tryforge.dev/primitives/#schedule>. Register work here; drive ticks with
    /// [`Forge::run_scheduler`].
    pub fn schedule(&self) -> &dyn Schedule {
        self.inner.schedule.as_ref()
    }

    /// Run one scheduler pass, firing every due schedule once, and return how many jobs were
    /// enqueued. For tests or a custom loop; most apps call [`Forge::run_scheduler`]. Safe to
    /// run concurrently across replicas.
    pub async fn run_scheduler_once(&self) -> Result<u64> {
        self.ensure_open()?;
        self.inner.schedule.process_due().await
    }

    /// Run the scheduler loop until [`Forge::close`] begins, firing due schedules roughly
    /// every 30s. Applications own process signals and call `close`.
    pub async fn run_scheduler(&self) {
        let mut shutdown = self.inner.shutdown.subscribe();
        loop {
            if self.is_closing() {
                break;
            }
            if let Err(e) = self.inner.schedule.process_due().await {
                tracing::warn!(error = %e, "scheduler tick failed; will retry");
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {},
                _ = shutdown.wait_for(|closing| *closing) => break,
            }
        }
    }

    /// A managed worker for `queue_name`: bounded concurrency, auto-heartbeat, `ack`/`nack`
    /// on completion, graceful shutdown.
    pub fn worker(&self, queue_name: impl Into<String>) -> WorkerBuilder {
        WorkerBuilder::new(
            self.inner.queue.clone(),
            queue_name,
            self.inner.shutdown.subscribe(),
            self.inner.workers.clone(),
            self.inner.obs.clone(),
        )
    }

    /// Run the maintenance sweep across every backend: purge expired kv rows and old
    /// completed jobs, reclaim leases orphaned by crashed workers, drop stale dedup and
    /// rate-limit rows, expire dead sessions and one-time tokens, and reclaim orphaned
    /// filesystem blobs. Idempotent; call it on a schedule.
    pub async fn maintain(&self) -> Result<()> {
        self.ensure_open()?;
        // Run every sweep even when one fails: a transient error in one backend
        // must not starve the rest of maintenance, sweep after sweep.
        let mut first_err = None;
        for backend in &self.inner.lifecycle {
            let started = std::time::Instant::now();
            let result = backend.maintain().await;
            let outcome = if result.is_ok() { "ok" } else { "error" };
            self.inner.obs.counter(
                "forge_maintenance_total",
                &[
                    ("primitive", backend.primitive().as_str()),
                    ("outcome", outcome),
                ],
                1,
            );
            self.inner.obs.histogram(
                "forge_maintenance_duration_seconds",
                &[("primitive", backend.primitive().as_str())],
                started.elapsed().as_secs_f64(),
            );
            if let Err(e) = result {
                tracing::warn!(
                    primitive = backend.primitive().as_str(),
                    provider = backend.name(),
                    error.variant = obs::error_variant(&e),
                    "maintenance sweep failed; continuing with remaining backends"
                );
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Builds a [`Forge`] with externally-implemented backends swapped in per primitive. Start
/// from [`Forge::builder`], point it at the system database, inject a backend for any
/// primitive you want to own, and leave the rest on their config-selected built-in:
///
/// ```no_run
/// # use std::sync::Arc;
/// # async fn demo(custom_kv: Arc<dyn forgelib::KvBackend>) -> forgelib::Result<()> {
/// let forge = forgelib::Forge::builder()
///     .config_str(r#"[postgres]
/// url = "postgres://localhost/myapp_forge""#)?
///     .kv(custom_kv) // kv runs on your backend; the other seven stay on Postgres
///     .build()
///     .await?;
/// # let _ = forge; Ok(())
/// # }
/// ```
///
/// An injected primitive supplies its own state and lifecycle, so Forge never connects or
/// migrates Postgres on its behalf. Every other knob (the system database, namespaces,
/// per-feature databases, blob signing) comes from the `forge.toml` supplied via
/// [`config_str`](ForgeBuilder::config_str) / [`config_path`](ForgeBuilder::config_path), or
/// the `./forge.toml` loaded by default; the builder itself stays small.
pub struct ForgeBuilder {
    cfg: Option<ForgeConfig>,
    injected: Injected,
}

impl ForgeBuilder {
    /// Supply the `forge.toml` config as an in-memory string. Replaces any previously set
    /// config. When neither this nor [`config_path`](Self::config_path) is called,
    /// [`build`](Self::build) reads `./forge.toml`.
    pub fn config_str(mut self, toml: &str) -> Result<Self> {
        self.cfg = Some(ForgeConfig::from_toml_str(toml)?);
        Ok(self)
    }

    /// Supply the `forge.toml` config from a file path. Replaces any previously set config.
    pub fn config_path(mut self, path: impl AsRef<Path>) -> Result<Self> {
        self.cfg = Some(ForgeConfig::from_toml_file(path)?);
        Ok(self)
    }

    /// Inject the key/value backend.
    pub fn kv(mut self, b: Arc<dyn KvBackend>) -> Self {
        self.injected.kv = Some(b);
        self
    }

    /// Inject the queue backend.
    pub fn queue(mut self, b: Arc<dyn QueueBackend>) -> Self {
        self.injected.queue = Some(b);
        self
    }

    /// Inject the config-store backend. Named `config_store` so it does not collide with
    /// [`config_str`](Self::config_str), which supplies the `forge.toml`.
    pub fn config_store(mut self, b: Arc<dyn ConfigStoreBackend>) -> Self {
        self.injected.config = Some(b);
        self
    }

    /// Inject the ratelimit backend.
    pub fn ratelimit(mut self, b: Arc<dyn RateLimitBackend>) -> Self {
        self.injected.ratelimit = Some(b);
        self
    }

    /// Inject the blob backend.
    pub fn blob(mut self, b: Arc<dyn BlobBackend>) -> Self {
        self.injected.blob = Some(b);
        self
    }

    /// Inject the auth backend.
    pub fn auth(mut self, b: Arc<dyn AuthBackend>) -> Self {
        self.injected.auth = Some(b);
        self
    }

    /// Inject the schedule backend.
    pub fn schedule(mut self, b: Arc<dyn ScheduleBackend>) -> Self {
        self.injected.schedule = Some(b);
        self
    }

    /// Inject the pubsub backend.
    pub fn pubsub(mut self, b: Arc<dyn PubsubBackend>) -> Self {
        self.injected.pubsub = Some(b);
        self
    }

    /// Validate, connect, migrate, and construct the [`Forge`] through the same path as
    /// [`Forge::init`]. When no config was supplied, reads `./forge.toml`.
    pub async fn build(self) -> Result<Forge> {
        let cfg = match self.cfg {
            Some(cfg) => cfg,
            None => ForgeConfig::from_toml_file("forge.toml")?,
        };
        Forge::build_from(cfg, self.injected).await
    }
}

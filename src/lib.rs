//! Forge: the standard library for agent-built SaaS. One crate, every backend primitive an
//! app needs, on interfaces the industry already trusts. Per-primitive contracts live in
//! `docs/contracts/`.
//!
//! Forge owns a system database: a Postgres database kept separate from your application's.
//! [`Forge::init`] connects and migrates its `forge_*` tables at startup. Primitives can
//! point at their own database via [`ForgeConfig::with_feature_database`], but the system
//! database is always required.
//!
//! ```no_run
//! # async fn demo() -> forge::Result<()> {
//! use forge::{Forge, ForgeConfig};
//! // A database Forge owns, not the one holding your application tables.
//! let forge = Forge::init(ForgeConfig::new("postgres://localhost/myapp_forge")).await?;
//! forge.kv().set("greeting", "hi".into(), Default::default()).await?;
//! let id = forge.queue().enqueue("emails", b"payload".to_vec().into(), Default::default()).await?;
//! # let _ = id; Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

pub mod auth;
pub mod backend;
pub mod blob;
pub mod config;
pub mod config_store;
pub mod error;
pub mod kv;
pub mod pubsub;
pub mod queue;
pub mod ratelimit;
pub mod schedule;
pub mod typed;
pub mod types;

mod obs;
mod util;

mod pg;

#[cfg(feature = "pg-tests")]
pub mod testing;

#[cfg(any(feature = "conformance", test))]
pub mod conformance;

use std::collections::HashMap;
use std::sync::Arc;

pub use auth::{
    ApiKey, ApiKeyInfo, ApiKeySecret, Auth, PhcString, Session, SessionOpts, SessionToken,
};
pub use backend::{
    AuthBackend, BackendInfo, BackendLifecycle, BackendReport, BlobBackend, ConfigStoreBackend,
    KvBackend, Primitive, PubsubBackend, QueueBackend, RateLimitBackend, ScheduleBackend,
};
pub use blob::{Blob, BlobInfo, ListPage, PutOpts};
pub use config::{Backend, BlobBackendConfig, DatabaseConfig, ForgeConfig};
pub use config_store::{ConfigExt, ConfigStore, EvalCtx, FlagRule};
pub use error::{ForgeError, Result};
pub use kv::{Kv, SetMode, SetOpts};
pub use pubsub::{Pubsub, Subscription};
pub use queue::worker::WorkerBuilder;
pub use queue::{DequeueOpts, EnqueueOpts, Job, JobId, NackOpts, Queue, QueueDepth};
pub use ratelimit::{Algo, Decision, FailMode, Limit, RateLimit};
pub use schedule::{Schedule, ScheduleInfo, ScheduleKind, ScheduleOpts};
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
    /// One lifecycle handle per primitive, driven by `maintain`/`backend_report`.
    lifecycle: Vec<Arc<dyn BackendLifecycle>>,
    /// The Postgres pool every primitive is built on.
    pool: sqlx::PgPool,
}

/// Primitives whose correctness needs cross-process delivery (pubsub, ratelimit) but that
/// resolve to a memory backend under `cfg`. Pure and DB-free so it stays unit-testable.
fn non_durable_warnings(cfg: &ForgeConfig) -> Vec<Primitive> {
    [Primitive::Pubsub, Primitive::RateLimit]
        .into_iter()
        .filter(|&p| cfg.backend_for(p) == Backend::Memory)
        .collect()
}

fn warn_non_durable(p: Primitive) {
    tracing::warn!(
        primitive = p.as_str(),
        "non-durable backend selected for a primitive whose correctness depends on cross-process delivery/sharing"
    );
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

impl Injected {
    /// Whether a backend was injected for `p`.
    fn is_injected(&self, p: Primitive) -> bool {
        match p {
            Primitive::Kv => self.kv.is_some(),
            Primitive::Queue => self.queue.is_some(),
            Primitive::Config => self.config.is_some(),
            Primitive::RateLimit => self.ratelimit.is_some(),
            Primitive::Blob => self.blob.is_some(),
            Primitive::Auth => self.auth.is_some(),
            Primitive::Schedule => self.schedule.is_some(),
            Primitive::Pubsub => self.pubsub.is_some(),
        }
    }
}

impl Forge {
    /// Validate, connect, migrate, and construct every primitive. Migrates the system
    /// database (and each distinct feature database) at startup; idempotent and safe to run
    /// concurrently across replicas, with an advisory lock serializing it and checksums
    /// guarding immutability. Misconfiguration fails here with [`ForgeError::Config`], never
    /// lazily on first use.
    pub async fn init(cfg: ForgeConfig) -> Result<Self> {
        Self::build_from(cfg, Injected::default()).await
    }

    /// Builder for swapping in externally-implemented backends per primitive while leaving
    /// the rest on their config-selected built-in. See [`ForgeBuilder`].
    pub fn builder() -> ForgeBuilder {
        ForgeBuilder {
            cfg: ForgeConfig::default(),
            injected: Injected::default(),
        }
    }

    /// The single construction path behind [`Forge::init`] and [`Forge::builder`]. Each
    /// primitive uses its injected backend if present, else its config-selected built-in;
    /// the pool/migration plan covers only the Postgres-backed built-ins.
    async fn build_from(cfg: ForgeConfig, injected: Injected) -> Result<Self> {
        cfg.validate()?;

        for p in non_durable_warnings(&cfg) {
            warn_non_durable(p);
        }

        // In-process, injected, and (for blob) memory backends never connect or migrate
        // Postgres, so the pool/migration plan covers only the Postgres-backed built-ins.
        // Filesystem blob counts as Postgres-backed: it keeps its metadata in Postgres.
        let is_pg_backed = |p: Primitive| -> bool {
            if injected.is_injected(p) {
                false
            } else if p == Primitive::Blob {
                !matches!(cfg.blob_backend, BlobBackendConfig::Memory)
            } else {
                cfg.backend_for(p) == Backend::Postgres
            }
        };

        // The system pool is mandatory even when every primitive is in-memory: Forge owns
        // and migrates its system database. Each Postgres-backed feature override gets its
        // own isolated pool; a memory-backed override is ignored.
        let system_pool = pg::connect(&cfg.system_database()).await?;
        let mut feature_pools: HashMap<Primitive, sqlx::PgPool> = HashMap::new();
        for (feature, db) in &cfg.feature_databases {
            if is_pg_backed(*feature) {
                feature_pools.insert(*feature, pg::connect(db).await?);
            }
        }
        let pool_for = |feature: Primitive| -> sqlx::PgPool {
            feature_pools
                .get(&feature)
                .cloned()
                .unwrap_or_else(|| system_pool.clone())
        };

        // Migrate each distinct Postgres target once: the system database plus the distinct
        // targets of Postgres-backed feature overrides.
        let mut by_target: HashMap<String, sqlx::PgPool> = HashMap::new();
        by_target.insert(cfg.system_database().postgres, system_pool.clone());
        for (feature, db) in &cfg.feature_databases {
            if let Some(pool) = feature_pools.get(feature) {
                by_target
                    .entry(db.postgres.clone())
                    .or_insert_with(|| pool.clone());
            }
        }
        for pool in by_target.into_values() {
            pg::MigrationRunner::new(pool).run().await?;
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
                    None => match cfg.backend_for($prim) {
                        Backend::Postgres => {
                            let v = Arc::new($pg);
                            (v.clone(), v)
                        }
                        Backend::Memory => {
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
            kv::PgKv::new(pool_for(Primitive::Kv), ns.clone()),
            kv::MemKv::new(ns.clone())
        );
        let (queue, queue_life) = resolve!(
            Queue,
            injected.queue,
            Primitive::Queue,
            queue::PgQueue::new(
                pool_for(Primitive::Queue),
                cfg.queue_dedup_window,
                cfg.queue_retention,
                ns.clone()
            ),
            queue::MemQueue::new(cfg.queue_dedup_window, cfg.queue_retention, ns.clone())
        );
        let (config, config_life) = resolve!(
            ConfigStore,
            injected.config,
            Primitive::Config,
            config_store::PgConfig::new(pool_for(Primitive::Config), ns.clone()),
            config_store::MemConfig::new(ns.clone())
        );
        let (ratelimit, ratelimit_life) = resolve!(
            RateLimit,
            injected.ratelimit,
            Primitive::RateLimit,
            ratelimit::PgRateLimit::new(
                pool_for(Primitive::RateLimit),
                ns.clone(),
                cfg.ratelimit_fail_open
            ),
            ratelimit::MemRateLimit::new(ns.clone(), cfg.ratelimit_fail_open)
        );
        let (auth, auth_life) = resolve!(
            Auth,
            injected.auth,
            Primitive::Auth,
            auth::PgAuth::new(pool_for(Primitive::Auth), ns.clone()),
            auth::MemAuth::new(ns.clone())
        );
        // Postgres pubsub needs the connection URL, not just the pool, for LISTEN/NOTIFY.
        let (pubsub, pubsub_life) = resolve!(
            Pubsub,
            injected.pubsub,
            Primitive::Pubsub,
            pubsub::PgPubsub::new(
                pool_for(Primitive::Pubsub),
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
            schedule::PgSchedule::new(pool_for(Primitive::Schedule), ns.clone(), queue.clone()),
            schedule::MemSchedule::new(ns.clone(), queue.clone())
        );

        // Blob has three built-ins (BYTEA, filesystem, memory) instead of the Postgres/Memory
        // pair, so it resolves on its own. Filesystem still keeps its metadata in Postgres.
        let (blob, blob_life): (Arc<dyn Blob>, Arc<dyn BackendLifecycle>) = match injected.blob {
            Some(b) => (b.clone(), b),
            None => match &cfg.blob_backend {
                BlobBackendConfig::Postgres => {
                    let b = Arc::new(blob::PgBlob::new(
                        pool_for(Primitive::Blob),
                        ns.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                    ));
                    (b.clone(), b)
                }
                BlobBackendConfig::Filesystem { root } => {
                    let b = Arc::new(blob::FsBlob::new(
                        pool_for(Primitive::Blob),
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
            },
        };

        // Lifecycle handles in Primitive order, for maintain + backend_report. Each is the
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
                pool: system_pool,
            }),
        })
    }

    /// Which backend powers each primitive, for logs and health pages. Not needed for
    /// request handling; the backend choice must not leak into app logic.
    pub fn backend_report(&self) -> BackendReport {
        let backends = self
            .inner
            .lifecycle
            .iter()
            .map(|b| BackendInfo::new(b.primitive(), b.name(), b.durable(), b.caveats()))
            .collect();
        BackendReport::new(backends)
    }

    /// The pool to Forge's system database (the `forge_*` tables, not your application's). An
    /// escape hatch for Forge-adjacent SQL, not a home for your domain tables; features with
    /// their own database run on separate pools not reachable here. Using it ties the caller
    /// to Forge's `sqlx` major version.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.inner.pool
    }

    /// The key/value store. Lineage: Redis. See `docs/contracts/kv.md`.
    pub fn kv(&self) -> &dyn Kv {
        self.inner.kv.as_ref()
    }

    /// The job queue. Lineage: AWS SQS. See `docs/contracts/queue.md`.
    pub fn queue(&self) -> &dyn Queue {
        self.inner.queue.as_ref()
    }

    /// Live publish/subscribe for realtime fan-out (subscriptions, presence). Lineage:
    /// Postgres LISTEN/NOTIFY + Redis pub/sub. See `docs/contracts/pubsub.md`. Not durable;
    /// use [`Forge::queue`] when a message must not be lost.
    pub fn pubsub(&self) -> &dyn Pubsub {
        self.inner.pubsub.as_ref()
    }

    /// Runtime config + feature flags. Lineage: 12-factor + OpenFeature. See
    /// `docs/contracts/config.md`.
    pub fn config(&self) -> &dyn ConfigStore {
        self.inner.config.as_ref()
    }

    /// Rate limiter. Lineage: token bucket / GCRA + IETF RateLimit headers. See
    /// `docs/contracts/ratelimit.md`.
    pub fn ratelimit(&self) -> &dyn RateLimit {
        self.inner.ratelimit.as_ref()
    }

    /// Object storage. Lineage: AWS S3. See `docs/contracts/blob.md`.
    pub fn blob(&self) -> &dyn Blob {
        self.inner.blob.as_ref()
    }

    /// Auth primitives: passwords, sessions, API keys. Lineage: OWASP + PHC + Stripe/GitHub
    /// keys. See `docs/contracts/auth.md`.
    pub fn auth(&self) -> &dyn Auth {
        self.inner.auth.as_ref()
    }

    /// Recurring + one-shot scheduling. Lineage: cron + Unix `at` + k8s CronJob. See
    /// `docs/contracts/schedule.md`. Register work here; drive ticks with
    /// [`Forge::run_scheduler`].
    pub fn schedule(&self) -> &dyn Schedule {
        self.inner.schedule.as_ref()
    }

    /// Run one scheduler pass, firing every due schedule once, and return how many jobs were
    /// enqueued. For tests or a custom loop; most apps call [`Forge::run_scheduler`]. Safe to
    /// run concurrently across replicas.
    pub async fn run_scheduler_once(&self) -> Result<u64> {
        self.inner.schedule.process_due().await
    }

    /// Run the scheduler loop until SIGINT/SIGTERM, firing due schedules roughly every 30s.
    /// Run it on every replica; each tick enqueues exactly once.
    pub async fn run_scheduler(&self) {
        let mut shutdown = std::pin::pin!(queue::worker::shutdown_signal());
        loop {
            if let Err(e) = self.inner.schedule.process_due().await {
                tracing::warn!(error = %e, "scheduler tick failed; will retry");
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {},
                _ = &mut shutdown => break,
            }
        }
    }

    /// A managed worker for `queue_name`: bounded concurrency, auto-heartbeat, `ack`/`nack`
    /// on completion, graceful shutdown.
    pub fn worker(&self, queue_name: impl Into<String>) -> WorkerBuilder {
        WorkerBuilder::new(self.inner.queue.clone(), queue_name)
    }

    /// Run the maintenance sweep across every backend: purge expired kv rows and old
    /// completed jobs, reclaim leases orphaned by crashed workers, drop stale dedup and
    /// rate-limit rows, expire dead sessions, and reclaim orphaned filesystem blobs.
    /// Idempotent; call it on a schedule.
    pub async fn maintain(&self) -> Result<()> {
        for backend in &self.inner.lifecycle {
            backend.maintain().await?;
        }
        Ok(())
    }
}

/// Builds a [`Forge`] with externally-implemented backends swapped in per primitive. Start
/// from [`Forge::builder`], point it at the system database, inject a backend for any
/// primitive you want to own, and leave the rest on their config-selected built-in:
///
/// ```no_run
/// # use std::sync::Arc;
/// # async fn demo(custom_kv: Arc<dyn forge::KvBackend>) -> forge::Result<()> {
/// let forge = forge::Forge::builder()
///     .postgres("postgres://localhost/myapp_forge")
///     .kv(custom_kv) // kv runs on your backend; the other seven stay on Postgres
///     .build()
///     .await?;
/// # let _ = forge; Ok(())
/// # }
/// ```
///
/// An injected primitive supplies its own state and lifecycle, so Forge never connects or
/// migrates Postgres on its behalf. Other knobs (namespaces, per-feature databases, blob
/// signing) come from a [`ForgeConfig`] passed to [`config`](ForgeBuilder::config); the
/// builder itself stays small.
pub struct ForgeBuilder {
    cfg: ForgeConfig,
    injected: Injected,
}

impl ForgeBuilder {
    /// Set the mandatory system database connection string. Equivalent to setting `postgres`
    /// on the inner [`ForgeConfig`]; required unless [`config`](Self::config) carries one.
    pub fn postgres(mut self, url: impl Into<String>) -> Self {
        self.cfg.postgres = url.into();
        self
    }

    /// Supply the full base [`ForgeConfig`]. Replaces the builder's config wholesale, so set
    /// it before [`postgres`](Self::postgres) if you use both, or just set `postgres` on the
    /// config you pass here.
    pub fn config(mut self, cfg: ForgeConfig) -> Self {
        self.cfg = cfg;
        self
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
    /// [`config`](Self::config), which supplies the base [`ForgeConfig`].
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
    /// [`Forge::init`]. Fails with [`ForgeError::Config`] if no system database was set.
    pub async fn build(self) -> Result<Forge> {
        if self.cfg.postgres.trim().is_empty() {
            return Err(ForgeError::config(
                "Forge::builder requires a system database; call .postgres(url) (or .config(cfg) with one set)",
            ));
        }
        Forge::build_from(self.cfg, self.injected).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The warning decision flags exactly the two delivery/sharing primitives when they
    /// resolve to memory, regardless of order, with no database.
    #[test]
    fn non_durable_warnings_flags_memory_pubsub_and_ratelimit() {
        // default_backend(Memory) makes every primitive memory; only pubsub and ratelimit
        // should be flagged, proving the helper filters rather than echoing the config.
        let cfg = ForgeConfig::new("postgres://x/y").default_backend(Backend::Memory);
        let mut got = non_durable_warnings(&cfg);
        got.sort_by_key(|p| p.as_str());
        let mut want = [Primitive::Pubsub, Primitive::RateLimit];
        want.sort_by_key(|p| p.as_str());
        assert_eq!(got, want);
    }

    #[test]
    fn non_durable_warnings_empty_for_all_postgres() {
        let cfg = ForgeConfig::new("postgres://x/y");
        assert!(non_durable_warnings(&cfg).is_empty());
    }
}

//! Forge — the standard library for agent-built SaaS.
//!
//! One crate, every backend primitive an app needs, hardened once and built on
//! interfaces the industry already trusts. See the per-primitive contracts in
//! `docs/contracts/`.
//!
//! Forge requires its own **system database**: a Postgres database it fully owns, kept
//! separate from your application's database. [`Forge::init`] connects to it and migrates
//! its `forge_*` tables at startup. Individual primitives can be pointed at their own
//! database via [`ForgeConfig::with_feature_database`], but a system database is always
//! required.
//!
//! ```no_run
//! # async fn demo() -> forge::Result<()> {
//! use forge::{Forge, ForgeConfig};
//! // A database Forge owns — not the one holding your application tables.
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
pub mod types;
pub mod typed;

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

// Re-exported so callers needn't add a separate `bytes` dependency.
pub use bytes::Bytes;

#[cfg(feature = "otel")]
pub use obs::install_otlp;

/// The single handle an application holds. Cheap to clone (`Arc` inside) and
/// `Send + Sync`. Construct it once with [`Forge::init`]; it owns the pool and
/// every primitive.
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

/// The primitives whose correctness leans on cross-process delivery/sharing — pubsub and
/// ratelimit — that resolve to a non-durable (memory) backend under `cfg`. Kept pure and
/// DB-free so the warning decision is unit-testable; [`Forge::build_from`] iterates it to
/// emit one warning per affected primitive before construction.
fn non_durable_warnings(cfg: &ForgeConfig) -> Vec<Primitive> {
    [Primitive::Pubsub, Primitive::RateLimit]
        .into_iter()
        .filter(|&p| cfg.backend_for(p) == Backend::Memory)
        .collect()
}

/// Warn that a non-durable backend was selected for a primitive whose correctness depends
/// on cross-process delivery/sharing (pubsub, ratelimit).
fn warn_non_durable(p: Primitive) {
    tracing::warn!(
        primitive = p.as_str(),
        "non-durable backend selected for a primitive whose correctness depends on cross-process delivery/sharing"
    );
}

/// Externally-supplied backends, one optional slot per primitive, threaded through the
/// single construction path. A present slot is used as both the operation trait and the
/// lifecycle handle and suppresses any Postgres connect/migrate on that primitive's behalf
/// — it brings its own state and lifecycle, exactly like an in-process backend.
/// [`Forge::init`] passes the all-`None` default; [`ForgeBuilder`] fills the slots.
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
    /// Whether a backend was injected for `p`. Drives the pool/migration plan: an injected
    /// primitive is excluded from the Postgres-backed set, like a memory one.
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
    /// Validate config, connect, migrate the schema, and construct every primitive.
    /// Forge owns its system database, so init migrates it (and every distinct feature
    /// database) at startup — idempotent and safe to run concurrently across replicas:
    /// an advisory lock serializes it and checksums guard immutability. Misconfiguration
    /// fails here with [`ForgeError::Config`], never lazily on first use.
    pub async fn init(cfg: ForgeConfig) -> Result<Self> {
        Self::build_from(cfg, Injected::default()).await
    }

    /// Start a builder for the open injection escape hatch: keep most primitives on their
    /// config-selected built-in, but swap in an externally-implemented backend for one or
    /// more. See [`ForgeBuilder`].
    pub fn builder() -> ForgeBuilder {
        ForgeBuilder {
            cfg: ForgeConfig::default(),
            injected: Injected::default(),
        }
    }

    /// The single construction path behind both [`Forge::init`] and [`Forge::builder`].
    /// For each primitive with an injected backend, that backend is used as both the
    /// operation trait and the lifecycle handle, and no Postgres connect/migrate happens on
    /// its behalf; every other primitive falls back to its config-selected built-in. The
    /// pool/migration plan is therefore built from the Postgres-backed *built-in* set only.
    async fn build_from(cfg: ForgeConfig, injected: Injected) -> Result<Self> {
        cfg.validate()?;

        // Warn once, up front, for every primitive whose correctness leans on cross-process
        // delivery/sharing but resolves to a non-durable memory backend (pubsub, ratelimit).
        for p in non_durable_warnings(&cfg) {
            warn_non_durable(p);
        }

        // A primitive on the in-process backend, or one supplied by an injected backend,
        // must never trigger a Postgres connect or migrate, so the pool/migration plan is
        // built from the Postgres-backed built-in set only. Blob counts as Postgres-backed
        // for both BYTEA and filesystem, since filesystem still keeps its metadata in
        // Postgres.
        let is_pg_backed = |p: Primitive| -> bool {
            if injected.is_injected(p) {
                false
            } else if p == Primitive::Blob {
                !matches!(cfg.blob_backend, BlobBackendConfig::Memory)
            } else {
                cfg.backend_for(p) == Backend::Postgres
            }
        };

        // The system pool is mandatory: Forge owns its system database and migrates it at
        // init even when every primitive is in-memory. Each Postgres-backed feature
        // override gets its own isolated pool; a memory-backed primitive's feature-database
        // override is ignored entirely — no connect, no migrate.
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
        // targets of Postgres-backed feature overrides. Memory-backed features never made
        // it into `feature_pools`, so they contribute no migration target.
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

        // Each primitive resolves to its operation handle plus the lifecycle handle off the
        // same object. An injected backend wins outright (upcast to both halves); otherwise
        // the config-selected built-in is constructed, drawing its pool only here. Every
        // primitive now ships both a Postgres and an in-process built-in; the non-durable
        // warning for memory pubsub/ratelimit was emitted above.
        let (kv, kv_life): (Arc<dyn Kv>, Arc<dyn BackendLifecycle>) = match injected.kv {
            Some(b) => (b.clone(), b),
            None => match cfg.backend_for(Primitive::Kv) {
                Backend::Postgres => {
                    let k = Arc::new(kv::PgKv::new(
                        pool_for(Primitive::Kv),
                        cfg.kv_namespace.clone(),
                    ));
                    (k.clone(), k)
                }
                Backend::Memory => {
                    let k = Arc::new(kv::MemKv::new(cfg.kv_namespace.clone()));
                    (k.clone(), k)
                }
            },
        };
        let (queue, queue_life): (Arc<dyn Queue>, Arc<dyn BackendLifecycle>) = match injected.queue
        {
            Some(b) => (b.clone(), b),
            None => match cfg.backend_for(Primitive::Queue) {
                Backend::Postgres => {
                    let q = Arc::new(queue::PgQueue::new(
                        pool_for(Primitive::Queue),
                        cfg.queue_dedup_window,
                        cfg.queue_retention,
                        cfg.kv_namespace.clone(),
                    ));
                    (q.clone(), q)
                }
                Backend::Memory => {
                    let q = Arc::new(queue::MemQueue::new(
                        cfg.queue_dedup_window,
                        cfg.queue_retention,
                        cfg.kv_namespace.clone(),
                    ));
                    (q.clone(), q)
                }
            },
        };
        let (config, config_life): (Arc<dyn ConfigStore>, Arc<dyn BackendLifecycle>) =
            match injected.config {
                Some(b) => (b.clone(), b),
                None => match cfg.backend_for(Primitive::Config) {
                    Backend::Postgres => {
                        let c = Arc::new(config_store::PgConfig::new(
                            pool_for(Primitive::Config),
                            cfg.kv_namespace.clone(),
                        ));
                        (c.clone(), c)
                    }
                    Backend::Memory => {
                        let c = Arc::new(config_store::MemConfig::new(cfg.kv_namespace.clone()));
                        (c.clone(), c)
                    }
                },
            };
        let (ratelimit, ratelimit_life): (Arc<dyn RateLimit>, Arc<dyn BackendLifecycle>) =
            match injected.ratelimit {
                Some(b) => (b.clone(), b),
                None => match cfg.backend_for(Primitive::RateLimit) {
                    Backend::Postgres => {
                        let r = Arc::new(ratelimit::PgRateLimit::new(
                            pool_for(Primitive::RateLimit),
                            cfg.kv_namespace.clone(),
                            cfg.ratelimit_fail_open,
                        ));
                        (r.clone(), r)
                    }
                    Backend::Memory => {
                        let r = Arc::new(ratelimit::MemRateLimit::new(
                            cfg.kv_namespace.clone(),
                            cfg.ratelimit_fail_open,
                        ));
                        (r.clone(), r)
                    }
                },
            };
        let (auth, auth_life): (Arc<dyn Auth>, Arc<dyn BackendLifecycle>) = match injected.auth {
            Some(b) => (b.clone(), b),
            None => match cfg.backend_for(Primitive::Auth) {
                Backend::Postgres => {
                    let a = Arc::new(auth::PgAuth::new(
                        pool_for(Primitive::Auth),
                        cfg.kv_namespace.clone(),
                    ));
                    (a.clone(), a)
                }
                Backend::Memory => {
                    let a = Arc::new(auth::MemAuth::new(cfg.kv_namespace.clone()));
                    (a.clone(), a)
                }
            },
        };
        let (pubsub, pubsub_life): (Arc<dyn Pubsub>, Arc<dyn BackendLifecycle>) =
            match injected.pubsub {
                Some(b) => (b.clone(), b),
                // Postgres pubsub needs the connection URL (not just the pool) for LISTEN/NOTIFY.
                None => match cfg.backend_for(Primitive::Pubsub) {
                    Backend::Postgres => {
                        let p = Arc::new(pubsub::PgPubsub::new(
                            pool_for(Primitive::Pubsub),
                            cfg.database_for(Primitive::Pubsub).postgres,
                            cfg.kv_namespace.clone(),
                        ));
                        (p.clone(), p)
                    }
                    Backend::Memory => {
                        let p = Arc::new(pubsub::MemPubsub::new(cfg.kv_namespace.clone()));
                        (p.clone(), p)
                    }
                },
            };
        let (schedule, schedule_life): (Arc<dyn Schedule>, Arc<dyn BackendLifecycle>) =
            match injected.schedule {
                Some(b) => (b.clone(), b),
                None => match cfg.backend_for(Primitive::Schedule) {
                    Backend::Postgres => {
                        let s = Arc::new(schedule::PgSchedule::new(
                            pool_for(Primitive::Schedule),
                            cfg.kv_namespace.clone(),
                        ));
                        (s.clone(), s)
                    }
                    Backend::Memory => {
                        // The scheduler delivers through the resolved queue backend, so a
                        // memory-backed schedule actually enqueues work (queue is built above).
                        let s = Arc::new(schedule::MemSchedule::new(
                            cfg.kv_namespace.clone(),
                            queue.clone(),
                        ));
                        (s.clone(), s)
                    }
                },
            };

        // The blob backend choice: an injected store, else bytes in BYTEA, on a filesystem
        // directory, or (not yet) in memory.
        let (blob, blob_life): (Arc<dyn Blob>, Arc<dyn BackendLifecycle>) = match injected.blob {
            Some(b) => (b.clone(), b),
            None => match &cfg.blob_backend {
                BlobBackendConfig::Postgres => {
                    let b = Arc::new(blob::PgBlob::new(
                        pool_for(Primitive::Blob),
                        cfg.kv_namespace.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                    ));
                    (b.clone(), b)
                }
                BlobBackendConfig::Filesystem { root } => {
                    let b = Arc::new(blob::FsBlob::new(
                        pool_for(Primitive::Blob),
                        cfg.kv_namespace.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                        root.clone(),
                    )?);
                    (b.clone(), b)
                }
                BlobBackendConfig::Memory => {
                    let b = Arc::new(blob::MemBlob::new(
                        cfg.kv_namespace.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                    ));
                    (b.clone(), b)
                }
            },
        };

        // One lifecycle handle per primitive (Primitive order), for maintain + report. Each
        // is the object the primitive actually resolved to, so `backend_report()` reflects
        // the live choice (an injected or memory backend reports its own name/durability).
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

    /// A snapshot of which backend powers each primitive — for logs, health pages, and
    /// debugging. Not needed for ordinary request handling (the provider must never
    /// leak into app logic).
    pub fn backend_report(&self) -> BackendReport {
        let backends = self
            .inner
            .lifecycle
            .iter()
            .map(|b| BackendInfo::new(b.primitive(), b.name(), b.durable(), b.caveats()))
            .collect();
        BackendReport::new(backends)
    }

    /// The pool to Forge's **system database** — the one every feature without a
    /// per-feature override shares. This is Forge's own database, *not* your application's;
    /// it holds the `forge_*` tables and Forge migrates it at init. Exposed as an escape
    /// hatch for Forge-adjacent SQL (a read against a `forge_*` table, a one-off in a
    /// migration job) — not as a home for your application's domain tables, which belong in
    /// your own database. Features given their own database via
    /// [`ForgeConfig::with_feature_database`] run on separate, isolated pools not reachable
    /// through this accessor.
    ///
    /// Using it ties the caller to Forge's `sqlx` major version; that is the price of
    /// sharing the connection pool, and it is opt-in.
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

    /// Live publish/subscribe for realtime fan-out (subscriptions, presence).
    /// Lineage: Postgres LISTEN/NOTIFY + Redis pub/sub. See `docs/contracts/pubsub.md`.
    /// Not durable — use [`Forge::queue`] when a message must not be lost.
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

    /// Auth primitives: passwords, sessions, API keys. Lineage: OWASP + PHC +
    /// Stripe/GitHub keys. See `docs/contracts/auth.md`.
    pub fn auth(&self) -> &dyn Auth {
        self.inner.auth.as_ref()
    }

    /// Recurring + one-shot scheduling. Lineage: cron + Unix `at` + k8s CronJob. See
    /// `docs/contracts/schedule.md`. Register work via this; drive ticks with
    /// [`Forge::run_scheduler`].
    pub fn schedule(&self) -> &dyn Schedule {
        self.inner.schedule.as_ref()
    }

    /// Run one scheduler pass — fire every due schedule once — and return how many
    /// jobs were enqueued. For tests or a custom loop; most apps call
    /// [`Forge::run_scheduler`]. Safe to run concurrently across replicas.
    pub async fn run_scheduler_once(&self) -> Result<u64> {
        self.inner.schedule.process_due().await
    }

    /// Run the scheduler loop until SIGINT/SIGTERM, firing due schedules roughly every
    /// 30s. Run it on every replica — each tick enqueues exactly once.
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

    /// A managed worker for `queue_name`: bounded concurrency, auto-heartbeat,
    /// `ack`/`nack` on completion, graceful shutdown.
    pub fn worker(&self, queue_name: impl Into<String>) -> WorkerBuilder {
        WorkerBuilder::new(self.inner.queue.clone(), queue_name)
    }

    /// Run the maintenance sweep across every backend: purge expired kv rows and old
    /// completed jobs, reclaim leases orphaned by crashed workers, drop stale dedup and
    /// rate-limit rows, expire dead sessions, and (filesystem blob) reclaim orphaned
    /// files. Idempotent; call it on a schedule. Drives each backend's lifecycle hook.
    pub async fn maintain(&self) -> Result<()> {
        for backend in &self.inner.lifecycle {
            backend.maintain().await?;
        }
        Ok(())
    }
}

/// Builds a [`Forge`] with externally-implemented backends swapped in per primitive — the
/// open injection escape hatch. Start from [`Forge::builder`], point it at the mandatory
/// system database, inject a backend for any primitive you want to own, and leave the rest
/// on their config-selected built-in:
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
/// migrates Postgres on its behalf. Knobs beyond the system database (namespaces, per-
/// feature databases, blob signing, …) are set on a [`ForgeConfig`] passed to
/// [`config`](ForgeBuilder::config); the builder itself stays deliberately small.
pub struct ForgeBuilder {
    cfg: ForgeConfig,
    injected: Injected,
}

impl ForgeBuilder {
    /// Set the mandatory system database connection string. Equivalent to setting
    /// `postgres` on the inner [`ForgeConfig`]; required unless [`config`](Self::config)
    /// already carries one.
    pub fn postgres(mut self, url: impl Into<String>) -> Self {
        self.cfg.postgres = url.into();
        self
    }

    /// Supply the full base [`ForgeConfig`]. Replaces the builder's config wholesale, so set
    /// it before calling [`postgres`](Self::postgres) if you use both, or just set `postgres`
    /// on the config you pass here.
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

    /// Inject the config-store backend. (Named `config_store` so it does not collide with
    /// [`config`](Self::config), which supplies the base [`ForgeConfig`].)
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

    /// A pure, DB-free check: the warning decision flags exactly the two delivery/sharing
    /// primitives when they resolve to memory, regardless of order.
    #[test]
    fn non_durable_warnings_flags_memory_pubsub_and_ratelimit() {
        // default_backend(Memory) makes every primitive memory; only pubsub and ratelimit
        // should be flagged, proving the helper filters rather than echoing the whole config.
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

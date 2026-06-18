//! Forge — the standard library for agent-built SaaS.
//!
//! One crate, one Postgres connection, every backend primitive an app needs,
//! hardened once and built on interfaces the industry already trusts. See the
//! per-primitive contracts in `docs/contracts/`.
//!
//! ```no_run
//! # async fn demo() -> forge::Result<()> {
//! use forge::{Forge, ForgeConfig};
//! let forge = Forge::init(ForgeConfig::new("postgres://localhost/myapp")).await?;
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

mod obs;
mod types;
mod util;

/// Sealing. The primitive traits (`Kv`, `Queue`, `Blob`, …) are public to *call*
/// but not to *implement* outside this crate: each has `sealed::Sealed` as a
/// supertrait, and `Sealed` cannot be named (let alone implemented) by downstream
/// crates. This keeps the traits a one-way contract, so methods can be added on
/// point releases without breaking external code. New backends are added *inside*
/// Forge via `backend.rs`; a deliberate, versioned provider SPI can come later.
pub(crate) mod sealed {
    pub trait Sealed {}
}

#[cfg(feature = "postgres")]
mod pg;

#[cfg(feature = "pg-tests")]
pub mod testing;

use std::sync::Arc;

pub use auth::{
    ApiKey, ApiKeyInfo, ApiKeySecret, Auth, PhcString, Session, SessionOpts, SessionToken,
};
pub use backend::{BackendHealth, BackendInfo, BackendLifecycle, BackendReport, Primitive};
pub use blob::{Blob, BlobInfo, ListPage, PutOpts};
pub use config::{BlobBackendConfig, ForgeConfig};
pub use config_store::{ConfigExt, ConfigStore, EvalCtx, FlagRule};
pub use error::{ForgeError, Result};
pub use kv::{Kv, KvExt, SetMode, SetOpts};
pub use pubsub::{Pubsub, Subscription};
pub use queue::worker::WorkerBuilder;
pub use queue::{
    Backoff, DequeueOpts, EnqueueOpts, Job, JobId, NackOpts, Queue, QueueDepth, QueueExt,
};
pub use ratelimit::{Algo, Decision, FailMode, Limit, RateLimit};
pub use schedule::{Schedule, ScheduleInfo, ScheduleKind};
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
    /// The Postgres pool, when this `Forge` was built on one (always, via `init`/the
    /// builder; optional only through `from_parts`).
    #[cfg(feature = "postgres")]
    pool: Option<sqlx::PgPool>,
}

impl Forge {
    /// Validate config, connect, run (or verify) migrations, and construct every
    /// primitive. Misconfiguration fails here with [`ForgeError::Config`], never
    /// lazily on first use.
    pub async fn init(cfg: ForgeConfig) -> Result<Self> {
        cfg.validate()?;
        let pool = pg::connect(&cfg).await?;

        let runner = pg::MigrationRunner::new(pool.clone());
        if cfg.run_migrations {
            runner.run().await?;
        } else {
            runner.verify_only().await?;
        }

        let secret = cfg.blob_signing_secret.clone().map(String::into_bytes);

        let kv = Arc::new(kv::PgKv::new(pool.clone(), cfg.kv_namespace.clone()));
        let queue = Arc::new(queue::PgQueue::new(
            pool.clone(),
            cfg.queue_dedup_window,
            cfg.queue_retention,
        ));
        let config = Arc::new(config_store::PgConfig::new(pool.clone()));
        let ratelimit = Arc::new(ratelimit::PgRateLimit::new(
            pool.clone(),
            cfg.kv_namespace.clone(),
            cfg.ratelimit_fail_open,
        ));
        let auth = Arc::new(auth::PgAuth::new(pool.clone()));
        let pubsub = Arc::new(pubsub::PgPubsub::new(pool.clone(), cfg.postgres.clone()));
        let schedule = Arc::new(schedule::PgSchedule::new(pool.clone()));

        // The one v1 backend choice: blob bytes in BYTEA, or on a filesystem directory.
        let (blob, blob_lifecycle): (Arc<dyn Blob>, Arc<dyn BackendLifecycle>) =
            match &cfg.blob_backend {
                BlobBackendConfig::Postgres => {
                    let b = Arc::new(blob::PgBlob::new(
                        pool.clone(),
                        cfg.kv_namespace.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                    ));
                    (b.clone() as Arc<dyn Blob>, b as Arc<dyn BackendLifecycle>)
                }
                BlobBackendConfig::Filesystem { root } => {
                    let b = Arc::new(blob::FsBlob::new(
                        pool.clone(),
                        cfg.kv_namespace.clone(),
                        secret.clone(),
                        cfg.blob_base_url.clone(),
                        root.clone(),
                    )?);
                    (b.clone() as Arc<dyn Blob>, b as Arc<dyn BackendLifecycle>)
                }
            };

        // One lifecycle handle per primitive (Primitive order), for maintain + report.
        let lifecycle: Vec<Arc<dyn BackendLifecycle>> = vec![
            kv.clone() as Arc<dyn BackendLifecycle>,
            queue.clone() as Arc<dyn BackendLifecycle>,
            blob_lifecycle,
            auth.clone() as Arc<dyn BackendLifecycle>,
            config.clone() as Arc<dyn BackendLifecycle>,
            ratelimit.clone() as Arc<dyn BackendLifecycle>,
            schedule.clone() as Arc<dyn BackendLifecycle>,
            pubsub.clone() as Arc<dyn BackendLifecycle>,
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
                pool: Some(pool),
            }),
        })
    }

    /// A builder for selecting per-primitive backends at construction. v1's one choice
    /// is the blob backend; the same shape accepts later per-primitive backends.
    ///
    /// ```no_run
    /// # async fn demo() -> forge::Result<()> {
    /// use forge::Forge;
    /// let forge = Forge::builder()
    ///     .postgres("postgres://localhost/myapp")
    ///     .filesystem_blob("/var/lib/app/blobs")
    ///     .build()
    ///     .await?;
    /// # let _ = forge; Ok(())
    /// # }
    /// ```
    pub fn builder() -> ForgeBuilder {
        ForgeBuilder {
            cfg: ForgeConfig::default(),
        }
    }

    /// Construct a `Forge` from caller-provided primitive implementations — the escape
    /// hatch for external provider crates that implement Forge's traits without
    /// forking. Calls each backend's `init` lifecycle hook. Built-in deployments should
    /// use [`Forge::init`] / [`Forge::builder`] instead.
    pub async fn from_parts(parts: ForgeParts) -> Result<Self> {
        for backend in &parts.lifecycle {
            backend.init().await?;
        }
        Ok(Self {
            inner: Arc::new(ForgeInner {
                kv: parts.kv,
                queue: parts.queue,
                config: parts.config,
                ratelimit: parts.ratelimit,
                blob: parts.blob,
                auth: parts.auth,
                schedule: parts.schedule,
                pubsub: parts.pubsub,
                lifecycle: parts.lifecycle,
                #[cfg(feature = "postgres")]
                pool: parts.pool,
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
            .map(|b| BackendInfo {
                primitive: b.primitive(),
                provider: b.name(),
                durable: b.durable(),
                caveats: b.caveats(),
            })
            .collect();
        BackendReport { backends }
    }

    /// The underlying Postgres pool that backs every primitive. Exposed as an
    /// escape hatch so an application can run its *own* domain SQL on the same pool
    /// Forge already manages, rather than opening a second pool to the same database.
    ///
    /// Using it ties the application to Forge's `sqlx` major version; that is the
    /// price of sharing the connection pool, and it is opt-in.
    #[cfg(feature = "postgres")]
    pub fn pool(&self) -> &sqlx::PgPool {
        self.inner
            .pool
            .as_ref()
            .expect("Forge::pool() requires a Forge built with a Postgres pool (init/builder)")
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
        self.run_scheduler_until(queue::worker::shutdown_signal())
            .await;
    }

    /// Like [`Forge::run_scheduler`] but stops when `shutdown` resolves (for tests or
    /// custom lifecycle management). Ticks every 30s.
    pub async fn run_scheduler_until<S: std::future::Future<Output = ()> + Send>(
        &self,
        shutdown: S,
    ) {
        self.run_scheduler_with(std::time::Duration::from_secs(30), shutdown)
            .await;
    }

    /// Like [`Forge::run_scheduler_until`] but with a caller-chosen tick `interval`
    /// instead of the fixed 30s — so an app needn't hand-roll its own
    /// `process_due` loop just to change the cadence (e.g. a short tick in tests).
    /// Each tick enqueues every due schedule exactly once; safe across replicas.
    pub async fn run_scheduler_with<S: std::future::Future<Output = ()> + Send>(
        &self,
        interval: std::time::Duration,
        shutdown: S,
    ) {
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            if let Err(e) = self.inner.schedule.process_due().await {
                tracing::warn!(error = %e, "scheduler tick failed; will retry");
            }
            tokio::select! {
                _ = tokio::time::sleep(interval) => {},
                _ = &mut shutdown => break,
            }
        }
    }

    /// An axum router that serves presigned blob URLs against the Postgres backend.
    /// Mount it where the presigned URLs point (the configured `blob_base_url`):
    /// `app.nest("/_forge/blob", forge.blob_router()?)`. Requires
    /// `ForgeConfig.blob_signing_secret`; errors with `Config` otherwise.
    #[cfg(feature = "blob-router")]
    pub fn blob_router(&self) -> Result<axum::Router> {
        if !self.inner.blob.presign_ready() {
            return Err(ForgeError::config(
                "blob_router requires ForgeConfig.blob_signing_secret to be set",
            ));
        }
        Ok(blob::router::router(self.inner.blob.clone()))
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

/// Builder for [`Forge`] with per-primitive backend selection. See [`Forge::builder`].
#[derive(Debug, Clone, Default)]
pub struct ForgeBuilder {
    cfg: ForgeConfig,
}

impl ForgeBuilder {
    /// Set the Postgres connection string (required).
    pub fn postgres(mut self, url: impl Into<String>) -> Self {
        self.cfg.postgres = url.into();
        self
    }

    /// Select the blob byte-storage backend.
    pub fn blob(mut self, backend: BlobBackendConfig) -> Self {
        self.cfg.blob_backend = backend;
        self
    }

    /// Store blob bytes on a local filesystem directory (metadata stays in Postgres).
    pub fn filesystem_blob(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.cfg.blob_backend = BlobBackendConfig::Filesystem { root: root.into() };
        self
    }

    /// Set the HMAC secret enabling presigned blob URLs.
    pub fn blob_signing_secret(mut self, secret: impl Into<String>) -> Self {
        self.cfg.blob_signing_secret = Some(secret.into());
        self
    }

    /// Set the kv key namespace (shared by kv, ratelimit, and blob).
    pub fn kv_namespace(mut self, ns: impl Into<String>) -> Self {
        self.cfg.kv_namespace = ns.into();
        self
    }

    /// Set the maximum pool size.
    pub fn max_connections(mut self, n: u32) -> Self {
        self.cfg.max_connections = n;
        self
    }

    /// Replace the whole config (escape hatch for knobs without a builder method).
    pub fn config(mut self, cfg: ForgeConfig) -> Self {
        self.cfg = cfg;
        self
    }

    /// Build and initialize the `Forge`.
    pub async fn build(self) -> Result<Forge> {
        Forge::init(self.cfg).await
    }
}

/// Caller-provided primitive implementations for [`Forge::from_parts`]. A plain struct
/// (constructible by external provider crates); fields are coerced trait objects so any
/// backend that implements Forge's traits can be plugged in. `lifecycle` should carry
/// one [`BackendLifecycle`] per primitive so `maintain`/`backend_report` see them.
pub struct ForgeParts {
    pub kv: Arc<dyn Kv>,
    pub queue: Arc<dyn Queue>,
    pub blob: Arc<dyn Blob>,
    pub auth: Arc<dyn Auth>,
    pub config: Arc<dyn ConfigStore>,
    pub ratelimit: Arc<dyn RateLimit>,
    pub schedule: Arc<dyn Schedule>,
    pub pubsub: Arc<dyn Pubsub>,
    pub lifecycle: Vec<Arc<dyn BackendLifecycle>>,
    /// The Postgres pool, if any primitive is Postgres-backed (enables [`Forge::pool`]).
    #[cfg(feature = "postgres")]
    pub pool: Option<sqlx::PgPool>,
}

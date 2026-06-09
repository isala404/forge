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
pub mod blob;
pub mod config;
pub mod config_store;
pub mod error;
pub mod kv;
pub mod pubsub;
pub mod queue;
pub mod ratelimit;
pub mod schedule;

mod obs;
mod types;
mod util;

#[cfg(feature = "postgres")]
mod pg;

#[cfg(feature = "pg-tests")]
pub mod testing;

use std::sync::Arc;

pub use auth::{
    ApiKey, ApiKeyInfo, ApiKeySecret, Auth, PhcString, Session, SessionOpts, SessionToken,
};
pub use blob::{Blob, BlobInfo, ListPage, PutOpts};
pub use config::ForgeConfig;
pub use config_store::{ConfigExt, ConfigStore, EvalCtx, FlagRule};
pub use error::{ForgeError, Result};
pub use kv::{Kv, KvExt, SetMode, SetOpts};
pub use pubsub::{Pubsub, Subscription};
pub use queue::worker::WorkerBuilder;
pub use queue::{Backoff, DequeueOpts, EnqueueOpts, Job, JobId, NackOpts, Queue, QueueExt};
pub use ratelimit::{Algo, Decision, Limit, RateLimit};
pub use schedule::{Schedule, ScheduleInfo, ScheduleKind};
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
    kv: Arc<kv::PgKv>,
    queue: Arc<queue::PgQueue>,
    config: Arc<config_store::PgConfig>,
    ratelimit: Arc<ratelimit::PgRateLimit>,
    blob: Arc<blob::PgBlob>,
    auth: Arc<auth::PgAuth>,
    schedule: Arc<schedule::PgSchedule>,
    pubsub: Arc<pubsub::PgPubsub>,
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
        let blob = Arc::new(blob::PgBlob::new(
            pool.clone(),
            cfg.kv_namespace.clone(),
            cfg.blob_signing_secret.clone().map(String::into_bytes),
            cfg.blob_base_url.clone(),
        ));
        let auth = Arc::new(auth::PgAuth::new(pool.clone()));
        let pubsub = Arc::new(pubsub::PgPubsub::new(pool.clone(), cfg.postgres.clone()));
        let schedule = Arc::new(schedule::PgSchedule::new(pool));

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
            }),
        })
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
    /// custom lifecycle management).
    pub async fn run_scheduler_until<S: std::future::Future<Output = ()> + Send>(
        &self,
        shutdown: S,
    ) {
        let mut shutdown = std::pin::pin!(shutdown);
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

    /// An axum router that serves presigned blob URLs against the Postgres backend.
    /// Mount it where the presigned URLs point (the configured `blob_base_url`):
    /// `app.nest("/_forge/blob", forge.blob_router()?)`. Requires
    /// `ForgeConfig.blob_signing_secret`; errors with `Config` otherwise.
    #[cfg(feature = "blob-router")]
    pub fn blob_router(&self) -> Result<axum::Router> {
        if self.inner.blob.signing_secret().is_none() {
            return Err(ForgeError::config(
                "blob_router requires ForgeConfig.blob_signing_secret to be set",
            ));
        }
        Ok(blob::router::router(self.inner.blob.clone()))
    }

    /// A managed worker for `queue_name`: bounded concurrency, auto-heartbeat,
    /// `ack`/`nack` on completion, graceful shutdown.
    pub fn worker(&self, queue_name: impl Into<String>) -> WorkerBuilder {
        WorkerBuilder::new(self.inner.queue.clone() as Arc<dyn Queue>, queue_name)
    }

    /// Run the maintenance sweep: purge expired kv rows and old completed jobs,
    /// reclaim leases orphaned by crashed workers across all queues, and drop
    /// stale dedup entries. Idempotent; call it on a schedule.
    pub async fn maintain(&self) -> Result<()> {
        self.inner.kv.sweep().await?;
        self.inner.queue.maintenance().await?;
        self.inner.ratelimit.sweep().await?;
        self.inner.auth.sweep().await?;
        Ok(())
    }
}

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

mod backends;
pub mod config;
pub mod core;
pub mod error;
mod obs;
mod util;
pub mod worker;

#[cfg(feature = "pg-tests")]
pub mod testing;

use std::sync::Arc;

pub use config::ForgeConfig;
pub use core::{
    Backoff, Cursor, DequeueOpts, EnqueueOpts, Job, JobId, Kv, KvExt, NackOpts, Queue, QueueExt,
    SetMode, SetOpts,
};
pub use error::{ForgeError, Result};
pub use worker::WorkerBuilder;

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
    kv: Arc<backends::postgres::PgKv>,
    queue: Arc<backends::postgres::PgQueue>,
}

impl Forge {
    /// Validate config, connect, run (or verify) migrations, and construct every
    /// primitive. Misconfiguration fails here with [`ForgeError::Config`], never
    /// lazily on first use.
    pub async fn init(cfg: ForgeConfig) -> Result<Self> {
        cfg.validate()?;
        let pool = backends::postgres::connect(&cfg).await?;

        let runner = backends::postgres::MigrationRunner::new(pool.clone());
        if cfg.run_migrations {
            runner.run().await?;
        } else {
            runner.verify_only().await?;
        }

        let kv = Arc::new(backends::postgres::PgKv::new(
            pool.clone(),
            cfg.kv_namespace.clone(),
        ));
        let queue = Arc::new(backends::postgres::PgQueue::new(
            pool,
            cfg.queue_dedup_window,
            cfg.queue_retention,
        ));

        Ok(Self {
            inner: Arc::new(ForgeInner { kv, queue }),
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
        Ok(())
    }
}

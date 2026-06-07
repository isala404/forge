//! Node.js bindings for Forge via napi-rs.
//!
//! Exposes the kv + queue primitives to JavaScript. Async Rust methods become
//! JS `Promise`s (snake_case → camelCase). The queue is exposed as raw
//! `enqueue`/`dequeue`/`ack`/`nack`: leased jobs are held Rust-side in a map and
//! referenced from JS by id, so the opaque lease fence never crosses the boundary.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

fn err(e: forge::ForgeError) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// A leased job handed to JavaScript. `ack`/`nack` it by `id`.
#[napi(object)]
pub struct JsJob {
    pub id: String,
    pub payload: String,
    pub attempt: u32,
}

/// A Forge client: one Postgres pool, every primitive. Construct with
/// `ForgeClient.connect(url)`.
#[napi]
pub struct ForgeClient {
    forge: forge::Forge,
    /// Leased-but-not-settled jobs, keyed by job id, so `ack`/`nack` can recover
    /// the `forge::Job` (whose lease fence is not part of the public surface).
    /// Every dequeued job MUST be settled with `queue_ack`/`queue_nack`; an
    /// abandoned lease would otherwise linger here forever, so `queue_dequeue`
    /// also evicts entries whose lease has already expired (the queue redelivers
    /// them regardless).
    leased: Arc<Mutex<HashMap<String, forge::Job>>>,
}

#[napi]
impl ForgeClient {
    /// Connect, run migrations, and ping — mirrors `Forge::init`.
    #[napi(factory)]
    pub async fn connect(postgres_url: String) -> Result<ForgeClient> {
        let forge = forge::Forge::init(forge::ForgeConfig::new(postgres_url))
            .await
            .map_err(err)?;
        Ok(ForgeClient {
            forge,
            leased: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// `GET key` → the value as a UTF-8 string, or `null`.
    #[napi]
    pub async fn kv_get(&self, key: String) -> Result<Option<String>> {
        let v = self.forge.kv().get(&key).await.map_err(err)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// `SET key value`. `ttlSeconds > 0` sets a TTL; `ifNotExists` does `SET NX`.
    /// Returns whether the write happened.
    #[napi]
    pub async fn kv_set(
        &self,
        key: String,
        value: String,
        ttl_seconds: Option<f64>,
        if_not_exists: Option<bool>,
    ) -> Result<bool> {
        let mut opts = forge::SetOpts::new();
        if let Some(t) = ttl_seconds {
            if t > 0.0 {
                opts = opts.with_ttl(Duration::from_secs_f64(t));
            }
        }
        if if_not_exists.unwrap_or(false) {
            opts = opts.with_mode(forge::SetMode::IfNotExists);
        }
        self.forge
            .kv()
            .set(&key, forge::Bytes::from(value), opts)
            .await
            .map_err(err)
    }

    /// `INCRBY key by` (atomic). Returns the new value.
    #[napi]
    pub async fn kv_incr(&self, key: String, by: i32) -> Result<f64> {
        let v = self
            .forge
            .kv()
            .incr(&key, i64::from(by))
            .await
            .map_err(err)?;
        Ok(v as f64)
    }

    /// `SCAN prefix*` (first page) → up to `limit` matching keys.
    #[napi]
    pub async fn kv_scan(&self, prefix: String, limit: u32) -> Result<Vec<String>> {
        let (keys, _next) = self
            .forge
            .kv()
            .scan(&prefix, None, limit)
            .await
            .map_err(err)?;
        Ok(keys)
    }

    /// Enqueue a job (string payload). Returns the job id.
    #[napi]
    pub async fn queue_enqueue(
        &self,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
    ) -> Result<String> {
        let mut opts = forge::EnqueueOpts::new();
        if let Some(m) = max_attempts {
            opts = opts.with_max_attempts(m);
        }
        let id = self
            .forge
            .queue()
            .enqueue(&queue, forge::Bytes::from(payload), opts)
            .await
            .map_err(err)?;
        Ok(id.to_string())
    }

    /// Lease one job for `visibilitySeconds`, long-polling up to `waitSeconds`.
    /// `null` if none arrived. `ack`/`nack` it by the returned `id`.
    #[napi]
    pub async fn queue_dequeue(
        &self,
        queue: String,
        visibility_seconds: f64,
        wait_seconds: f64,
    ) -> Result<Option<JsJob>> {
        let opts = forge::DequeueOpts::new()
            .with_visibility_timeout(Duration::from_secs_f64(visibility_seconds.max(0.0)))
            .with_wait(Duration::from_secs_f64(wait_seconds.max(0.0)));
        match self
            .forge
            .queue()
            .dequeue(&queue, opts)
            .await
            .map_err(err)?
        {
            Some(job) => {
                let js = JsJob {
                    id: job.id.to_string(),
                    payload: String::from_utf8_lossy(&job.payload).into_owned(),
                    attempt: job.attempt,
                };
                let mut leased = self.leased.lock().await;
                // Drop entries for leases that already lapsed (abandoned without
                // ack/nack) so the map can't grow without bound.
                let now = SystemTime::now();
                leased.retain(|_, j| j.leased_until > now);
                leased.insert(js.id.clone(), job);
                Ok(Some(js))
            }
            None => Ok(None),
        }
    }

    /// Ack a leased job by id (idempotent).
    #[napi]
    pub async fn queue_ack(&self, id: String) -> Result<()> {
        let job = self.leased.lock().await.remove(&id);
        if let Some(job) = job {
            self.forge.queue().ack(&job).await.map_err(err)?;
        }
        Ok(())
    }

    /// Nack a leased job by id; optional `retrySeconds` delays the redelivery.
    #[napi]
    pub async fn queue_nack(&self, id: String, retry_seconds: Option<f64>) -> Result<()> {
        let job = self.leased.lock().await.remove(&id);
        if let Some(job) = job {
            let opts = match retry_seconds {
                Some(s) => forge::NackOpts::retry_in(Duration::from_secs_f64(s.max(0.0))),
                None => forge::NackOpts::default(),
            };
            self.forge.queue().nack(&job, opts).await.map_err(err)?;
        }
        Ok(())
    }
}

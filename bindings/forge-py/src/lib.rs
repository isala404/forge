//! Python bindings for Forge via pyo3.
//!
//! Exposes a representative slice of every primitive (kv, queue, config, ratelimit,
//! blob, auth, schedule) as a synchronous `ForgeClient`. Forge's API is async; each
//! method drives it to completion on an embedded Tokio runtime, so Python callers see
//! ordinary blocking methods. Leased jobs are held Rust-side and referenced by id, as
//! in the Node binding.

// Trait-object methods (forge.kv().set(), forge.auth().hash_password(), …) don't need
// the traits in scope, so only value types are imported here.
use forge::{EvalCtx, FlagRule, Forge, ForgeConfig, Limit, PutOpts, SessionOpts, SetOpts};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn pyerr(e: forge::ForgeError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// A Forge client: one Postgres pool, every primitive, driven on an embedded runtime.
/// Construct with `ForgeClient.connect(url)`.
#[pyclass]
struct ForgeClient {
    forge: Forge,
    rt: tokio::runtime::Runtime,
    leased: Mutex<HashMap<String, forge::Job>>,
}

#[pymethods]
impl ForgeClient {
    /// Connect, run migrations, and ping — mirrors `Forge::init`. Pass
    /// `signing_secret` to enable presigned blob URLs.
    #[staticmethod]
    #[pyo3(signature = (postgres_url, signing_secret=None))]
    fn connect(postgres_url: String, signing_secret: Option<String>) -> PyResult<Self> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| PyRuntimeError::new_err(format!("tokio runtime: {e}")))?;
        let mut cfg = ForgeConfig::new(postgres_url);
        if let Some(secret) = signing_secret {
            cfg = cfg.with_blob_signing_secret(secret);
        }
        let forge = rt.block_on(Forge::init(cfg)).map_err(pyerr)?;
        Ok(Self {
            forge,
            rt,
            leased: Mutex::new(HashMap::new()),
        })
    }

    // ---- kv ------------------------------------------------------------------

    #[pyo3(signature = (key, value, ttl_seconds=None))]
    fn kv_set(&self, key: String, value: String, ttl_seconds: Option<f64>) -> PyResult<bool> {
        let mut opts = SetOpts::new();
        if let Some(t) = ttl_seconds {
            if t > 0.0 {
                opts = opts.with_ttl(Duration::from_secs_f64(t));
            }
        }
        self.rt
            .block_on(self.forge.kv().set(&key, forge::Bytes::from(value), opts))
            .map_err(pyerr)
    }

    fn kv_get(&self, key: String) -> PyResult<Option<String>> {
        let v = self.rt.block_on(self.forge.kv().get(&key)).map_err(pyerr)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    fn kv_incr(&self, key: String, by: i64) -> PyResult<i64> {
        self.rt.block_on(self.forge.kv().incr(&key, by)).map_err(pyerr)
    }

    fn kv_delete(&self, key: String) -> PyResult<bool> {
        self.rt.block_on(self.forge.kv().delete(&key)).map_err(pyerr)
    }

    fn kv_exists(&self, key: String) -> PyResult<bool> {
        self.rt.block_on(self.forge.kv().exists(&key)).map_err(pyerr)
    }

    /// `SCAN prefix*` (first page) → up to `limit` matching keys.
    fn kv_scan(&self, prefix: String, limit: u32) -> PyResult<Vec<String>> {
        let (keys, _next) = self
            .rt
            .block_on(self.forge.kv().scan(&prefix, None, limit))
            .map_err(pyerr)?;
        Ok(keys)
    }

    // ---- config + flags ------------------------------------------------------

    fn config_set(&self, key: String, value: String) -> PyResult<()> {
        self.rt
            .block_on(self.forge.config().set_raw(&key, &value))
            .map_err(pyerr)
    }

    fn config_get(&self, key: String) -> PyResult<Option<String>> {
        self.rt
            .block_on(self.forge.config().get_raw(&key))
            .map_err(pyerr)
    }

    fn set_flag_percent(&self, key: String, percent: u8) -> PyResult<()> {
        self.rt
            .block_on(self.forge.config().set_flag(&key, FlagRule::Percent(percent)))
            .map_err(pyerr)
    }

    #[pyo3(signature = (key, default_value, targeting_key=None))]
    fn flag(&self, key: String, default_value: bool, targeting_key: Option<String>) -> bool {
        let ctx = match targeting_key {
            Some(k) => EvalCtx::user(k),
            None => EvalCtx::new(),
        };
        self.rt
            .block_on(self.forge.config().flag(&key, default_value, &ctx))
    }

    // ---- ratelimit -----------------------------------------------------------

    /// Atomic check-and-consume. Returns `(allowed, remaining, retry_after_seconds)`.
    fn rate_limit_check(
        &self,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
    ) -> PyResult<(bool, u32, Option<f64>)> {
        let limit = Limit::per_duration(max, Duration::from_secs_f64(per_seconds.max(1.0)));
        let d = self
            .rt
            .block_on(self.forge.ratelimit().check(&bucket, &key, limit))
            .map_err(pyerr)?;
        Ok((d.allowed, d.remaining, d.retry_after.map(|x| x.as_secs_f64())))
    }

    // ---- blob ----------------------------------------------------------------

    #[pyo3(signature = (key, data, content_type=None))]
    fn blob_put(&self, key: String, data: String, content_type: Option<String>) -> PyResult<()> {
        let mut opts = PutOpts::new();
        if let Some(ct) = content_type {
            opts = opts.with_content_type(ct);
        }
        self.rt
            .block_on(self.forge.blob().put(&key, forge::Bytes::from(data), opts))
            .map_err(pyerr)
    }

    fn blob_get(&self, key: String) -> PyResult<Option<String>> {
        let v = self.rt.block_on(self.forge.blob().get(&key)).map_err(pyerr)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    fn blob_presign_download(&self, key: String, expires_seconds: f64) -> PyResult<String> {
        self.rt
            .block_on(
                self.forge
                    .blob()
                    .presign_download(&key, Duration::from_secs_f64(expires_seconds.max(1.0))),
            )
            .map_err(pyerr)
    }

    fn blob_presign_upload(
        &self,
        key: String,
        expires_seconds: f64,
        max_bytes: u64,
    ) -> PyResult<String> {
        self.rt
            .block_on(self.forge.blob().presign_upload(
                &key,
                Duration::from_secs_f64(expires_seconds.max(1.0)),
                max_bytes,
            ))
            .map_err(pyerr)
    }

    fn blob_content_type(&self, key: String) -> PyResult<Option<String>> {
        Ok(self
            .rt
            .block_on(self.forge.blob().head(&key))
            .map_err(pyerr)?
            .map(|i| i.content_type))
    }

    fn blob_delete(&self, key: String) -> PyResult<bool> {
        self.rt.block_on(self.forge.blob().delete(&key)).map_err(pyerr)
    }

    // ---- auth ----------------------------------------------------------------

    fn hash_password(&self, plain: String) -> PyResult<String> {
        let h = self
            .rt
            .block_on(self.forge.auth().hash_password(&plain))
            .map_err(pyerr)?;
        Ok(h.as_str().to_string())
    }

    fn verify_password(&self, plain: String, hash: String) -> PyResult<bool> {
        self.rt
            .block_on(
                self.forge
                    .auth()
                    .verify_password(&plain, &forge::PhcString::new(hash)),
            )
            .map_err(pyerr)
    }

    #[pyo3(signature = (user_id, idle_seconds=None, absolute_seconds=None))]
    fn create_session(
        &self,
        user_id: String,
        idle_seconds: Option<f64>,
        absolute_seconds: Option<f64>,
    ) -> PyResult<String> {
        let mut opts = SessionOpts::new();
        if let Some(s) = idle_seconds {
            opts = opts.with_idle_timeout(Duration::from_secs_f64(s.max(1.0)));
        }
        if let Some(s) = absolute_seconds {
            opts = opts.with_absolute_timeout(Duration::from_secs_f64(s.max(1.0)));
        }
        let t = self
            .rt
            .block_on(self.forge.auth().create_session(&user_id, opts))
            .map_err(pyerr)?;
        Ok(t.as_str().to_string())
    }

    fn validate_session(&self, token: String) -> PyResult<Option<String>> {
        Ok(self
            .rt
            .block_on(self.forge.auth().validate_session(&token))
            .map_err(pyerr)?
            .map(|s| s.user_id))
    }

    fn revoke_session(&self, token: String) -> PyResult<()> {
        self.rt
            .block_on(self.forge.auth().revoke_session(&token))
            .map_err(pyerr)
    }

    fn revoke_all_sessions(&self, user_id: String) -> PyResult<u64> {
        self.rt
            .block_on(self.forge.auth().revoke_all_sessions(&user_id))
            .map_err(pyerr)
    }

    /// Mint an `fk_` API key. Returns `(id, secret)`; the secret is shown once.
    fn create_api_key(&self, owner_id: String, label: String) -> PyResult<(String, String)> {
        let k = self
            .rt
            .block_on(self.forge.auth().create_api_key(&owner_id, &label))
            .map_err(pyerr)?;
        Ok((k.id, k.secret.as_str().to_string()))
    }

    fn verify_api_key(&self, key: String) -> PyResult<Option<String>> {
        Ok(self
            .rt
            .block_on(self.forge.auth().verify_api_key(&key))
            .map_err(pyerr)?
            .map(|i| i.owner_id))
    }

    // ---- schedule ------------------------------------------------------------

    /// Schedule a one-shot enqueue at `when_epoch_seconds`; returns the future JobId.
    fn schedule_at(&self, when_epoch_seconds: f64, queue: String, payload: String) -> PyResult<String> {
        let when = UNIX_EPOCH + Duration::from_secs_f64(when_epoch_seconds.max(0.0));
        let id = self
            .rt
            .block_on(self.forge.schedule().at(when, &queue, forge::Bytes::from(payload)))
            .map_err(pyerr)?;
        Ok(id.to_string())
    }

    fn schedule_cron(&self, name: String, expr: String, queue: String, payload: String) -> PyResult<()> {
        self.rt
            .block_on(self.forge.schedule().cron(&name, &expr, &queue, forge::Bytes::from(payload)))
            .map_err(pyerr)
    }

    fn run_scheduler_once(&self) -> PyResult<u64> {
        self.rt
            .block_on(self.forge.run_scheduler_once())
            .map_err(pyerr)
    }

    // ---- queue ---------------------------------------------------------------

    fn queue_enqueue(&self, queue: String, payload: String) -> PyResult<String> {
        let id = self
            .rt
            .block_on(
                self.forge
                    .queue()
                    .enqueue(&queue, forge::Bytes::from(payload), forge::EnqueueOpts::new()),
            )
            .map_err(pyerr)?;
        Ok(id.to_string())
    }

    /// Lease one job, long-polling up to `wait_seconds`. Returns
    /// `(id, payload, attempt)` or `None`. Settle it with `queue_ack`.
    fn queue_dequeue(
        &self,
        queue: String,
        visibility_seconds: f64,
        wait_seconds: f64,
    ) -> PyResult<Option<(String, String, u32)>> {
        let opts = forge::DequeueOpts::new()
            .with_visibility_timeout(Duration::from_secs_f64(visibility_seconds.max(0.001)))
            .with_wait(Duration::from_secs_f64(wait_seconds.max(0.0)));
        let job = self
            .rt
            .block_on(self.forge.queue().dequeue(&queue, opts))
            .map_err(pyerr)?;
        match job {
            Some(job) => {
                let tuple = (
                    job.id.to_string(),
                    String::from_utf8_lossy(&job.payload).into_owned(),
                    job.attempt,
                );
                if let Ok(mut leased) = self.leased.lock() {
                    // Evict leases that already lapsed (dequeued but never settled) so
                    // the map cannot grow without bound; the queue redelivers them.
                    let now = SystemTime::now();
                    leased.retain(|_, j| j.leased_until > now);
                    leased.insert(tuple.0.clone(), job);
                }
                Ok(Some(tuple))
            }
            None => Ok(None),
        }
    }

    fn queue_ack(&self, id: String) -> PyResult<()> {
        let job = self.leased.lock().ok().and_then(|mut m| m.remove(&id));
        if let Some(job) = job {
            self.rt.block_on(self.forge.queue().ack(&job)).map_err(pyerr)?;
        }
        Ok(())
    }

    /// Nack a leased job by id; optional `retry_seconds` delays redelivery.
    #[pyo3(signature = (id, retry_seconds=None))]
    fn queue_nack(&self, id: String, retry_seconds: Option<f64>) -> PyResult<()> {
        let job = self.leased.lock().ok().and_then(|mut m| m.remove(&id));
        if let Some(job) = job {
            let opts = match retry_seconds {
                Some(s) => forge::NackOpts::retry_in(Duration::from_secs_f64(s.max(0.0))),
                None => forge::NackOpts::default(),
            };
            self.rt
                .block_on(self.forge.queue().nack(&job, opts))
                .map_err(pyerr)?;
        }
        Ok(())
    }

    // ---- pubsub --------------------------------------------------------------

    /// Publish a payload to a realtime topic (fire-and-forget).
    fn pubsub_publish(&self, topic: String, payload: String) -> PyResult<()> {
        self.rt
            .block_on(self.forge.pubsub().publish(&topic, forge::Bytes::from(payload)))
            .map_err(pyerr)
    }

    /// The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. `LISTEN` on this with
    /// a native Postgres client to receive what `pubsub_publish(topic, …)` sends.
    fn pubsub_channel(&self, topic: String) -> String {
        forge::pubsub::channel_for(&topic)
    }
}

/// The `forge_py` extension module.
#[pymodule]
fn forge_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ForgeClient>()?;
    Ok(())
}

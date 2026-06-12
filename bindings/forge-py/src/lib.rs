//! Python bindings for Forge via pyo3 — natively asynchronous.
//!
//! Every method returns a Python awaitable driven on a shared Tokio runtime
//! (`pyo3-async-runtimes`), so an asyncio app `await`s the binding directly:
//!
//! ```python
//! forge = await ForgeClient.connect(url, signing_secret)
//! await forge.kv_set("k", "v")
//! async for payload in await forge.pubsub_subscribe("chat:1"):
//!     ...
//! ```
//!
//! There is no thread-pool wrapper to write: the binding never blocks the event
//! loop. Forge errors surface as typed exceptions (`forge_py.NotFound`,
//! `forge_py.Limit`, …, all subclasses of `forge_py.ForgeError`). Leased queue jobs
//! are held Rust-side and referenced by id, as in the Node binding.

// `Limit` is intentionally NOT imported by name: the `Limit` exception type below
// would collide with `forge::Limit`. It is referenced fully-qualified where used.
use forge::{EvalCtx, FailMode, FlagRule, Forge, ForgeConfig, PutOpts, SessionOpts, SetMode, SetOpts};
use futures_util::StreamExt;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyStopAsyncIteration};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_async_runtimes::tokio::future_into_py;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

create_exception!(forge_py, ForgeError, PyException, "Base class for all Forge errors.");
create_exception!(forge_py, NotFound, ForgeError);
create_exception!(forge_py, Invalid, ForgeError);
create_exception!(forge_py, Limit, ForgeError);
create_exception!(forge_py, Precondition, ForgeError);
create_exception!(forge_py, Unavailable, ForgeError);
create_exception!(forge_py, Config, ForgeError);
create_exception!(forge_py, Backend, ForgeError);

/// Map a `ForgeError` onto the matching typed Python exception.
fn pyerr(e: forge::ForgeError) -> PyErr {
    use forge::ForgeError as F;
    let msg = e.to_string();
    match e {
        F::NotFound => NotFound::new_err(msg),
        F::Invalid(_) => Invalid::new_err(msg),
        F::Limit(_) => Limit::new_err(msg),
        F::Precondition(_) => Precondition::new_err(msg),
        F::Unavailable(_) => Unavailable::new_err(msg),
        F::Config(_) => Config::new_err(msg),
        F::Backend { .. } => Backend::new_err(msg),
        _ => Backend::new_err(msg),
    }
}

/// A live subscription, usable as a Python async iterator
/// (`async for payload in subscription:`). Each item is `bytes`.
#[pyclass]
struct Subscription {
    inner: Arc<Mutex<forge::Subscription>>,
}

#[pymethods]
impl Subscription {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        future_into_py(py, async move {
            let mut sub = inner.lock().await;
            match sub.next().await {
                Some(Ok(b)) => Ok(Python::with_gil(|py| PyBytes::new(py, &b).unbind())),
                Some(Err(e)) => Err(pyerr(e)),
                None => Err(PyStopAsyncIteration::new_err("subscription ended")),
            }
        })
    }
}

/// A Forge client: one Postgres pool, every primitive, driven on a shared async
/// runtime. Construct with `await ForgeClient.connect(url)`.
#[pyclass]
struct ForgeClient {
    forge: Forge,
    leased: Arc<Mutex<HashMap<String, forge::Job>>>,
}

#[pymethods]
impl ForgeClient {
    /// Connect, run migrations, and ping — mirrors `Forge::init`. Pass
    /// `signing_secret` to enable presigned blob URLs. `await` the result.
    #[staticmethod]
    #[pyo3(signature = (postgres_url, signing_secret=None))]
    fn connect<'py>(
        py: Python<'py>,
        postgres_url: String,
        signing_secret: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        future_into_py(py, async move {
            let mut cfg = ForgeConfig::new(postgres_url);
            if let Some(secret) = signing_secret {
                cfg = cfg.with_blob_signing_secret(secret);
            }
            let forge = Forge::init(cfg).await.map_err(pyerr)?;
            Ok(ForgeClient {
                forge,
                leased: Arc::new(Mutex::new(HashMap::new())),
            })
        })
    }

    #[pyo3(signature = (key, value, ttl_seconds=None, if_not_exists=None))]
    fn kv_set<'py>(
        &self,
        py: Python<'py>,
        key: String,
        value: String,
        ttl_seconds: Option<f64>,
        if_not_exists: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = SetOpts::new();
            if let Some(t) = ttl_seconds {
                if t > 0.0 {
                    opts = opts.with_ttl(Duration::from_secs_f64(t));
                }
            }
            if if_not_exists == Some(true) {
                opts = opts.with_mode(SetMode::IfNotExists);
            }
            // Returns whether the write happened (false when `if_not_exists` and the key exists).
            forge
                .kv()
                .set(&key, forge::Bytes::from(value), opts)
                .await
                .map_err(pyerr)
        })
    }

    fn kv_get<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let v = forge.kv().get(&key).await.map_err(pyerr)?;
            Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
        })
    }

    /// `MGET keys` → a list with one slot per input key (the value as a string, or
    /// `None` if missing/expired), in input order. One round-trip.
    fn kv_mget<'py>(&self, py: Python<'py>, keys: Vec<String>) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            let vals = forge.kv().mget(&refs).await.map_err(pyerr)?;
            Ok(vals
                .into_iter()
                .map(|o| o.map(|b| String::from_utf8_lossy(&b).into_owned()))
                .collect::<Vec<Option<String>>>())
        })
    }

    fn kv_incr<'py>(&self, py: Python<'py>, key: String, by: i64) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.kv().incr(&key, by).await.map_err(pyerr) })
    }

    fn kv_delete<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.kv().delete(&key).await.map_err(pyerr) })
    }

    fn kv_exists<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.kv().exists(&key).await.map_err(pyerr) })
    }

    /// `SCAN prefix*` (first page) → up to `limit` matching keys.
    fn kv_scan<'py>(
        &self,
        py: Python<'py>,
        prefix: String,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let (keys, _next) = forge.kv().scan(&prefix, None, limit).await.map_err(pyerr)?;
            Ok(keys)
        })
    }

    fn config_set<'py>(
        &self,
        py: Python<'py>,
        key: String,
        value: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.config().set_raw(&key, &value).await.map_err(pyerr)
        })
    }

    fn config_get<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.config().get_raw(&key).await.map_err(pyerr) })
    }

    fn set_flag_percent<'py>(
        &self,
        py: Python<'py>,
        key: String,
        percent: u8,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .config()
                .set_flag(&key, FlagRule::Percent(percent))
                .await
                .map_err(pyerr)
        })
    }

    #[pyo3(signature = (key, default_value, targeting_key=None))]
    fn flag<'py>(
        &self,
        py: Python<'py>,
        key: String,
        default_value: bool,
        targeting_key: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let ctx = match targeting_key {
                Some(k) => EvalCtx::user(k),
                None => EvalCtx::new(),
            };
            Ok::<bool, PyErr>(forge.config().flag(&key, default_value, &ctx).await)
        })
    }

    /// Atomic check-and-consume. `fail_open` overrides the instance default for what
    /// happens on a backend error: `None` = default, `True` = allow, `False` = deny.
    /// Returns `(allowed, remaining, retry_after_seconds)`.
    #[pyo3(signature = (bucket, key, max, per_seconds, fail_open=None))]
    fn rate_limit_check<'py>(
        &self,
        py: Python<'py>,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
        fail_open: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let limit =
                forge::Limit::per_duration(max, Duration::from_secs_f64(per_seconds.max(1.0)));
            let mode = match fail_open {
                None => FailMode::Default,
                Some(true) => FailMode::Open,
                Some(false) => FailMode::Closed,
            };
            let d = forge
                .ratelimit()
                .check_with(&bucket, &key, limit, mode)
                .await
                .map_err(pyerr)?;
            Ok((d.allowed, d.remaining, d.retry_after.map(|x| x.as_secs_f64())))
        })
    }

    /// Store an object from raw `bytes` (binary-safe; no base64 needed).
    #[pyo3(signature = (key, data, content_type=None))]
    fn blob_put<'py>(
        &self,
        py: Python<'py>,
        key: String,
        data: Bound<'py, PyBytes>,
        content_type: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let bytes = forge::Bytes::from(data.as_bytes().to_vec());
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = PutOpts::new();
            if let Some(ct) = content_type {
                opts = opts.with_content_type(ct);
            }
            forge.blob().put(&key, bytes, opts).await.map_err(pyerr)
        })
    }

    /// Fetch an object as raw `bytes`, or `None`.
    fn blob_get<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let v = forge.blob().get(&key).await.map_err(pyerr)?;
            Ok(v.map(|b| Python::with_gil(|py| PyBytes::new(py, &b).unbind())))
        })
    }

    fn blob_presign_download<'py>(
        &self,
        py: Python<'py>,
        key: String,
        expires_seconds: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .presign_download(&key, Duration::from_secs_f64(expires_seconds.max(1.0)))
                .await
                .map_err(pyerr)
        })
    }

    fn blob_presign_upload<'py>(
        &self,
        py: Python<'py>,
        key: String,
        expires_seconds: f64,
        max_bytes: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .presign_upload(
                    &key,
                    Duration::from_secs_f64(expires_seconds.max(1.0)),
                    max_bytes,
                )
                .await
                .map_err(pyerr)
        })
    }

    fn blob_content_type<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .blob()
                .head(&key)
                .await
                .map_err(pyerr)?
                .map(|i| i.content_type))
        })
    }

    fn blob_delete<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.blob().delete(&key).await.map_err(pyerr) })
    }

    /// Verify a presigned URL's query params against the signing secret. Returns
    /// `True` only if the signature matches and the URL has not expired.
    fn blob_verify_presign<'py>(
        &self,
        py: Python<'py>,
        method: String,
        key: String,
        expires_epoch: i64,
        max_bytes: u64,
        sig: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .verify_presigned(&method, &key, expires_epoch, max_bytes, &sig)
                .await
                .map_err(pyerr)
        })
    }

    fn hash_password<'py>(&self, py: Python<'py>, plain: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let h = forge.auth().hash_password(&plain).await.map_err(pyerr)?;
            Ok(h.as_str().to_string())
        })
    }

    fn verify_password<'py>(
        &self,
        py: Python<'py>,
        plain: String,
        hash: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .auth()
                .verify_password(&plain, &forge::PhcString::new(hash))
                .await
                .map_err(pyerr)
        })
    }

    /// Whether a stored PHC `hash` should be re-hashed (its argon2id params are below
    /// the current Forge baseline). Call after a successful `verify_password`; if
    /// `True`, re-hash the plaintext and persist it — transparent upgrade, no reset.
    fn needs_rehash(&self, hash: String) -> bool {
        self.forge.auth().needs_rehash(&forge::PhcString::new(hash))
    }

    #[pyo3(signature = (user_id, idle_seconds=None, absolute_seconds=None))]
    fn create_session<'py>(
        &self,
        py: Python<'py>,
        user_id: String,
        idle_seconds: Option<f64>,
        absolute_seconds: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = SessionOpts::new();
            if let Some(s) = idle_seconds {
                opts = opts.with_idle_timeout(Duration::from_secs_f64(s.max(1.0)));
            }
            if let Some(s) = absolute_seconds {
                opts = opts.with_absolute_timeout(Duration::from_secs_f64(s.max(1.0)));
            }
            let t = forge
                .auth()
                .create_session(&user_id, opts)
                .await
                .map_err(pyerr)?;
            Ok(t.as_str().to_string())
        })
    }

    fn validate_session<'py>(
        &self,
        py: Python<'py>,
        token: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .auth()
                .validate_session(&token)
                .await
                .map_err(pyerr)?
                .map(|s| s.user_id))
        })
    }

    fn revoke_session<'py>(&self, py: Python<'py>, token: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.auth().revoke_session(&token).await.map_err(pyerr)
        })
    }

    fn revoke_all_sessions<'py>(
        &self,
        py: Python<'py>,
        user_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.auth().revoke_all_sessions(&user_id).await.map_err(pyerr)
        })
    }

    /// Mint an `fk_` API key. Returns `(id, secret)`; the secret is shown once.
    fn create_api_key<'py>(
        &self,
        py: Python<'py>,
        owner_id: String,
        label: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let k = forge
                .auth()
                .create_api_key(&owner_id, &label)
                .await
                .map_err(pyerr)?;
            Ok((k.id, k.secret.as_str().to_string()))
        })
    }

    fn verify_api_key<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .auth()
                .verify_api_key(&key)
                .await
                .map_err(pyerr)?
                .map(|i| i.owner_id))
        })
    }

    /// Schedule a one-shot enqueue at `when_epoch_seconds`; returns the future JobId.
    fn schedule_at<'py>(
        &self,
        py: Python<'py>,
        when_epoch_seconds: f64,
        queue: String,
        payload: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let when = UNIX_EPOCH + Duration::from_secs_f64(when_epoch_seconds.max(0.0));
            let id = forge
                .schedule()
                .at(when, &queue, forge::Bytes::from(payload))
                .await
                .map_err(pyerr)?;
            Ok(id.to_string())
        })
    }

    fn schedule_cron<'py>(
        &self,
        py: Python<'py>,
        name: String,
        expr: String,
        queue: String,
        payload: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .schedule()
                .cron(&name, &expr, &queue, forge::Bytes::from(payload))
                .await
                .map_err(pyerr)
        })
    }

    fn run_scheduler_once<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.run_scheduler_once().await.map_err(pyerr)
        })
    }

    /// Run periodic housekeeping once: sweep expired kv keys, settled/dead queue
    /// jobs, stale ratelimit buckets, and expired sessions. Call on an interval
    /// alongside `run_scheduler_once` (mirrors the Rust `Forge::maintain` loop).
    fn maintain<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.maintain().await.map_err(pyerr) })
    }

    /// Approximate `(visible, in_flight, delayed)` counts for a queue (SQS
    /// `GetQueueAttributes`). Pass `"<queue>.dlq"` to gauge a dead-letter backlog
    /// without leasing its jobs. For a DLQ every job is `visible`, so `visible` is its size.
    fn queue_depth<'py>(&self, py: Python<'py>, queue: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let d = forge.queue().depth(&queue).await.map_err(pyerr)?;
            Ok((d.visible, d.in_flight, d.delayed))
        })
    }

    #[pyo3(signature = (queue, payload, max_attempts=None, dedup_id=None))]
    fn queue_enqueue<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
        dedup_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = forge::EnqueueOpts::new();
            if let Some(m) = max_attempts {
                opts = opts.with_max_attempts(m);
            }
            if let Some(d) = dedup_id {
                opts = opts.with_dedup_id(d);
            }
            let id = forge
                .queue()
                .enqueue(&queue, forge::Bytes::from(payload), opts)
                .await
                .map_err(pyerr)?;
            Ok(id.to_string())
        })
    }

    /// Lease one job, long-polling up to `wait_seconds`. Returns
    /// `(id, payload, attempt)` or `None`. Settle it with `queue_ack`.
    fn queue_dequeue<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        visibility_seconds: f64,
        wait_seconds: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let opts = forge::DequeueOpts::new()
                .with_visibility_timeout(Duration::from_secs_f64(visibility_seconds.max(0.001)))
                .with_wait(Duration::from_secs_f64(wait_seconds.max(0.0)));
            let job = forge.queue().dequeue(&queue, opts).await.map_err(pyerr)?;
            match job {
                Some(job) => {
                    let tuple = (
                        job.id.to_string(),
                        String::from_utf8_lossy(&job.payload).into_owned(),
                        job.attempt,
                    );
                    let mut map = leased.lock().await;
                    // Evict leases that already lapsed so the map cannot grow unbounded;
                    // the queue redelivers them regardless.
                    let now = SystemTime::now();
                    map.retain(|_, j| j.leased_until > now);
                    map.insert(tuple.0.clone(), job);
                    Ok(Some(tuple))
                }
                None => Ok(None),
            }
        })
    }

    fn queue_ack<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.remove(&id);
            if let Some(job) = job {
                forge.queue().ack(&job).await.map_err(pyerr)?;
            }
            Ok(())
        })
    }

    #[pyo3(signature = (id, retry_seconds=None))]
    fn queue_nack<'py>(
        &self,
        py: Python<'py>,
        id: String,
        retry_seconds: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.remove(&id);
            if let Some(job) = job {
                let opts = match retry_seconds {
                    Some(s) => forge::NackOpts::retry_in(Duration::from_secs_f64(s.max(0.0))),
                    None => forge::NackOpts::default(),
                };
                forge.queue().nack(&job, opts).await.map_err(pyerr)?;
            }
            Ok(())
        })
    }

    /// Extend the lease on a job leased by this client (SQS ChangeMessageVisibility /
    /// beanstalkd touch) by one visibility timeout, so a handler that may outlive its
    /// visibility window is not redelivered mid-flight. No-op if not leased here.
    fn queue_heartbeat<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.get(&id).cloned();
            if let Some(job) = job {
                forge.queue().heartbeat(&job).await.map_err(pyerr)?;
            }
            Ok(())
        })
    }

    /// Publish a payload to a realtime topic (fire-and-forget).
    fn pubsub_publish<'py>(
        &self,
        py: Python<'py>,
        topic: String,
        payload: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .pubsub()
                .publish(&topic, forge::Bytes::from(payload))
                .await
                .map_err(pyerr)
        })
    }

    /// Subscribe to a realtime topic. `await`s registration, then yields a
    /// [`Subscription`] usable as `async for payload in subscription:`.
    fn pubsub_subscribe<'py>(&self, py: Python<'py>, topic: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let sub = forge.pubsub().subscribe(&topic).await.map_err(pyerr)?;
            Ok(Subscription {
                inner: Arc::new(Mutex::new(sub)),
            })
        })
    }

    /// The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. Pure and cheap; kept
    /// for parity with the Node binding. Prefer `pubsub_subscribe`.
    fn pubsub_channel(&self, topic: String) -> String {
        forge::pubsub::channel_for(&topic)
    }
}

#[pymodule]
fn forge_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Own the Tokio runtime that drives every awaitable this module returns.
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    pyo3_async_runtimes::tokio::init(builder);

    m.add_class::<ForgeClient>()?;
    m.add_class::<Subscription>()?;

    let py = m.py();
    m.add("ForgeError", py.get_type::<ForgeError>())?;
    m.add("NotFound", py.get_type::<NotFound>())?;
    m.add("Invalid", py.get_type::<Invalid>())?;
    m.add("Limit", py.get_type::<Limit>())?;
    m.add("Precondition", py.get_type::<Precondition>())?;
    m.add("Unavailable", py.get_type::<Unavailable>())?;
    m.add("Config", py.get_type::<Config>())?;
    m.add("Backend", py.get_type::<Backend>())?;
    Ok(())
}

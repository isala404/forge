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
use forge::{
    EvalCtx, FailMode, FlagRule, Forge, ForgeConfig, PutOpts, SessionOpts, SetMode, SetOpts,
};
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

create_exception!(
    forge_py,
    ForgeError,
    PyException,
    "Base class for all Forge errors."
);
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

/// Convert an `f64` seconds value into a `Duration`, raising `Invalid` on a
/// negative or non-finite input. Zero passes straight through so the core applies
/// its own validation: bindings convert and pass through, they never clamp or
/// silently coerce a caller's out-of-range value (P0-5).
fn secs(field: &str, value: f64) -> PyResult<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| {
        pyerr(forge::ForgeError::invalid(format!(
            "{field} must be a non-negative number of seconds"
        )))
    })
}

/// Epoch milliseconds for a `SystemTime` (saturating).
fn epoch_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Object metadata (S3 `HeadObject`). `last_modified_ms` is epoch milliseconds.
#[pyclass(get_all)]
struct BlobInfo {
    key: String,
    size: u64,
    content_type: String,
    etag: String,
    last_modified_ms: f64,
    metadata: HashMap<String, String>,
}

/// A registered schedule. `kind` is `"cron"` or `"at"`; `cron_expr` is set only for
/// crons. Times are epoch milliseconds.
#[pyclass(get_all)]
struct ScheduleInfo {
    name: String,
    kind: String,
    cron_expr: Option<String>,
    queue: String,
    next_run_ms: f64,
    last_run_ms: Option<f64>,
}

/// A validated session's metadata. Times are epoch milliseconds.
#[pyclass(get_all)]
struct SessionInfo {
    user_id: String,
    created_at_ms: f64,
    expires_at_ms: f64,
}

/// Non-secret API-key metadata.
#[pyclass(get_all)]
struct ApiKeyInfo {
    id: String,
    owner_id: String,
    label: String,
}

/// One line of `backend_report`: which provider powers a primitive.
#[pyclass(get_all)]
struct BackendInfo {
    primitive: String,
    provider: String,
    durable: bool,
    caveats: String,
}

/// A freshly minted API key. `secret` is shown exactly once.
#[pyclass(get_all)]
struct ApiKey {
    id: String,
    secret: String,
    label: String,
    created_at_ms: f64,
}

/// A leased job. Settle it with ack/nack/heartbeat using the opaque,
/// delivery-unique `receipt` (NOT `id`, which is stable across redeliveries and is
/// the natural idempotency key).
#[pyclass(get_all)]
struct Job {
    id: String,
    receipt: String,
    payload: String,
    attempt: u32,
    max_attempts: u32,
    leased_until_ms: f64,
    queue: String,
}

/// A full rate-limit decision (all IETF RateLimit fields, unlike the legacy
/// `rate_limit_check` tuple which omits `limit`/`reset_after_seconds`).
#[pyclass(get_all)]
struct Decision {
    allowed: bool,
    limit: u32,
    remaining: u32,
    reset_after_seconds: f64,
    retry_after_seconds: Option<f64>,
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
    /// Leased-but-not-settled jobs, keyed by a delivery-unique opaque receipt (not
    /// the job id), so a redelivered job gets a fresh entry rather than overwriting
    /// the in-flight one. Evicted on settle and, as a leak backstop, once the
    /// original lease has been expired for over 24h.
    leased: Arc<Mutex<HashMap<String, forge::Job>>>,
    /// Monotonic counter making each dequeue's receipt unique.
    seq: Arc<std::sync::atomic::AtomicU64>,
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
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
                opts = opts.with_ttl(secs("ttl_seconds", t)?);
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

    /// `SET key` with raw value bytes (lossless, unlike `kv_set`). Returns whether
    /// the write happened.
    #[pyo3(signature = (key, value, ttl_seconds=None, if_not_exists=None))]
    fn kv_set_bytes<'py>(
        &self,
        py: Python<'py>,
        key: String,
        value: Vec<u8>,
        ttl_seconds: Option<f64>,
        if_not_exists: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = SetOpts::new();
            if let Some(t) = ttl_seconds {
                opts = opts.with_ttl(secs("ttl_seconds", t)?);
            }
            if if_not_exists == Some(true) {
                opts = opts.with_mode(SetMode::IfNotExists);
            }
            forge
                .kv()
                .set(&key, forge::Bytes::from(value), opts)
                .await
                .map_err(pyerr)
        })
    }

    /// `GET key` → the raw value bytes, or `None`. Lossless, unlike `kv_get` (P0-4).
    fn kv_get_bytes<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let v = forge.kv().get(&key).await.map_err(pyerr)?;
            Ok(v.map(|b| Python::with_gil(|py| PyBytes::new(py, &b).unbind())))
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
        future_into_py(
            py,
            async move { forge.kv().incr(&key, by).await.map_err(pyerr) },
        )
    }

    fn kv_delete<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(
            py,
            async move { forge.kv().delete(&key).await.map_err(pyerr) },
        )
    }

    fn kv_exists<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(
            py,
            async move { forge.kv().exists(&key).await.map_err(pyerr) },
        )
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
        future_into_py(py, async move {
            forge.config().get_raw(&key).await.map_err(pyerr)
        })
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
                forge::Limit::per_duration(max, secs("per_seconds", per_seconds)?);
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
            Ok((
                d.allowed,
                d.remaining,
                d.retry_after.map(|x| x.as_secs_f64()),
            ))
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
                .presign_download(&key, secs("expires_seconds", expires_seconds)?)
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
                    secs("expires_seconds", expires_seconds)?,
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
        future_into_py(
            py,
            async move { forge.blob().delete(&key).await.map_err(pyerr) },
        )
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
                opts = opts.with_idle_timeout(secs("idle_seconds", s)?);
            }
            if let Some(s) = absolute_seconds {
                opts = opts.with_absolute_timeout(secs("absolute_seconds", s)?);
            }
            let t = forge
                .auth()
                .create_session(&user_id, opts)
                .await
                .map_err(pyerr)?;
            Ok(t.as_str().to_string())
        })
    }

    fn validate_session<'py>(&self, py: Python<'py>, token: String) -> PyResult<Bound<'py, PyAny>> {
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
            forge
                .auth()
                .revoke_all_sessions(&user_id)
                .await
                .map_err(pyerr)
        })
    }

    /// Mint an `fk_` API key. Returns an `ApiKey` (id, secret, label,
    /// created_at_ms); the secret is shown once.
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
            Ok(ApiKey {
                id: k.id,
                secret: k.secret.as_str().to_string(),
                label: k.label,
                created_at_ms: epoch_ms(k.created_at),
            })
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

    /// Schedule a one-shot enqueue at `when_epoch_ms`; returns the future JobId.
    fn schedule_at<'py>(
        &self,
        py: Python<'py>,
        when_epoch_ms: f64,
        queue: String,
        payload: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let when = UNIX_EPOCH + Duration::from_millis(when_epoch_ms.max(0.0) as u64);
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

    /// Lease one job, long-polling up to `wait_seconds`. Returns a `Job` (settle it
    /// with `queue_ack`/`queue_nack`/`queue_heartbeat` by `job.receipt`) or `None`.
    fn queue_dequeue<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        visibility_seconds: f64,
        wait_seconds: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        let seq = self.seq.clone();
        future_into_py(py, async move {
            let opts = forge::DequeueOpts::new()
                .with_visibility_timeout(secs("visibility_seconds", visibility_seconds)?)
                .with_wait(secs("wait_seconds", wait_seconds)?);
            let job = forge.queue().dequeue(&queue, opts).await.map_err(pyerr)?;
            match job {
                Some(job) => {
                    let receipt = format!(
                        "{}:{}",
                        job.id,
                        seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    );
                    let out = Job {
                        id: job.id.to_string(),
                        receipt: receipt.clone(),
                        payload: String::from_utf8_lossy(&job.payload).into_owned(),
                        attempt: job.attempt,
                        max_attempts: job.max_attempts,
                        leased_until_ms: epoch_ms(job.leased_until),
                        queue: job.queue.clone(),
                    };
                    let mut map = leased.lock().await;
                    // Leak backstop: drop entries whose lease lapsed over 24h ago. The
                    // grace far exceeds any heartbeat window, so a heartbeated job is
                    // never evicted mid-flight.
                    let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
                    map.retain(|_, j| j.leased_until > cutoff);
                    map.insert(receipt, job);
                    Ok(Some(out))
                }
                None => Ok(None),
            }
        })
    }

    /// Ack a leased job by its `receipt` (idempotent: a no-op if already settled).
    fn queue_ack<'py>(&self, py: Python<'py>, receipt: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.remove(&receipt);
            if let Some(job) = job {
                forge.queue().ack(&job).await.map_err(pyerr)?;
            }
            Ok(())
        })
    }

    /// Nack a leased job by its `receipt`. Raises `Precondition` if the receipt is
    /// unknown (the lease was lost — stop working on this job).
    #[pyo3(signature = (receipt, retry_seconds=None))]
    fn queue_nack<'py>(
        &self,
        py: Python<'py>,
        receipt: String,
        retry_seconds: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.remove(&receipt);
            let Some(job) = job else {
                return Err(pyerr(forge::ForgeError::precondition(
                    "unknown receipt: the lease was lost",
                )));
            };
            let opts = match retry_seconds {
                Some(s) => forge::NackOpts::retry_in(secs("retry_seconds", s)?),
                None => forge::NackOpts::default(),
            };
            forge.queue().nack(&job, opts).await.map_err(pyerr)
        })
    }

    /// Extend the lease on a job leased by this client by one visibility timeout, so a
    /// handler that may outlive its visibility window is not redelivered mid-flight.
    /// Raises `Precondition` if the receipt is unknown (the lease was lost).
    fn queue_heartbeat<'py>(
        &self,
        py: Python<'py>,
        receipt: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.get(&receipt).cloned();
            let Some(job) = job else {
                return Err(pyerr(forge::ForgeError::precondition(
                    "unknown receipt: the lease was lost",
                )));
            };
            forge.queue().heartbeat(&job).await.map_err(pyerr)
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

    /// Connect with the full per-deployment option surface (namespace, pool size,
    /// migration toggle, filesystem blob backend, …). `await` the result.
    #[staticmethod]
    #[pyo3(signature = (postgres_url, signing_secret=None, kv_namespace=None, max_connections=None, run_migrations=None, blob_base_url=None, filesystem_blob_root=None))]
    #[allow(clippy::too_many_arguments)]
    fn connect_with<'py>(
        py: Python<'py>,
        postgres_url: String,
        signing_secret: Option<String>,
        kv_namespace: Option<String>,
        max_connections: Option<u32>,
        run_migrations: Option<bool>,
        blob_base_url: Option<String>,
        filesystem_blob_root: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        future_into_py(py, async move {
            let mut cfg = ForgeConfig::new(postgres_url);
            if let Some(s) = signing_secret {
                cfg = cfg.with_blob_signing_secret(s);
            }
            if let Some(ns) = kv_namespace {
                cfg = cfg.with_kv_namespace(ns);
            }
            if let Some(n) = max_connections {
                cfg = cfg.with_max_connections(n);
            }
            if run_migrations == Some(false) {
                cfg = cfg.without_migrations();
            }
            if let Some(base) = blob_base_url {
                cfg = cfg.with_blob_base_url(base);
            }
            if let Some(root) = filesystem_blob_root {
                cfg = cfg.with_filesystem_blob(root);
            }
            let forge = Forge::init(cfg).await.map_err(pyerr)?;
            Ok(ForgeClient {
                forge,
                leased: Arc::new(Mutex::new(HashMap::new())),
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            })
        })
    }

    /// A backend report: which provider powers each primitive (for health pages/logs).
    fn backend_report(&self) -> Vec<BackendInfo> {
        self.forge
            .backend_report()
            .backends
            .into_iter()
            .map(|b| BackendInfo {
                primitive: b.primitive.as_str().to_string(),
                provider: b.provider.to_string(),
                durable: b.durable,
                caveats: b.caveats.to_string(),
            })
            .collect()
    }

    // --- kv parity -----------------------------------------------------------------

    /// `EXPIRE key ttl_seconds`. Sets/replaces the TTL on a live key; `False` if absent.
    fn kv_expire<'py>(
        &self,
        py: Python<'py>,
        key: String,
        ttl_seconds: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .kv()
                .expire(&key, secs("ttl_seconds", ttl_seconds)?)
                .await
                .map_err(pyerr)
        })
    }

    /// Atomic compare-and-swap (string values). Writes `new_value` iff the current value
    /// equals `old` (`old=None` means "expected absent/expired"). Returns success.
    #[pyo3(signature = (key, old, new_value))]
    fn kv_compare_and_swap<'py>(
        &self,
        py: Python<'py>,
        key: String,
        old: Option<String>,
        new_value: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .kv()
                .compare_and_swap(
                    &key,
                    old.map(forge::Bytes::from),
                    forge::Bytes::from(new_value),
                )
                .await
                .map_err(pyerr)
        })
    }

    /// `SCAN prefix*` with cursor pagination. Returns `(keys, cursor)`; pass `cursor`
    /// back for the next page (`None` when iteration is done).
    #[pyo3(signature = (prefix, cursor=None, limit=100))]
    fn kv_scan_page<'py>(
        &self,
        py: Python<'py>,
        prefix: String,
        cursor: Option<String>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let cur = cursor.map(forge::Cursor::from_token);
            let (keys, next) = forge.kv().scan(&prefix, cur, limit).await.map_err(pyerr)?;
            Ok((keys, next.map(|c| c.token().to_string())))
        })
    }

    // --- blob parity ---------------------------------------------------------------

    /// `HeadObject`: full metadata, or `None` if the object does not exist.
    fn blob_head<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .blob()
                .head(&key)
                .await
                .map_err(pyerr)?
                .map(|i| BlobInfo {
                    key: i.key,
                    size: i.size,
                    content_type: i.content_type,
                    etag: i.etag,
                    last_modified_ms: epoch_ms(i.last_modified),
                    metadata: i.metadata.into_iter().collect(),
                }))
        })
    }

    /// `ListObjectsV2`: up to `limit` objects under `prefix`, lexicographic, with cursor
    /// pagination. Returns `(items, cursor)`.
    #[pyo3(signature = (prefix, cursor=None, limit=100))]
    fn blob_list<'py>(
        &self,
        py: Python<'py>,
        prefix: String,
        cursor: Option<String>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let cur = cursor.map(forge::Cursor::from_token);
            let page = forge
                .blob()
                .list(&prefix, cur, limit)
                .await
                .map_err(pyerr)?;
            let items: Vec<BlobInfo> = page
                .items
                .into_iter()
                .map(|i| BlobInfo {
                    key: i.key,
                    size: i.size,
                    content_type: i.content_type,
                    etag: i.etag,
                    last_modified_ms: epoch_ms(i.last_modified),
                    metadata: i.metadata.into_iter().collect(),
                })
                .collect();
            Ok((items, page.next.map(|c| c.token().to_string())))
        })
    }

    /// Store an object from raw `bytes` with optional content type and user metadata.
    #[pyo3(signature = (key, data, content_type=None, metadata=None))]
    fn blob_put_object<'py>(
        &self,
        py: Python<'py>,
        key: String,
        data: Bound<'py, PyBytes>,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let bytes = forge::Bytes::from(data.as_bytes().to_vec());
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = PutOpts::new();
            if let Some(ct) = content_type {
                opts = opts.with_content_type(ct);
            }
            if let Some(meta) = metadata {
                for (k, v) in meta {
                    opts = opts.with_metadata(k, v);
                }
            }
            forge.blob().put(&key, bytes, opts).await.map_err(pyerr)
        })
    }

    // --- ratelimit parity (full Decision) ------------------------------------------

    /// Atomic check-and-consume returning the FULL [`Decision`] (all IETF RateLimit
    /// fields). Prefer this over `rate_limit_check`, whose tuple omits `limit` and
    /// `reset_after_seconds`.
    #[pyo3(signature = (bucket, key, max, per_seconds, fail_open=None))]
    fn rate_limit<'py>(
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
                forge::Limit::per_duration(max, secs("per_seconds", per_seconds)?);
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
            Ok(Decision {
                allowed: d.allowed,
                limit: d.limit,
                remaining: d.remaining,
                reset_after_seconds: d.reset_after.as_secs_f64(),
                retry_after_seconds: d.retry_after.map(|x| x.as_secs_f64()),
            })
        })
    }

    // --- schedule parity -----------------------------------------------------------

    /// Cancel a schedule by name. `True` if one was removed, `False` if none existed.
    fn schedule_cancel<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.schedule().cancel(&name).await.map_err(pyerr)
        })
    }

    /// List every registered schedule (crons and pending one-shots).
    fn schedule_list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let items = forge.schedule().list().await.map_err(pyerr)?;
            Ok(items
                .into_iter()
                .map(|s| {
                    let (kind, cron_expr) = match s.kind {
                        forge::ScheduleKind::Cron(e) => ("cron".to_string(), Some(e)),
                        _ => ("at".to_string(), None),
                    };
                    ScheduleInfo {
                        name: s.name,
                        kind,
                        cron_expr,
                        queue: s.queue,
                        next_run_ms: epoch_ms(s.next_run),
                        last_run_ms: s.last_run.map(epoch_ms),
                    }
                })
                .collect::<Vec<_>>())
        })
    }

    // --- config flag parity --------------------------------------------------------

    /// Set a flag to always-on.
    fn set_flag_on<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .config()
                .set_flag(&key, FlagRule::On)
                .await
                .map_err(pyerr)
        })
    }

    /// Set a flag to always-off.
    fn set_flag_off<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .config()
                .set_flag(&key, FlagRule::Off)
                .await
                .map_err(pyerr)
        })
    }

    /// Set a flag to an allow-list of targeting keys.
    fn set_flag_allow_list<'py>(
        &self,
        py: Python<'py>,
        key: String,
        entries: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .config()
                .set_flag(&key, FlagRule::AllowList(entries))
                .await
                .map_err(pyerr)
        })
    }

    // --- auth parity ---------------------------------------------------------------

    /// Validate a session token; returns full [`SessionInfo`] (user id + times) or
    /// `None`. Use `validate_session` when only the user id is needed.
    fn validate_session_info<'py>(
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
                .map(|s| SessionInfo {
                    user_id: s.user_id,
                    created_at_ms: epoch_ms(s.created_at),
                    expires_at_ms: epoch_ms(s.expires_at),
                }))
        })
    }

    /// Verify an API key; returns full non-secret [`ApiKeyInfo`] or `None`. Use
    /// `verify_api_key` when only the owner id is needed.
    fn verify_api_key_info<'py>(
        &self,
        py: Python<'py>,
        key: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .auth()
                .verify_api_key(&key)
                .await
                .map_err(pyerr)?
                .map(|i| ApiKeyInfo {
                    id: i.id,
                    owner_id: i.owner_id,
                    label: i.label,
                }))
        })
    }

    /// Revoke an API key by its (non-secret) id. `True` if one was removed.
    fn revoke_api_key<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.auth().revoke_api_key(&id).await.map_err(pyerr)
        })
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
    m.add_class::<BlobInfo>()?;
    m.add_class::<ScheduleInfo>()?;
    m.add_class::<SessionInfo>()?;
    m.add_class::<ApiKeyInfo>()?;
    m.add_class::<BackendInfo>()?;
    m.add_class::<ApiKey>()?;
    m.add_class::<Job>()?;
    m.add_class::<Decision>()?;

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

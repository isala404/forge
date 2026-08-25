use forge::{
    Algo, ApiKeyOpts, EvalCtx, FailMode, FlagRule, Forge, PutOpts, SessionOpts, SetMode, SetOpts,
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
    forgelib,
    ForgeError,
    PyException,
    "Base class for all Forge errors. Every raised instance carries a `retryable: bool` attribute."
);
// Leaf names are the canonical error code + "Error" (LimitError <-> "Limit"), so the
// class taxonomy maps mechanically onto forge_error_code() and never shadows common
// names like `Config`/`Limit`/`Backend` under `from forgelib import *`.
create_exception!(forgelib, NotFoundError, ForgeError);
create_exception!(forgelib, InvalidError, ForgeError);
create_exception!(forgelib, LimitError, ForgeError);
create_exception!(forgelib, PreconditionError, ForgeError);
create_exception!(forgelib, UnavailableError, ForgeError);
create_exception!(forgelib, ConfigError, ForgeError);
create_exception!(forgelib, NotConfiguredError, ForgeError);
create_exception!(forgelib, BackendError, ForgeError);

/// Map a `ForgeError` onto the matching typed Python exception, carrying the
/// core-side retryable flag across as a `retryable` attribute (the class alone
/// cannot express it: `BackendError` is only sometimes retryable).
fn pyerr(e: forge::ForgeError) -> PyErr {
    let msg = e.safe_message();
    let code = e.code();
    let retryable = e.is_retryable();
    let operation = e.operation().unwrap_or("unknown").to_string();
    let backend = e.backend_id().map(str::to_string);
    let err = match code {
        "NOT_FOUND" => NotFoundError::new_err(msg.clone()),
        "INVALID" => InvalidError::new_err(msg.clone()),
        "LIMIT" => LimitError::new_err(msg.clone()),
        "PRECONDITION" => PreconditionError::new_err(msg.clone()),
        "UNAVAILABLE" => UnavailableError::new_err(msg.clone()),
        "CONFIG" => ConfigError::new_err(msg.clone()),
        "NOT_CONFIGURED" => NotConfiguredError::new_err(msg.clone()),
        _ => BackendError::new_err(msg.clone()),
    };
    Python::attach(|py| {
        // Best-effort: a failed setattr must not replace the real error being raised.
        let _ = err.value(py).setattr("retryable", retryable);
        let _ = err.value(py).setattr("code", code);
        let _ = err.value(py).setattr("operation", operation);
        let _ = err.value(py).setattr("backend", backend);
        let _ = err.value(py).setattr("safe_message", msg);
    });
    err
}

/// Convert `f64` seconds to a `Duration`, raising `Invalid` on negative or non-finite
/// input. Zero passes through so the core runs its own validation; the binding never
/// clamps or coerces an out-of-range value.
fn secs(field: &str, value: f64) -> PyResult<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| {
        pyerr(forge::ForgeError::invalid(format!(
            "{field} must be a non-negative number of seconds"
        )))
    })
}

fn epoch_time(field: &str, value: f64) -> PyResult<SystemTime> {
    if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 {
        return Err(pyerr(forge::ForgeError::invalid(format!(
            "{field} must be non-negative epoch milliseconds"
        ))));
    }
    Ok(UNIX_EPOCH + Duration::from_millis(value as u64))
}

fn redrive_policy(value: &str) -> PyResult<forge::RedriveDedupPolicy> {
    match value {
        "clear" => Ok(forge::RedriveDedupPolicy::Clear),
        "preserve" => Ok(forge::RedriveDedupPolicy::Preserve),
        _ => Err(pyerr(forge::ForgeError::invalid(
            "dedup_policy must be 'clear' or 'preserve'",
        ))),
    }
}

/// Build [`forge::ScheduleOpts`] from native optional controls.
fn schedule_opts(
    max_attempts: Option<u32>,
    misfire_policy: Option<String>,
    max_catch_up: Option<u32>,
) -> PyResult<forge::ScheduleOpts> {
    let mut opts = forge::ScheduleOpts::new();
    if let Some(m) = max_attempts {
        opts = opts.with_max_attempts(m);
    }
    let policy = match misfire_policy.as_deref().unwrap_or("run_once") {
        "skip" => forge::MisfirePolicy::Skip,
        "run_once" => forge::MisfirePolicy::RunOnce,
        "catch_up" => forge::MisfirePolicy::CatchUp(max_catch_up.unwrap_or(10)),
        _ => {
            return Err(pyerr(forge::ForgeError::invalid(
                "misfire_policy must be 'skip', 'run_once', or 'catch_up'",
            )));
        }
    };
    if !matches!(policy, forge::MisfirePolicy::CatchUp(_)) && max_catch_up.unwrap_or(0) != 0 {
        return Err(pyerr(forge::ForgeError::invalid(
            "max_catch_up is only valid with catch_up",
        )));
    }
    Ok(opts.with_misfire_policy(policy))
}

fn schedule_info(schedule: forge::ScheduleInfo) -> ScheduleInfo {
    let (kind, cron_expr) = match schedule.kind {
        forge::ScheduleKind::Cron(expression) => ("cron".to_string(), Some(expression)),
        _ => ("at".to_string(), None),
    };
    ScheduleInfo {
        name: schedule.name,
        kind,
        cron_expr,
        queue: schedule.queue,
        next_run_ms: epoch_ms(schedule.next_run),
        last_run_ms: schedule.last_run.map(epoch_ms),
        paused: schedule.paused,
        misfire_policy: schedule.misfire_policy.name().to_string(),
        max_catch_up: schedule.misfire_policy.max_catch_up(),
    }
}

struct BlobPutArgs {
    content_type: Option<String>,
    metadata: Option<HashMap<String, String>>,
    create_only: bool,
    match_version: Option<String>,
    cache_control: Option<String>,
    content_disposition: Option<String>,
    checksum_sha256: Option<String>,
    sse_algorithm: Option<String>,
    sse_kms_key_id: Option<String>,
}

fn blob_put_opts(args: BlobPutArgs) -> PyResult<PutOpts> {
    if args.create_only && args.match_version.is_some() {
        return Err(pyerr(forge::ForgeError::invalid(
            "create_only and match_version are mutually exclusive",
        )));
    }
    let mut opts = PutOpts::new();
    if let Some(value) = args.content_type {
        opts = opts.with_content_type(value);
    }
    for (name, value) in args.metadata.unwrap_or_default() {
        opts = opts.with_metadata(name, value);
    }
    if let Some(value) = args.cache_control {
        opts = opts.with_cache_control(value);
    }
    if let Some(value) = args.content_disposition {
        opts = opts.with_content_disposition(value);
    }
    if let Some(value) = args.checksum_sha256 {
        opts = opts.with_checksum_sha256(value);
    }
    match args.sse_algorithm.as_deref() {
        Some("AES256") => opts = opts.with_s3_encryption(forge::S3Encryption::S3Managed),
        Some("aws:kms") => {
            opts = opts.with_s3_encryption(forge::S3Encryption::Kms {
                key_id: args.sse_kms_key_id,
            });
        }
        Some(_) => {
            return Err(pyerr(forge::ForgeError::invalid(
                "sse_algorithm must be AES256 or aws:kms",
            )));
        }
        None if args.sse_kms_key_id.is_some() => {
            return Err(pyerr(forge::ForgeError::invalid(
                "sse_kms_key_id requires sse_algorithm aws:kms",
            )));
        }
        None => {}
    }
    if args.create_only {
        opts = opts.create_only();
    } else if let Some(value) = args.match_version {
        opts = opts.match_version(value);
    }
    Ok(opts)
}

fn blob_info(info: forge::BlobInfo) -> BlobInfo {
    BlobInfo {
        key: info.key,
        size: info.size,
        content_type: info.content_type,
        etag: info.etag,
        last_modified_ms: epoch_ms(info.last_modified),
        metadata: info.metadata.into_iter().collect(),
        cache_control: info.cache_control,
        content_disposition: info.content_disposition,
        checksum_sha256: info.checksum_sha256,
        server_side_encryption: info.server_side_encryption,
    }
}

fn core_upload(upload: MultipartUpload) -> forge::MultipartUpload {
    let precondition = if upload.create_only {
        Some(forge::PutPrecondition::CreateOnly)
    } else {
        upload
            .match_version
            .map(forge::PutPrecondition::MatchVersion)
    };
    forge::MultipartUpload::new(upload.key, upload.upload_id, precondition)
}

fn multipart_upload(upload: forge::MultipartUpload) -> MultipartUpload {
    let (create_only, match_version) = match upload.precondition {
        Some(forge::PutPrecondition::CreateOnly) => (true, None),
        Some(forge::PutPrecondition::MatchVersion(value)) => (false, Some(value)),
        None => (false, None),
        Some(_) => (false, None),
    };
    MultipartUpload {
        key: upload.key,
        upload_id: upload.upload_id,
        create_only,
        match_version,
    }
}

/// Map an optional algorithm name onto [`Algo`]. `None` keeps the token-bucket
/// default; `"token_bucket"` / `"sliding_window"` select explicitly; anything else
/// is `Invalid`.
fn parse_algo(name: Option<&str>) -> PyResult<Algo> {
    match name {
        None | Some("token_bucket") => Ok(Algo::TokenBucket),
        Some("sliding_window") => Ok(Algo::SlidingWindow),
        Some(other) => Err(pyerr(forge::ForgeError::invalid(format!(
            "unknown rate-limit algo {other:?}; expected \"token_bucket\" or \"sliding_window\""
        )))),
    }
}

fn parse_priority(value: &str) -> PyResult<forge::Priority> {
    match value {
        "low" => Ok(forge::Priority::Low),
        "normal" => Ok(forge::Priority::Normal),
        "high" => Ok(forge::Priority::High),
        _ => Err(pyerr(forge::ForgeError::invalid(
            "priority must be low, normal, or high",
        ))),
    }
}
fn parse_job_state(value: &str) -> PyResult<forge::JobState> {
    match value {
        "queued" => Ok(forge::JobState::Queued),
        "delayed" => Ok(forge::JobState::Delayed),
        "leased" => Ok(forge::JobState::Leased),
        "retrying" => Ok(forge::JobState::Retrying),
        "succeeded" => Ok(forge::JobState::Succeeded),
        "dead" => Ok(forge::JobState::Dead),
        "cancel_requested" => Ok(forge::JobState::CancelRequested),
        "cancelled" => Ok(forge::JobState::Cancelled),
        _ => Err(pyerr(forge::ForgeError::invalid("unknown job state"))),
    }
}
fn job_status_json(value: &forge::JobStatus) -> serde_json::Value {
    let state = match value.state {
        forge::JobState::Queued => "queued",
        forge::JobState::Delayed => "delayed",
        forge::JobState::Leased => "leased",
        forge::JobState::Retrying => "retrying",
        forge::JobState::Succeeded => "succeeded",
        forge::JobState::Dead => "dead",
        forge::JobState::CancelRequested => "cancel_requested",
        forge::JobState::Cancelled => "cancelled",
    };
    serde_json::json!({"id":value.id.to_string(),"queue":value.queue,"state":state,"attempt_count":value.attempt_count,"max_attempts":value.max_attempts,"priority":format!("{:?}",value.priority).to_ascii_lowercase(),"concurrency_key":value.concurrency_key,"enqueued_at_ms":epoch_ms(value.enqueued_at),"available_at_ms":epoch_ms(value.available_at),"completed_at_ms":value.completed_at.map(epoch_ms)})
}
fn reservation_json(value: &forge::Reservation) -> serde_json::Value {
    serde_json::json!({"id":value.id.to_string(),"reserved_units":value.reserved_units,"expires_at_ms":epoch_ms(value.expires_at),"state":format!("{:?}",value.state).to_ascii_lowercase(),"committed_units":value.committed_units})
}

/// Epoch milliseconds for a `SystemTime` (saturating).
fn epoch_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn proxy_presign(ticket: forge::ProxyPresign) -> ProxyPresign {
    ProxyPresign {
        url: ticket.url,
        method: ticket.method,
        key: ticket.key,
        expires_epoch: ticket.expires_epoch,
        max_bytes: ticket.max_bytes,
        signature: ticket.signature,
        required_headers: ticket.required_headers.into_iter().collect(),
    }
}

fn native_presign(ticket: forge::NativePresign) -> NativePresign {
    NativePresign {
        url: ticket.url,
        method: ticket.method,
        expires_epoch: ticket.expires_epoch,
        required_headers: ticket.required_headers.into_iter().collect(),
        constraints: ticket.constraints.into_iter().collect(),
    }
}

// The value DTOs (Job, Decision, BlobInfo, …) are generated from one schema shared with
// the Node binding (tools/codegen/src/schema.rs). Regenerate with the codegen tool;
// never hand-edit.
include!("types.generated.rs");

fn migration_reports(reports: Vec<forge::MigrationReport>) -> Vec<MigrationReport> {
    reports
        .into_iter()
        .map(|report| MigrationReport {
            target: report.target,
            state: report.state.as_str().to_string(),
            current_version: report.current_version,
            target_version: report.target_version,
            applied: report.applied,
            pending: report.pending,
            lock_holder: report.lock_holder,
            message: report.message,
        })
        .collect()
}

fn health_report(report: forge::HealthReport) -> HealthReport {
    HealthReport {
        live: report.live,
        ready: report.ready,
        checked_at_ms: report.checked_at_ms,
        duration_ms: report.duration_ms,
        backends: report
            .backends
            .into_iter()
            .map(|backend| BackendHealth {
                primitive: backend.primitive.as_str().to_string(),
                provider: backend.provider,
                status: backend.status,
                latency_ms: backend.latency_ms,
                error_category: backend.error_category,
                last_success_ms: backend.last_success_ms,
                message: backend.message,
            })
            .collect(),
    }
}

fn diagnostics_report(report: forge::DiagnosticsReport) -> DiagnosticsReport {
    DiagnosticsReport {
        ready: report.ready,
        checked_at_ms: report.checked_at_ms as f64,
        checks: report
            .checks
            .into_iter()
            .map(|check| DiagnosticCheck {
                name: check.name,
                status: check.status,
                message: check.message,
            })
            .collect(),
    }
}

fn flag_evaluation(value: forge::FlagEvaluation) -> FlagEvaluation {
    FlagEvaluation {
        value_json: value.value_json,
        value_type: value.value_type,
        variant: value.variant,
        reason: value.reason,
        error_code: value.error_code,
    }
}

#[pymethods]
impl FlagEvaluationRequest {
    #[new]
    #[pyo3(signature = (id, key, default_json, targeting_key=None, context_json=None))]
    fn new(
        id: String,
        key: String,
        default_json: String,
        targeting_key: Option<String>,
        context_json: Option<String>,
    ) -> Self {
        Self {
            id,
            key,
            default_json,
            targeting_key,
            context_json,
        }
    }
}

fn core_flag_request(request: &FlagEvaluationRequest) -> PyResult<forge::FlagEvaluationRequest> {
    let default = serde_json::from_str(&request.default_json).map_err(|_| {
        pyerr(forge::ForgeError::invalid(
            "default_json must be valid JSON",
        ))
    })?;
    let attributes = match &request.context_json {
        Some(value) => {
            serde_json::from_str::<std::collections::BTreeMap<String, serde_json::Value>>(value)
                .map_err(|_| {
                    pyerr(forge::ForgeError::invalid(
                        "context_json must be a JSON object",
                    ))
                })?
        }
        None => std::collections::BTreeMap::new(),
    };
    let mut context = request
        .targeting_key
        .clone()
        .map_or_else(EvalCtx::new, EvalCtx::user);
    for (key, value) in attributes {
        context = context.with_field(key, value);
    }
    Ok(forge::FlagEvaluationRequest {
        id: request.id.clone(),
        key: request.key.clone(),
        default,
        context,
    })
}

fn flag_evaluation_entry(value: forge::FlagEvaluationEntry) -> FlagEvaluationEntry {
    FlagEvaluationEntry {
        id: value.id,
        key: value.key,
        evaluation: flag_evaluation(value.evaluation),
    }
}

fn config_snapshot(value: forge::ConfigSnapshot) -> ConfigSnapshot {
    ConfigSnapshot {
        schema_version: value.schema_version,
        created_at_ms: value.created_at_ms as f64,
        expires_at_ms: value.expires_at_ms as f64,
        secret_handling: value.secret_handling,
        config: value
            .config
            .into_iter()
            .map(|entry| ConfigEntry {
                key: entry.key,
                value: entry.value,
            })
            .collect(),
        flags: value.flags.into_iter().map(flag_evaluation_entry).collect(),
    }
}

fn core_config_snapshot(value: &ConfigSnapshot) -> PyResult<forge::ConfigSnapshot> {
    if !value.created_at_ms.is_finite()
        || !value.expires_at_ms.is_finite()
        || value.created_at_ms < 0.0
        || value.expires_at_ms < 0.0
    {
        return Err(pyerr(forge::ForgeError::invalid(
            "snapshot timestamps must be non-negative finite milliseconds",
        )));
    }
    Ok(forge::ConfigSnapshot {
        schema_version: value.schema_version,
        created_at_ms: value.created_at_ms as u64,
        expires_at_ms: value.expires_at_ms as u64,
        secret_handling: value.secret_handling.clone(),
        config: value
            .config
            .iter()
            .map(|entry| forge::ConfigEntry {
                key: entry.key.clone(),
                value: entry.value.clone(),
            })
            .collect(),
        flags: value
            .flags
            .iter()
            .map(|entry| {
                let evaluation = serde_json::from_value(serde_json::json!({
                    "value_json": entry.evaluation.value_json,
                    "value_type": entry.evaluation.value_type,
                    "variant": entry.evaluation.variant,
                    "reason": entry.evaluation.reason,
                    "error_code": entry.evaluation.error_code,
                }))
                .map_err(|_| {
                    pyerr(forge::ForgeError::invalid(
                        "snapshot flag evaluation is invalid",
                    ))
                })?;
                Ok(forge::FlagEvaluationEntry {
                    id: entry.id.clone(),
                    key: entry.key.clone(),
                    evaluation,
                })
            })
            .collect::<PyResult<Vec<_>>>()?,
    })
}

#[pyfunction]
fn encode_config_snapshot<'py>(
    py: Python<'py>,
    snapshot: Py<ConfigSnapshot>,
) -> PyResult<Bound<'py, PyBytes>> {
    let encoded = core_config_snapshot(&snapshot.borrow(py))?
        .encode()
        .map_err(pyerr)?;
    Ok(PyBytes::new(py, &encoded))
}

#[pyfunction]
fn decode_config_snapshot(encoded: Bound<'_, PyBytes>) -> PyResult<ConfigSnapshot> {
    Ok(config_snapshot(
        forge::ConfigSnapshot::decode(encoded.as_bytes()).map_err(pyerr)?,
    ))
}

fn snapshot_secret_handling(value: &str) -> PyResult<forge::SnapshotSecretHandling> {
    match value {
        "no_secrets" => Ok(forge::SnapshotSecretHandling::NoSecrets),
        "application_protected" => Ok(forge::SnapshotSecretHandling::ApplicationProtected),
        _ => Err(pyerr(forge::ForgeError::invalid(
            "secret_handling must be no_secrets or application_protected",
        ))),
    }
}

fn metric_sample(sample: forge::MetricSample) -> MetricSample {
    MetricSample {
        name: sample.name,
        kind: sample.kind,
        labels: sample.labels,
        value: sample.value,
        count: sample.count,
        sum: sample.sum,
    }
}

/// A live subscription, usable as a Python async iterator
/// (`async for payload in subscription:`). Each item is `bytes`.
#[pyclass]
struct Subscription {
    inner: Arc<Mutex<forge::Subscription>>,
    /// Flipped by `aclose`. A `watch` channel rather than the mutex, so `aclose` can
    /// interrupt an `__anext__` that is parked on the stream while holding the lock —
    /// with only the mutex, `aclose` deadlocked until the next message arrived.
    closed_tx: Arc<tokio::sync::watch::Sender<bool>>,
}

#[pymethods]
impl Subscription {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let closed_tx = self.closed_tx.clone();
        future_into_py(py, async move {
            let mut closed_rx = closed_tx.subscribe();
            let mut sub = inner.lock().await;
            tokio::select! {
                // `wait_for` is level-triggered: it also returns when aclose() already ran.
                _ = closed_rx.wait_for(|closed| *closed) => {
                    Err(PyStopAsyncIteration::new_err("subscription closed"))
                }
                item = sub.next() => match item {
                    Some(Ok(b)) => Ok(Python::attach(|py| PyBytes::new(py, &b).unbind())),
                    Some(Err(e)) => Err(pyerr(e)),
                    None => Err(PyStopAsyncIteration::new_err("subscription ended")),
                },
            }
        })
    }

    /// Unsubscribe now, releasing the broadcast receiver instead of waiting for GC. Call when a
    /// client's connection closes (e.g. a GraphQL subscription's WebSocket). Any pending
    /// `__anext__` stops immediately; idempotent; the iterator then stops.
    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let closed_tx = self.closed_tx.clone();
        future_into_py(py, async move {
            let _ = closed_tx.send(true);
            // A parked __anext__ wakes on the send and releases the lock promptly.
            let mut sub = inner.lock().await;
            *sub = futures_util::stream::empty().boxed();
            Ok(())
        })
    }
}

/// A Forge client: one Postgres pool, every primitive, driven on a shared async
/// runtime. Construct with `await ForgeClient.init()`, which reads `forge.toml`.
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
    /// Read `forge.toml` from the current directory and instantiate the runtime from it
    /// (mirrors Rust's `Forge::init`). The file is the single source of configuration; its
    /// string values may reference the environment as `${VAR}` / `${VAR:-default}`. Migrates
    /// the system database at startup. `await` it.
    #[staticmethod]
    fn init(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let forge = Forge::init().await.map_err(pyerr)?;
            Ok(ForgeClient {
                forge,
                leased: Arc::new(Mutex::new(HashMap::new())),
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            })
        })
    }

    /// Like `init`, but reads the `forge.toml` at `path` instead of the one in the current
    /// directory. `await` it.
    #[staticmethod]
    fn init_from(py: Python<'_>, path: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let forge = Forge::init_from(path).await.map_err(pyerr)?;
            Ok(ForgeClient {
                forge,
                leased: Arc::new(Mutex::new(HashMap::new())),
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            })
        })
    }

    /// Parse the canonical configuration from a TOML string. `await` it.
    #[staticmethod]
    fn init_from_string(py: Python<'_>, toml: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let forge = Forge::init_from_str(&toml).await.map_err(pyerr)?;
            Ok(ForgeClient {
                forge,
                leased: Arc::new(Mutex::new(HashMap::new())),
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            })
        })
    }

    /// Create a memory client with a manual clock and seeded token entropy for tests.
    #[staticmethod]
    fn init_memory_for_testing(
        py: Python<'_>,
        toml: String,
        start_ms: f64,
        seed: u64,
    ) -> PyResult<Bound<'_, PyAny>> {
        let start = epoch_time("start_ms", start_ms)?;
        future_into_py(py, async move {
            let forge = forge::Forge::init_memory_for_testing(&toml, start, seed)
                .await
                .map_err(pyerr)?;
            Ok(ForgeClient {
                forge,
                leased: Arc::new(Mutex::new(HashMap::new())),
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            })
        })
    }

    #[staticmethod]
    fn migrate(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(Forge::migrate().await.map_err(pyerr)?))
        })
    }

    #[staticmethod]
    fn migrate_from(py: Python<'_>, path: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::migrate_from(path).await.map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn migrate_from_string(py: Python<'_>, toml: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::migrate_from_str(&toml).await.map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn migration_status(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::migration_status().await.map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn migration_status_from(py: Python<'_>, path: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::migration_status_from(path).await.map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn migration_status_from_string(py: Python<'_>, toml: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::migration_status_from_str(&toml)
                    .await
                    .map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn validate_schema(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::validate_schema().await.map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn validate_schema_from(py: Python<'_>, path: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::validate_schema_from(path).await.map_err(pyerr)?,
            ))
        })
    }

    #[staticmethod]
    fn validate_schema_from_string(py: Python<'_>, toml: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            Ok(migration_reports(
                Forge::validate_schema_from_str(&toml)
                    .await
                    .map_err(pyerr)?,
            ))
        })
    }

    /// Idempotently stop accepting work and close owned resources within the deadline.
    #[pyo3(signature = (timeout_seconds=30.0))]
    fn close<'py>(&self, py: Python<'py>, timeout_seconds: f64) -> PyResult<Bound<'py, PyAny>> {
        let timeout = secs("timeout_seconds", timeout_seconds)?;
        let forge = self.forge.clone();
        future_into_py(py, async move { forge.close(timeout).await.map_err(pyerr) })
    }

    /// Move a test-factory client's manual clock forward without sleeping.
    fn advance_test_clock(&self, seconds: f64) -> PyResult<()> {
        self.forge
            .advance_test_clock(secs("seconds", seconds)?)
            .map_err(pyerr)
    }

    #[pyo3(signature = (key, value, ttl_seconds=None, if_not_exists=None, if_exists=None))]
    fn kv_set<'py>(
        &self,
        py: Python<'py>,
        key: String,
        value: String,
        ttl_seconds: Option<f64>,
        if_not_exists: Option<bool>,
        if_exists: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = SetOpts::new();
            if let Some(t) = ttl_seconds {
                opts = opts.with_ttl(secs("ttl_seconds", t)?);
            }
            // `if_exists` (XX) takes precedence over `if_not_exists` (NX) if both set.
            if if_exists == Some(true) {
                opts = opts.with_mode(SetMode::IfExists);
            } else if if_not_exists == Some(true) {
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

    /// `GET key` → the raw value bytes, or `None`. Lossless, unlike `kv_get`.
    fn kv_get_bytes<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let v = forge.kv().get(&key).await.map_err(pyerr)?;
            Ok(v.map(|b| Python::attach(|py| PyBytes::new(py, &b).unbind())))
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

    fn config_get_many<'py>(
        &self,
        py: Python<'py>,
        keys: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok::<Vec<ConfigEntry>, PyErr>(
                forge
                    .config()
                    .get_many_raw(&keys)
                    .await
                    .map_err(pyerr)?
                    .into_iter()
                    .map(|entry| ConfigEntry {
                        key: entry.key,
                        value: entry.value,
                    })
                    .collect(),
            )
        })
    }

    fn config_delete<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.config().delete_raw(&key).await.map_err(pyerr)
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

    #[pyo3(signature = (key, default_json, targeting_key=None))]
    fn flag_details<'py>(
        &self,
        py: Python<'py>,
        key: String,
        default_json: String,
        targeting_key: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let default = serde_json::from_str(&default_json).map_err(|_| {
            pyerr(forge::ForgeError::invalid(
                "default_json must be valid JSON",
            ))
        })?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let ctx = targeting_key.map_or_else(EvalCtx::new, EvalCtx::user);
            Ok::<FlagEvaluation, PyErr>(flag_evaluation(
                forge.config().flag_details(&key, &default, &ctx).await,
            ))
        })
    }

    fn flag_details_many<'py>(
        &self,
        py: Python<'py>,
        requests: Vec<Py<FlagEvaluationRequest>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let requests = requests
            .iter()
            .map(|request| core_flag_request(&request.borrow(py)))
            .collect::<PyResult<Vec<_>>>()?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok::<Vec<FlagEvaluationEntry>, PyErr>(
                forge
                    .config()
                    .flag_details_many(&requests)
                    .await
                    .map_err(pyerr)?
                    .into_iter()
                    .map(flag_evaluation_entry)
                    .collect(),
            )
        })
    }

    fn config_snapshot<'py>(
        &self,
        py: Python<'py>,
        config_keys: Vec<String>,
        flag_requests: Vec<Py<FlagEvaluationRequest>>,
        max_stale_seconds: f64,
        secret_handling: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let flag_requests = flag_requests
            .iter()
            .map(|request| core_flag_request(&request.borrow(py)))
            .collect::<PyResult<Vec<_>>>()?;
        let max_stale = secs("max_stale_seconds", max_stale_seconds)?;
        let secret_handling = snapshot_secret_handling(&secret_handling)?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok::<ConfigSnapshot, PyErr>(config_snapshot(
                forge
                    .config()
                    .snapshot(&config_keys, &flag_requests, max_stale, secret_handling)
                    .await
                    .map_err(pyerr)?,
            ))
        })
    }

    fn encode_config_snapshot<'py>(
        &self,
        py: Python<'py>,
        snapshot: Py<ConfigSnapshot>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        encode_config_snapshot(py, snapshot)
    }

    fn decode_config_snapshot(&self, encoded: Bound<'_, PyBytes>) -> PyResult<ConfigSnapshot> {
        decode_config_snapshot(encoded)
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
            forge.blob().put(&key, bytes, opts).await.map_err(pyerr)?;
            Ok(Python::attach(|py| py.None()))
        })
    }

    /// Fetch an object as raw `bytes`, or `None`.
    fn blob_get<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let v = forge.blob().get(&key).await.map_err(pyerr)?;
            Ok(v.map(|b| Python::attach(|py| PyBytes::new(py, &b).unbind())))
        })
    }

    #[pyo3(signature = (key, if_match=None, if_none_match=None))]
    fn blob_get_if<'py>(
        &self,
        py: Python<'py>,
        key: String,
        if_match: Option<String>,
        if_none_match: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let result = forge
                .blob()
                .get_if(&key, if_match.as_deref(), if_none_match.as_deref())
                .await
                .map_err(pyerr)?;
            Ok(match result {
                forge::ConditionalGet::Missing => ConditionalBlobGet {
                    state: "missing".to_string(),
                    body: None,
                    etag: None,
                },
                forge::ConditionalGet::NotModified { etag } => ConditionalBlobGet {
                    state: "not_modified".to_string(),
                    body: None,
                    etag: Some(etag),
                },
                forge::ConditionalGet::Found { body, etag } => ConditionalBlobGet {
                    state: "found".to_string(),
                    body: Some(Python::attach(|py| PyBytes::new(py, &body).unbind())),
                    etag: Some(etag),
                },
                _ => {
                    return Err(pyerr(forge::ForgeError::backend(
                        "unknown conditional blob state",
                    )));
                }
            })
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
                .map(proxy_presign)
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
                .presign_upload(&key, secs("expires_seconds", expires_seconds)?, max_bytes)
                .await
                .map(proxy_presign)
                .map_err(pyerr)
        })
    }

    fn blob_presign_native_get<'py>(
        &self,
        py: Python<'py>,
        key: String,
        expires_seconds: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .presign_native_get(&key, secs("expires_seconds", expires_seconds)?)
                .await
                .map(native_presign)
                .map_err(pyerr)
        })
    }

    #[pyo3(signature = (key, expires_seconds, content_type=None, metadata=None, create_only=false, match_version=None, cache_control=None, content_disposition=None, checksum_sha256=None, sse_algorithm=None, sse_kms_key_id=None))]
    #[allow(clippy::too_many_arguments)]
    fn blob_presign_native_put<'py>(
        &self,
        py: Python<'py>,
        key: String,
        expires_seconds: f64,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        create_only: bool,
        match_version: Option<String>,
        cache_control: Option<String>,
        content_disposition: Option<String>,
        checksum_sha256: Option<String>,
        sse_algorithm: Option<String>,
        sse_kms_key_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = blob_put_opts(BlobPutArgs {
            content_type,
            metadata,
            create_only,
            match_version,
            cache_control,
            content_disposition,
            checksum_sha256,
            sse_algorithm,
            sse_kms_key_id,
        })?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .presign_native_put(&key, secs("expires_seconds", expires_seconds)?, opts)
                .await
                .map(native_presign)
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
        future_into_py(py, async move {
            forge.blob().delete(&key).await.map_err(pyerr)?;
            Ok(Python::attach(|py| py.None()))
        })
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
    /// `True`, re-hash the plaintext and persist it (transparent upgrade, no reset).
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
                expires_at_ms: k.expires_at.map(epoch_ms),
                scopes: k.scopes,
                metadata: k.metadata,
            })
        })
    }

    #[pyo3(signature = (owner_id, label, expires_in_seconds=None, scopes=None, metadata=None))]
    fn create_api_key_with<'py>(
        &self,
        py: Python<'py>,
        owner_id: String,
        label: String,
        expires_in_seconds: Option<f64>,
        scopes: Option<Vec<String>>,
        metadata: Option<HashMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = ApiKeyOpts::new()
                .with_scopes(scopes.unwrap_or_default())
                .with_metadata(metadata.unwrap_or_default());
            if let Some(value) = expires_in_seconds {
                opts = opts.with_expires_in(secs("expires_in_seconds", value)?);
            }
            let key = forge
                .auth()
                .create_api_key_with(&owner_id, &label, opts)
                .await
                .map_err(pyerr)?;
            Ok(ApiKey {
                id: key.id,
                secret: key.secret.as_str().to_string(),
                label: key.label,
                created_at_ms: epoch_ms(key.created_at),
                expires_at_ms: key.expires_at.map(epoch_ms),
                scopes: key.scopes,
                metadata: key.metadata,
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
                .map(|i| ApiKeyInfo {
                    id: i.id,
                    owner_id: i.owner_id,
                    label: i.label,
                    expires_at_ms: i.expires_at.map(epoch_ms),
                    scopes: i.scopes,
                    metadata: i.metadata,
                }))
        })
    }

    /// Mint a single-use token scoped to `purpose` (e.g. "password-reset"), expiring
    /// after `ttl_seconds`; returns the opaque token (shown once).
    #[pyo3(signature = (user_id, purpose, ttl_seconds, payload=None))]
    fn create_token<'py>(
        &self,
        py: Python<'py>,
        user_id: String,
        purpose: String,
        ttl_seconds: f64,
        payload: Option<Vec<u8>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let ttl = secs("ttl_seconds", ttl_seconds)?;
            let t = forge
                .auth()
                .create_token_with_payload(
                    &user_id,
                    &purpose,
                    ttl,
                    forge::Bytes::from(payload.unwrap_or_default()),
                )
                .await
                .map_err(pyerr)?;
            Ok(t.as_str().to_string())
        })
    }

    fn create_token_with_payload<'py>(
        &self,
        py: Python<'py>,
        user_id: String,
        purpose: String,
        ttl_seconds: f64,
        payload: Vec<u8>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let token = forge
                .auth()
                .create_token_with_payload(
                    &user_id,
                    &purpose,
                    secs("ttl_seconds", ttl_seconds)?,
                    forge::Bytes::from(payload),
                )
                .await
                .map_err(pyerr)?;
            Ok(token.as_str().to_string())
        })
    }

    /// Atomically consume a token minted for `purpose`; returns its user and payload, or None
    /// when unknown/expired/already consumed.
    fn consume_token<'py>(
        &self,
        py: Python<'py>,
        token: String,
        purpose: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .auth()
                .consume_token_with_payload(&token, &purpose)
                .await
                .map_err(pyerr)?
                .map(|value| TokenConsumption {
                    user_id: value.user_id,
                    payload: Python::attach(|py| PyBytes::new(py, &value.payload).unbind()),
                }))
        })
    }

    fn consume_token_with_payload<'py>(
        &self,
        py: Python<'py>,
        token: String,
        purpose: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge
                .auth()
                .consume_token_with_payload(&token, &purpose)
                .await
                .map_err(pyerr)?
                .map(|value| TokenConsumption {
                    user_id: value.user_id,
                    payload: Python::attach(|py| PyBytes::new(py, &value.payload).unbind()),
                }))
        })
    }

    /// Schedule a one-shot enqueue at `when_epoch_ms`; returns the future JobId.
    #[pyo3(signature = (when_epoch_ms, queue, payload, max_attempts=None, misfire_policy=None, max_catch_up=None))]
    #[allow(clippy::too_many_arguments)]
    fn schedule_at<'py>(
        &self,
        py: Python<'py>,
        when_epoch_ms: f64,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
        misfire_policy: Option<String>,
        max_catch_up: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let when = UNIX_EPOCH + Duration::from_millis(when_epoch_ms.max(0.0) as u64);
            let id = forge
                .schedule()
                .at(
                    when,
                    &queue,
                    forge::Bytes::from(payload),
                    schedule_opts(max_attempts, misfire_policy, max_catch_up)?,
                )
                .await
                .map_err(pyerr)?;
            Ok(id.to_string())
        })
    }

    /// Upsert a recurring cron schedule. `max_attempts` overrides the delivery
    /// attempts of the job each tick enqueues (omit for the queue default of 5).
    #[pyo3(signature = (name, expr, queue, payload, max_attempts=None, misfire_policy=None, max_catch_up=None))]
    #[allow(clippy::too_many_arguments)]
    fn schedule_cron<'py>(
        &self,
        py: Python<'py>,
        name: String,
        expr: String,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
        misfire_policy: Option<String>,
        max_catch_up: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .schedule()
                .cron(
                    &name,
                    &expr,
                    &queue,
                    forge::Bytes::from(payload),
                    schedule_opts(max_attempts, misfire_policy, max_catch_up)?,
                )
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

    /// Approximate depth (`QueueDepth` with `visible`/`in_flight`/`delayed`) for a
    /// queue (SQS `GetQueueAttributes`). Pass `"<queue>.dlq"` to gauge a dead-letter
    /// backlog without leasing its jobs. For a DLQ every job is `visible`, so
    /// `visible` is its size.
    fn queue_depth<'py>(&self, py: Python<'py>, queue: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let d = forge.queue().depth(&queue).await.map_err(pyerr)?;
            Ok(QueueDepth {
                visible: d.visible,
                in_flight: d.in_flight,
                delayed: d.delayed,
                oldest_visible_age_ms: d.oldest_visible_age_ms.map(|value| value as f64),
            })
        })
    }

    fn queue_pause<'py>(&self, py: Python<'py>, queue: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.queue().pause(&queue).await.map_err(pyerr)
        })
    }

    fn queue_resume<'py>(&self, py: Python<'py>, queue: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.queue().resume(&queue).await.map_err(pyerr)
        })
    }

    fn queue_is_paused<'py>(&self, py: Python<'py>, queue: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.queue().is_paused(&queue).await.map_err(pyerr)
        })
    }

    fn queue_stats<'py>(&self, py: Python<'py>, queue: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let stats = forge.queue().stats(&queue).await.map_err(pyerr)?;
            Ok(QueueStats {
                enqueued_total: stats.enqueued_total,
                settled_total: stats.settled_total,
                dead_total: stats.dead_total,
                cancelled_total: stats.cancelled_total,
                enqueue_rate_per_minute: stats.enqueue_rate_per_minute,
                settle_rate_per_minute: stats.settle_rate_per_minute,
                oldest_visible_age_ms: stats.oldest_visible_age_ms.map(|value| value as f64),
                paused: stats.paused,
            })
        })
    }

    #[pyo3(signature = (queue, cursor=None, limit=50))]
    fn queue_dead_letters<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        cursor: Option<String>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let page = forge
                .queue()
                .dead_letters(&queue, cursor.map(forge::Cursor::from_token), limit)
                .await
                .map_err(pyerr)?;
            Ok(DeadLetterPage {
                items: page
                    .items
                    .into_iter()
                    .map(|item| DeadLetterInfo {
                        job_id: item.job_id.to_string(),
                        queue: item.queue,
                        attempt_count: item.attempt_count,
                        enqueued_at_ms: epoch_ms(item.enqueued_at),
                        dead_lettered_at_ms: epoch_ms(item.dead_lettered_at),
                        failure_summary: item.failure_summary,
                    })
                    .collect(),
                cursor: page.next_cursor.map(|value| value.token().to_string()),
            })
        })
    }

    fn queue_redrive<'py>(
        &self,
        py: Python<'py>,
        job_id: String,
        destination: String,
        dedup_policy: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .queue()
                .redrive(
                    forge::JobId::parse(&job_id).map_err(pyerr)?,
                    forge::RedriveOpts::new(destination, redrive_policy(&dedup_policy)?),
                )
                .await
                .map_err(pyerr)
        })
    }

    #[pyo3(signature = (queue, destination, dedup_policy, cursor=None, limit=50))]
    fn queue_redrive_batch<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        destination: String,
        dedup_policy: String,
        cursor: Option<String>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let result = forge
                .queue()
                .redrive_batch(
                    &queue,
                    cursor.map(forge::Cursor::from_token),
                    limit,
                    forge::RedriveOpts::new(destination, redrive_policy(&dedup_policy)?),
                )
                .await
                .map_err(pyerr)?;
            Ok(RedriveBatchResult {
                redriven: result.redriven,
                cursor: result.next_cursor.map(|value| value.token().to_string()),
            })
        })
    }

    fn queue_purge_dead_letters_dry_run<'py>(
        &self,
        py: Python<'py>,
        queue: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .queue()
                .purge_dead_letters_dry_run(&queue)
                .await
                .map_err(pyerr)
        })
    }

    fn queue_purge_dead_letters<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        confirmation: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .queue()
                .purge_dead_letters(&queue, &confirmation)
                .await
                .map_err(pyerr)
        })
    }

    #[pyo3(signature = (batch_size=None, claim_seconds=None, failure_backoff_seconds=None, baggage_allowlist=None))]
    fn run_outbox_once<'py>(
        &self,
        py: Python<'py>,
        batch_size: Option<u32>,
        claim_seconds: Option<f64>,
        failure_backoff_seconds: Option<f64>,
        baggage_allowlist: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let mut opts = forge::OutboxRelayOpts::new();
            if let Some(value) = batch_size {
                opts = opts.with_batch_size(value);
            }
            if let Some(value) = claim_seconds {
                opts = opts.with_claim_for(secs("claim_seconds", value)?);
            }
            if let Some(value) = failure_backoff_seconds {
                opts = opts.with_failure_backoff(secs("failure_backoff_seconds", value)?);
            }
            if let Some(value) = baggage_allowlist {
                opts = opts.with_baggage_allowlist(value);
            }
            let report = forge.run_outbox_once(opts).await.map_err(pyerr)?;
            Ok(OutboxRelayReport {
                claimed: report.claimed,
                dispatched: report.dispatched,
                failed: report.failed,
                pending: report.pending,
                oldest_pending_age_ms: report.oldest_pending_age_ms.map(|value| value as f64),
            })
        })
    }

    #[pyo3(signature = (queue, payload, max_attempts=None, dedup_id=None, delay_seconds=None, job_id=None, traceparent=None, tracestate=None, baggage=None, baggage_allowlist=None, priority=None, concurrency_key=None))]
    #[allow(clippy::too_many_arguments)]
    fn queue_enqueue<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        payload: Vec<u8>,
        max_attempts: Option<u32>,
        dedup_id: Option<String>,
        delay_seconds: Option<f64>,
        job_id: Option<String>,
        traceparent: Option<String>,
        tracestate: Option<String>,
        baggage: Option<String>,
        baggage_allowlist: Option<Vec<String>>,
        priority: Option<String>,
        concurrency_key: Option<String>,
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
            if let Some(s) = delay_seconds {
                opts = opts.with_delay(secs("delay_seconds", s)?);
            }
            if let Some(id) = job_id {
                opts = opts.with_job_id(forge::JobId::parse(&id).map_err(pyerr)?);
            }
            if let Some(traceparent) = traceparent {
                opts = opts.with_trace_context(
                    forge::TraceContext::from_headers(
                        traceparent,
                        tracestate,
                        baggage,
                        &baggage_allowlist.unwrap_or_default(),
                    )
                    .map_err(pyerr)?,
                );
            } else if tracestate.is_some() || baggage.is_some() {
                return Err(pyerr(forge::ForgeError::invalid(
                    "traceparent is required when tracestate or baggage is set",
                )));
            }
            if let Some(priority) = priority {
                opts = opts.with_priority(parse_priority(&priority)?);
            }
            if let Some(key) = concurrency_key {
                opts = opts.with_concurrency_key(key);
            }
            let id = forge
                .queue()
                .enqueue(&queue, forge::Bytes::from(payload), opts)
                .await
                .map_err(pyerr)?;
            Ok(id.to_string())
        })
    }

    /// Enqueue up to 100 `(payload_bytes, optional_job_id)` items with ordered results.
    fn queue_enqueue_batch<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        items: Vec<(Vec<u8>, Option<String>)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            if items.is_empty() || items.len() > forge::MAX_ENQUEUE_BATCH {
                return Err(pyerr(forge::ForgeError::limit(
                    "batch enqueue size must be in 1..=100",
                )));
            }
            let mut results = Vec::with_capacity(items.len());
            for (payload, job_id) in items {
                let opts = match job_id {
                    Some(value) => match forge::JobId::parse(&value) {
                        Ok(id) => forge::EnqueueOpts::new().with_job_id(id),
                        Err(error) => {
                            results.push(BatchEnqueueResult {
                                job_id: None,
                                error_code: Some(error.code().to_string()),
                                retryable: error.is_retryable(),
                                message: Some(error.safe_message().to_string()),
                            });
                            continue;
                        }
                    },
                    None => forge::EnqueueOpts::new(),
                };
                match forge
                    .queue()
                    .enqueue(&queue, forge::Bytes::from(payload), opts)
                    .await
                {
                    Ok(id) => results.push(BatchEnqueueResult {
                        job_id: Some(id.to_string()),
                        error_code: None,
                        retryable: false,
                        message: None,
                    }),
                    Err(error) => results.push(BatchEnqueueResult {
                        job_id: None,
                        error_code: Some(error.code().to_string()),
                        retryable: error.is_retryable(),
                        message: Some(error.safe_message().to_string()),
                    }),
                }
            }
            Ok(results)
        })
    }

    /// Lease one job, long-polling up to `wait_seconds`. Returns a `Job` (settle it
    /// with `queue_ack`/`queue_nack`/`queue_heartbeat` by `job.receipt`) or `None`.
    #[pyo3(signature = (queue, visibility_seconds, wait_seconds, concurrency_limit_per_key=None))]
    fn queue_dequeue<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        visibility_seconds: f64,
        wait_seconds: f64,
        concurrency_limit_per_key: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        let seq = self.seq.clone();
        future_into_py(py, async move {
            let mut opts = forge::DequeueOpts::new()
                .with_visibility_timeout(secs("visibility_seconds", visibility_seconds)?)
                .with_wait(secs("wait_seconds", wait_seconds)?);
            if let Some(limit) = concurrency_limit_per_key {
                opts = opts.with_concurrency_limit_per_key(limit);
            }
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
                        payload: Python::attach(|py| PyBytes::new(py, &job.payload).unbind()),
                        attempt: job.attempt,
                        max_attempts: job.max_attempts,
                        leased_until_ms: epoch_ms(job.leased_until),
                        queue: job.queue.clone(),
                        traceparent: job
                            .trace_context
                            .as_ref()
                            .map(|context| context.traceparent().to_string()),
                        tracestate: job
                            .trace_context
                            .as_ref()
                            .and_then(|context| context.tracestate().map(str::to_string)),
                        baggage: job
                            .trace_context
                            .as_ref()
                            .and_then(|context| context.baggage().map(str::to_string)),
                    };
                    let mut map = leased.lock().await;
                    map.insert(receipt, job);
                    Ok(Some(out))
                }
                None => Ok(None),
            }
        })
    }

    #[pyo3(signature = (queue, max_items, visibility_seconds=30.0, wait_seconds=20.0, concurrency_limit_per_key=None))]
    fn queue_dequeue_batch<'py>(
        &self,
        py: Python<'py>,
        queue: String,
        max_items: u32,
        visibility_seconds: f64,
        wait_seconds: f64,
        concurrency_limit_per_key: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        let seq = self.seq.clone();
        future_into_py(py, async move {
            let mut opts = forge::DequeueOpts::new()
                .with_visibility_timeout(secs("visibility_seconds", visibility_seconds)?)
                .with_wait(secs("wait_seconds", wait_seconds)?);
            if let Some(limit) = concurrency_limit_per_key {
                opts = opts.with_concurrency_limit_per_key(limit);
            }
            let jobs = forge
                .queue()
                .dequeue_batch(&queue, max_items as usize, opts)
                .await
                .map_err(pyerr)?;
            let mut output = Vec::with_capacity(jobs.len());
            let mut map = leased.lock().await;
            for job in jobs {
                let receipt = format!(
                    "{}:{}",
                    job.id,
                    seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                );
                output.push(Job {
                    id: job.id.to_string(),
                    receipt: receipt.clone(),
                    payload: Python::attach(|py| PyBytes::new(py, &job.payload).unbind()),
                    attempt: job.attempt,
                    max_attempts: job.max_attempts,
                    leased_until_ms: epoch_ms(job.leased_until),
                    queue: job.queue.clone(),
                    traceparent: job
                        .trace_context
                        .as_ref()
                        .map(|context| context.traceparent().to_string()),
                    tracestate: job
                        .trace_context
                        .as_ref()
                        .and_then(|context| context.tracestate().map(str::to_string)),
                    baggage: job
                        .trace_context
                        .as_ref()
                        .and_then(|context| context.baggage().map(str::to_string)),
                });
                map.insert(receipt, job);
            }
            Ok(output)
        })
    }

    /// Ack a leased job by its `receipt`. Raises `Precondition` if the receipt is
    /// unknown or belongs to another client/namespace.
    fn queue_ack<'py>(&self, py: Python<'py>, receipt: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.remove(&receipt);
            let Some(job) = job else {
                return Err(pyerr(forge::ForgeError::precondition(
                    "unknown receipt: the lease was lost",
                )));
            };
            forge.queue().ack(&job).await.map_err(pyerr)
        })
    }

    /// Nack a leased job by its `receipt`. Raises `Precondition` if the receipt is
    /// unknown (the lease was lost; stop working on this job).
    #[pyo3(signature = (receipt, retry_seconds=None, failure_summary=None))]
    fn queue_nack<'py>(
        &self,
        py: Python<'py>,
        receipt: String,
        retry_seconds: Option<f64>,
        failure_summary: Option<String>,
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
            let mut opts = match retry_seconds {
                Some(s) => forge::NackOpts::retry_in(secs("retry_seconds", s)?),
                None => forge::NackOpts::default(),
            };
            if let Some(summary) = failure_summary {
                opts = opts.with_failure_summary(summary);
            }
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
            forge.queue().heartbeat(&job).await.map_err(pyerr)?;
            if let Some(stored) = leased.lock().await.get_mut(&receipt)
                && stored.id == job.id
                && stored.lease_token() == job.lease_token()
            {
                stored.leased_until = SystemTime::now();
            }
            Ok(())
        })
    }

    fn queue_cancellation_requested<'py>(
        &self,
        py: Python<'py>,
        receipt: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.get(&receipt).cloned().ok_or_else(|| {
                pyerr(forge::ForgeError::precondition(
                    "unknown receipt: the lease was lost",
                ))
            })?;
            forge
                .queue()
                .cancellation_requested(&job)
                .await
                .map_err(pyerr)
        })
    }
    fn queue_finish_cancellation<'py>(
        &self,
        py: Python<'py>,
        receipt: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let leased = self.leased.clone();
        future_into_py(py, async move {
            let job = leased.lock().await.remove(&receipt).ok_or_else(|| {
                pyerr(forge::ForgeError::precondition(
                    "unknown receipt: the lease was lost",
                ))
            })?;
            forge.queue().finish_cancellation(&job).await.map_err(pyerr)
        })
    }
    fn queue_cancel<'py>(&self, py: Python<'py>, job_id: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let id = forge::JobId::parse(&job_id).map_err(pyerr)?;
            forge
                .queue()
                .cancel(id)
                .await
                .map_err(pyerr)?
                .map(|value| {
                    serde_json::to_string(&job_status_json(&value))
                        .map_err(|error| PyException::new_err(error.to_string()))
                })
                .transpose()
        })
    }
    fn queue_status<'py>(&self, py: Python<'py>, job_id: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let id = forge::JobId::parse(&job_id).map_err(pyerr)?;
            forge
                .queue()
                .status(id)
                .await
                .map_err(pyerr)?
                .map(|value| {
                    serde_json::to_string(&job_status_json(&value))
                        .map_err(|error| PyException::new_err(error.to_string()))
                })
                .transpose()
        })
    }
    #[pyo3(signature=(queue=None,states=None,cursor=None,limit=50))]
    fn queue_list_status<'py>(
        &self,
        py: Python<'py>,
        queue: Option<String>,
        states: Option<Vec<String>>,
        cursor: Option<String>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let states = states
                .unwrap_or_default()
                .iter()
                .map(|value| parse_job_state(value))
                .collect::<PyResult<Vec<_>>>()?;
            let page = forge
                .queue()
                .list_status(forge::JobStatusFilter {
                    queue,
                    states,
                    cursor: cursor.map(forge::Cursor::from_token),
                    limit,
                })
                .await
                .map_err(pyerr)?;
            serde_json::to_string(&serde_json::json!({"items":page.items.iter().map(job_status_json).collect::<Vec<_>>(),"cursor":page.next_cursor.map(|value|value.token().to_string())})).map_err(|error|PyException::new_err(error.to_string()))
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
            let (closed_tx, _) = tokio::sync::watch::channel(false);
            Ok(Subscription {
                inner: Arc::new(Mutex::new(sub)),
                closed_tx: Arc::new(closed_tx),
            })
        })
    }

    /// The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. Pure and cheap; kept
    /// for parity with the Node binding. Prefer `pubsub_subscribe`.
    fn pubsub_channel(&self, topic: String) -> PyResult<String> {
        self.forge.pubsub().channel_for(&topic).map_err(pyerr)
    }

    /// The resolved connection string of Forge's system database — the configured
    /// `[postgres] url`, or the DSN an embedded server minted at init. Contains
    /// credentials; use it to point the app's own tables/pool at the same database
    /// (the only way to reach an embedded server from outside Forge).
    fn postgres_url(&self) -> PyResult<String> {
        self.forge.postgres_url().map(str::to_string).map_err(pyerr)
    }

    /// Static provider capabilities. This performs no I/O.
    fn backend_capabilities(&self) -> Vec<BackendInfo> {
        self.forge
            .backend_capabilities()
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

    fn is_live(&self) -> bool {
        self.forge.is_live()
    }

    #[pyo3(signature = (deadline_seconds=2.0, readiness_backends=None))]
    fn probe<'py>(
        &self,
        py: Python<'py>,
        deadline_seconds: f64,
        readiness_backends: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let readiness_backends = readiness_backends
            .unwrap_or_default()
            .iter()
            .map(|value| forge::Primitive::parse(value).map_err(pyerr))
            .collect::<PyResult<Vec<_>>>()?;
        let options = forge::ProbeOptions::new()
            .with_deadline(secs("deadline_seconds", deadline_seconds)?)
            .with_readiness_backends(readiness_backends);
        future_into_py(py, async move {
            forge.probe(options).await.map(health_report).map_err(pyerr)
        })
    }

    #[pyo3(signature = (deadline_seconds=2.0))]
    fn diagnostics<'py>(
        &self,
        py: Python<'py>,
        deadline_seconds: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        let deadline = secs("deadline_seconds", deadline_seconds)?;
        future_into_py(py, async move {
            forge
                .diagnostics(deadline)
                .await
                .map(diagnostics_report)
                .map_err(pyerr)
        })
    }

    fn metrics_snapshot(&self) -> Vec<MetricSample> {
        self.forge
            .metrics_snapshot()
            .into_iter()
            .map(metric_sample)
            .collect()
    }

    fn render_prometheus(&self) -> String {
        self.forge.render_prometheus()
    }

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

    /// `SCAN prefix*` with cursor pagination. Returns a `ScanPage` (`keys` plus a
    /// next-page `cursor`); pass `cursor` back for the next page (`None` when done).
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
            Ok(ScanPage {
                keys,
                cursor: next.map(|c| c.token().to_string()),
            })
        })
    }

    /// `HeadObject`: full metadata, or `None` if the object does not exist.
    fn blob_head<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            Ok(forge.blob().head(&key).await.map_err(pyerr)?.map(blob_info))
        })
    }

    #[pyo3(signature = (source, destination, content_type=None, metadata=None, create_only=false, match_version=None, cache_control=None, content_disposition=None, checksum_sha256=None, sse_algorithm=None, sse_kms_key_id=None))]
    #[allow(clippy::too_many_arguments)]
    fn blob_copy<'py>(
        &self,
        py: Python<'py>,
        source: String,
        destination: String,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        create_only: bool,
        match_version: Option<String>,
        cache_control: Option<String>,
        content_disposition: Option<String>,
        checksum_sha256: Option<String>,
        sse_algorithm: Option<String>,
        sse_kms_key_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = blob_put_opts(BlobPutArgs {
            content_type,
            metadata,
            create_only,
            match_version,
            cache_control,
            content_disposition,
            checksum_sha256,
            sse_algorithm,
            sse_kms_key_id,
        })?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .copy(&source, &destination, opts)
                .await
                .map(blob_info)
                .map_err(pyerr)
        })
    }

    #[pyo3(signature = (key, content_type=None, metadata=None, create_only=false, match_version=None, cache_control=None, content_disposition=None, sse_algorithm=None, sse_kms_key_id=None))]
    #[allow(clippy::too_many_arguments)]
    fn blob_create_multipart<'py>(
        &self,
        py: Python<'py>,
        key: String,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        create_only: bool,
        match_version: Option<String>,
        cache_control: Option<String>,
        content_disposition: Option<String>,
        sse_algorithm: Option<String>,
        sse_kms_key_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = blob_put_opts(BlobPutArgs {
            content_type,
            metadata,
            create_only,
            match_version,
            cache_control,
            content_disposition,
            checksum_sha256: None,
            sse_algorithm,
            sse_kms_key_id,
        })?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .create_multipart(&key, opts)
                .await
                .map(multipart_upload)
                .map_err(pyerr)
        })
    }

    fn blob_upload_part<'py>(
        &self,
        py: Python<'py>,
        upload: Py<MultipartUpload>,
        part_number: u32,
        body: Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let upload = core_upload(upload.borrow(py).clone());
        let body = forge::Bytes::from(body.as_bytes().to_vec());
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .upload_part(&upload, part_number, body)
                .await
                .map(|part| MultipartPart {
                    part_number: part.part_number,
                    etag: part.etag,
                    size: part.size,
                })
                .map_err(pyerr)
        })
    }

    fn blob_complete_multipart<'py>(
        &self,
        py: Python<'py>,
        upload: Py<MultipartUpload>,
        parts: Vec<Py<MultipartPart>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let upload = core_upload(upload.borrow(py).clone());
        let parts = parts
            .into_iter()
            .map(|part| {
                let part = part.borrow(py);
                forge::MultipartPart::new(part.part_number, part.etag.clone(), part.size)
            })
            .collect();
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .complete_multipart(&upload, parts)
                .await
                .map(blob_info)
                .map_err(pyerr)
        })
    }

    fn blob_abort_multipart<'py>(
        &self,
        py: Python<'py>,
        upload: Py<MultipartUpload>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let upload = core_upload(upload.borrow(py).clone());
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.blob().abort_multipart(&upload).await.map_err(pyerr)?;
            Ok(Python::attach(|py| py.None()))
        })
    }

    fn blob_verify_checksum_sha256<'py>(
        &self,
        py: Python<'py>,
        key: String,
        expected_hex: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .blob()
                .verify_checksum_sha256(&key, &expected_hex)
                .await
                .map_err(pyerr)
        })
    }

    /// `ListObjectsV2`: up to `limit` objects under `prefix`, lexicographic, with cursor
    /// pagination. Returns a `BlobListPage` (`items` plus a next-page `cursor`).
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
            let items: Vec<BlobSummary> = page
                .items
                .into_iter()
                .map(|i| BlobSummary {
                    key: i.key,
                    size: i.size,
                    etag: i.etag,
                    last_modified_ms: epoch_ms(i.last_modified),
                })
                .collect();
            Ok(BlobListPage {
                items,
                cursor: page.next.map(|c| c.token().to_string()),
            })
        })
    }

    /// Store an object from raw `bytes` with optional content type and user metadata.
    #[pyo3(signature = (key, data, content_type=None, metadata=None, create_only=false, match_version=None, cache_control=None, content_disposition=None, checksum_sha256=None, sse_algorithm=None, sse_kms_key_id=None))]
    #[allow(clippy::too_many_arguments)]
    fn blob_put_object<'py>(
        &self,
        py: Python<'py>,
        key: String,
        data: Bound<'py, PyBytes>,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        create_only: bool,
        match_version: Option<String>,
        cache_control: Option<String>,
        content_disposition: Option<String>,
        checksum_sha256: Option<String>,
        sse_algorithm: Option<String>,
        sse_kms_key_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let bytes = forge::Bytes::from(data.as_bytes().to_vec());
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let opts = blob_put_opts(BlobPutArgs {
                content_type,
                metadata,
                create_only,
                match_version,
                cache_control,
                content_disposition,
                checksum_sha256,
                sse_algorithm,
                sse_kms_key_id,
            })?;
            forge.blob().put(&key, bytes, opts).await.map_err(pyerr)?;
            Ok(Python::attach(|py| py.None()))
        })
    }

    #[pyo3(signature = (key, path, content_type=None, metadata=None, create_only=false, match_version=None, cache_control=None, content_disposition=None, checksum_sha256=None, sse_algorithm=None, sse_kms_key_id=None))]
    #[allow(clippy::too_many_arguments)]
    fn blob_put_file<'py>(
        &self,
        py: Python<'py>,
        key: String,
        path: String,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        create_only: bool,
        match_version: Option<String>,
        cache_control: Option<String>,
        content_disposition: Option<String>,
        checksum_sha256: Option<String>,
        sse_algorithm: Option<String>,
        sse_kms_key_id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = blob_put_opts(BlobPutArgs {
            content_type,
            metadata,
            create_only,
            match_version,
            cache_control,
            content_disposition,
            checksum_sha256,
            sse_algorithm,
            sse_kms_key_id,
        })?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let file = tokio::fs::File::open(path).await.map_err(|error| {
                pyerr(forge::ForgeError::backend_with(
                    "could not open blob input file",
                    false,
                    error,
                ))
            })?;
            let size = file
                .metadata()
                .await
                .map_err(|error| {
                    pyerr(forge::ForgeError::backend_with(
                        "could not stat blob input file",
                        false,
                        error,
                    ))
                })?
                .len();
            forge
                .blob()
                .put_stream(&key, Box::pin(file), size, opts)
                .await
                .map_err(pyerr)?;
            Ok(Python::attach(|py| py.None()))
        })
    }

    fn blob_get_range<'py>(
        &self,
        py: Python<'py>,
        key: String,
        start: u64,
        end: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let value = forge
                .blob()
                .get_range(&key, start, end)
                .await
                .map_err(pyerr)?;
            Ok(value.map(|body| Python::attach(|py| PyBytes::new(py, &body).unbind())))
        })
    }

    /// Atomic check-and-consume of one unit, returning the full [`Decision`] (all
    /// IETF RateLimit fields). `fail_open` overrides the instance default for what
    /// happens on a backend error: `None` = default, `True` = allow, `False` = deny.
    /// `algo` selects the algorithm: `"token_bucket"` (default) or `"sliding_window"`.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (bucket, key, max, per_seconds, fail_open=None, algo=None, cost=1))]
    fn rate_limit_check<'py>(
        &self,
        py: Python<'py>,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
        fail_open: Option<bool>,
        algo: Option<String>,
        cost: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let algo = parse_algo(algo.as_deref())?;
            let limit =
                forge::Limit::per_duration(max, secs("per_seconds", per_seconds)?).with_algo(algo);
            let mode = match fail_open {
                None => FailMode::Default,
                Some(true) => FailMode::Open,
                Some(false) => FailMode::Closed,
            };
            let d = forge
                .ratelimit()
                .check_cost_with(&bucket, &key, limit, cost, mode)
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

    #[pyo3(signature=(bucket,key,max,per_seconds,cost,ttl_seconds,algo=None))]
    #[allow(clippy::too_many_arguments)]
    fn rate_limit_reserve<'py>(
        &self,
        py: Python<'py>,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
        cost: u32,
        ttl_seconds: f64,
        algo: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let limit = forge::Limit::per_duration(max, secs("per_seconds", per_seconds)?)
                .with_algo(parse_algo(algo.as_deref())?);
            forge
                .ratelimit()
                .reserve(
                    &bucket,
                    &key,
                    limit,
                    cost,
                    secs("ttl_seconds", ttl_seconds)?,
                )
                .await
                .map_err(pyerr)?
                .map(|value| {
                    serde_json::to_string(&reservation_json(&value))
                        .map_err(|error| PyException::new_err(error.to_string()))
                })
                .transpose()
        })
    }
    fn rate_limit_commit<'py>(
        &self,
        py: Python<'py>,
        reservation_id: String,
        actual_units: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let id = forge::parse_reservation_id(&reservation_id).map_err(pyerr)?;
            let value = forge
                .ratelimit()
                .commit(id, actual_units)
                .await
                .map_err(pyerr)?;
            serde_json::to_string(&reservation_json(&value))
                .map_err(|error| PyException::new_err(error.to_string()))
        })
    }
    fn rate_limit_release<'py>(
        &self,
        py: Python<'py>,
        reservation_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let id = forge::parse_reservation_id(&reservation_id).map_err(pyerr)?;
            let value = forge.ratelimit().release(id).await.map_err(pyerr)?;
            serde_json::to_string(&reservation_json(&value))
                .map_err(|error| PyException::new_err(error.to_string()))
        })
    }

    /// Cancel a schedule by name. `True` if one was removed, `False` if none existed.
    fn schedule_cancel<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.schedule().cancel(&name).await.map_err(pyerr)
        })
    }

    fn schedule_inspect<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .schedule()
                .inspect(&name)
                .await
                .map(|value| value.map(schedule_info))
                .map_err(pyerr)
        })
    }

    fn schedule_pause<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.schedule().pause(&name).await.map_err(pyerr)
        })
    }

    fn schedule_resume<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.schedule().resume(&name).await.map_err(pyerr)
        })
    }

    fn scheduler_diagnostics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let value = forge.schedule().diagnostics().await.map_err(pyerr)?;
            Ok(SchedulerDiagnostics {
                lag_ms: value.lag.map(|lag| lag.as_secs_f64() * 1000.0),
                last_successful_tick_ms: value.last_successful_tick.map(epoch_ms),
                due_count: value.due_count,
                enqueue_failures: value.enqueue_failures,
            })
        })
    }

    /// Cancel a one-shot scheduled by `schedule_at`, by the JobId it returned. `True`
    /// if it was still pending and removed, `False` otherwise.
    fn schedule_cancel_at<'py>(
        &self,
        py: Python<'py>,
        job_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .schedule()
                .cancel(&format!("at:{job_id}"))
                .await
                .map_err(pyerr)
        })
    }

    /// List registered schedules, ordered by name, up to `limit` per page (default
    /// 100) plus an opaque next-page `cursor` (`None` when done). Returns a
    /// `SchedulePage`; pass its `cursor` back for the next page.
    #[pyo3(signature = (cursor=None, limit=100))]
    fn schedule_list<'py>(
        &self,
        py: Python<'py>,
        cursor: Option<String>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            let cur = cursor.map(forge::Cursor::from_token);
            let (items, next) = forge.schedule().list(cur, limit).await.map_err(pyerr)?;
            let items: Vec<ScheduleInfo> = items.into_iter().map(schedule_info).collect();
            Ok(SchedulePage {
                items,
                cursor: next.map(|c| c.token().to_string()),
            })
        })
    }

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

    fn set_flag_value<'py>(
        &self,
        py: Python<'py>,
        key: String,
        value_json: String,
        variant: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let value = serde_json::from_str(&value_json)
            .map_err(|_| pyerr(forge::ForgeError::invalid("value_json must be valid JSON")))?;
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge
                .config()
                .set_flag(&key, FlagRule::Value { value, variant })
                .await
                .map_err(pyerr)
        })
    }

    fn delete_flag<'py>(&self, py: Python<'py>, key: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.config().delete_flag(&key).await.map_err(pyerr)
        })
    }

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

    /// Revoke an API key by its (non-secret) id. `True` if one was removed.
    fn revoke_api_key<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let forge = self.forge.clone();
        future_into_py(py, async move {
            forge.auth().revoke_api_key(&id).await.map_err(pyerr)
        })
    }
}

#[pymodule]
fn forgelib(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Own the Tokio runtime that drives every awaitable this module returns.
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    pyo3_async_runtimes::tokio::init(builder);

    m.add_class::<ForgeClient>()?;
    m.add_class::<Subscription>()?;
    m.add_class::<BlobInfo>()?;
    m.add_class::<BlobSummary>()?;
    m.add_class::<ConditionalBlobGet>()?;
    m.add_class::<MultipartUpload>()?;
    m.add_class::<MultipartPart>()?;
    m.add_class::<ProxyPresign>()?;
    m.add_class::<NativePresign>()?;
    m.add_class::<ScheduleInfo>()?;
    m.add_class::<SchedulePage>()?;
    m.add_class::<SchedulerDiagnostics>()?;
    m.add_class::<SessionInfo>()?;
    m.add_class::<ApiKeyInfo>()?;
    m.add_class::<TokenConsumption>()?;
    m.add_class::<FlagEvaluation>()?;
    m.add_class::<ConfigEntry>()?;
    m.add_class::<FlagEvaluationRequest>()?;
    m.add_class::<FlagEvaluationEntry>()?;
    m.add_class::<ConfigSnapshot>()?;
    m.add_class::<BackendInfo>()?;
    m.add_class::<BackendHealth>()?;
    m.add_class::<HealthReport>()?;
    m.add_class::<DiagnosticCheck>()?;
    m.add_class::<DiagnosticsReport>()?;
    m.add_class::<MetricSample>()?;
    m.add_class::<MigrationReport>()?;
    m.add_class::<ApiKey>()?;
    m.add_class::<Job>()?;
    m.add_class::<Decision>()?;
    m.add_class::<QueueDepth>()?;
    m.add_class::<BatchEnqueueResult>()?;
    m.add_class::<QueueStats>()?;
    m.add_class::<ScanPage>()?;
    m.add_class::<BlobListPage>()?;
    m.add_function(wrap_pyfunction!(encode_config_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(decode_config_snapshot, m)?)?;

    let py = m.py();
    m.add("ForgeError", py.get_type::<ForgeError>())?;
    m.add("NotFoundError", py.get_type::<NotFoundError>())?;
    m.add("InvalidError", py.get_type::<InvalidError>())?;
    m.add("LimitError", py.get_type::<LimitError>())?;
    m.add("PreconditionError", py.get_type::<PreconditionError>())?;
    m.add("UnavailableError", py.get_type::<UnavailableError>())?;
    m.add("ConfigError", py.get_type::<ConfigError>())?;
    m.add("NotConfiguredError", py.get_type::<NotConfiguredError>())?;
    m.add("BackendError", py.get_type::<BackendError>())?;
    Ok(())
}

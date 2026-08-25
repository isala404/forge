use futures_util::StreamExt;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

#[napi(object)]
pub struct JsBatchEnqueueItem {
    pub payload: Buffer,
    pub max_attempts: Option<u32>,
    pub dedup_id: Option<String>,
    pub delay_seconds: Option<f64>,
    pub job_id: Option<String>,
    pub priority: Option<String>,
    pub concurrency_key: Option<String>,
}

/// Optional metadata, integrity, encryption, and write-precondition controls for blobs.
#[derive(Default)]
#[napi(object)]
pub struct BlobPutOptions {
    pub content_type: Option<String>,
    pub metadata: Option<HashMap<String, String>>,
    pub create_only: Option<bool>,
    pub match_version: Option<String>,
    pub cache_control: Option<String>,
    pub content_disposition: Option<String>,
    pub checksum_sha256: Option<String>,
    pub sse_algorithm: Option<String>,
    pub sse_kms_key_id: Option<String>,
}

/// A stable, machine-readable code for each `ForgeError` variant, so JS callers can
/// branch on the failure class (prefixed onto the error message in [`err`]).
fn code_of(e: &forgelib::ForgeError) -> &'static str {
    e.code()
}

fn err(e: forgelib::ForgeError) -> napi::Error {
    napi::Error::from_reason(
        serde_json::json!({
            "forge_error": true,
            "code": code_of(&e),
            "retryable": e.is_retryable(),
            "operation": e.operation().unwrap_or("unknown"),
            "backend": e.backend_id(),
            "message": e.safe_message(),
        })
        .to_string(),
    )
}

/// Convert an `f64` seconds value into a `Duration`, raising `Invalid` on a
/// negative or non-finite input. Zero passes straight through so the core applies
/// its own validation: bindings convert and pass through, they never clamp or
/// silently coerce a caller's out-of-range value.
fn secs(field: &str, value: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| {
        err(forgelib::ForgeError::invalid(format!(
            "{field} must be a non-negative number of seconds"
        )))
    })
}

fn redrive_policy(value: &str) -> Result<forgelib::RedriveDedupPolicy> {
    match value {
        "clear" => Ok(forgelib::RedriveDedupPolicy::Clear),
        "preserve" => Ok(forgelib::RedriveDedupPolicy::Preserve),
        _ => Err(err(forgelib::ForgeError::invalid(
            "dedupPolicy must be 'clear' or 'preserve'",
        ))),
    }
}

/// Convert an `f64` byte count into a `u64`, raising `Invalid` on a negative or
/// non-finite input rather than silently coercing it. JS has no native u64, so the
/// boundary stays `f64`; the core's own 50 MiB cap covers the high end.
fn bytes(field: &str, value: f64) -> Result<u64> {
    if !value.is_finite() || value < 0.0 {
        return Err(err(forgelib::ForgeError::invalid(format!(
            "{field} must be a non-negative number of bytes"
        ))));
    }
    Ok(value as u64)
}

fn epoch_time(field: &str, value: f64) -> Result<SystemTime> {
    if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 {
        return Err(err(forgelib::ForgeError::invalid(format!(
            "{field} must be non-negative epoch milliseconds"
        ))));
    }
    Ok(UNIX_EPOCH + Duration::from_millis(value as u64))
}

fn schedule_opts(
    max_attempts: Option<u32>,
    misfire_policy: Option<String>,
    max_catch_up: Option<u32>,
) -> Result<forgelib::ScheduleOpts> {
    let mut opts = forgelib::ScheduleOpts::new();
    if let Some(m) = max_attempts {
        opts = opts.with_max_attempts(m);
    }
    let policy = match misfire_policy.as_deref().unwrap_or("run_once") {
        "skip" => forgelib::MisfirePolicy::Skip,
        "run_once" => forgelib::MisfirePolicy::RunOnce,
        "catch_up" => forgelib::MisfirePolicy::CatchUp(max_catch_up.unwrap_or(10)),
        _ => {
            return Err(err(forgelib::ForgeError::invalid(
                "misfirePolicy must be 'skip', 'run_once', or 'catch_up'",
            )));
        }
    };
    if !matches!(policy, forgelib::MisfirePolicy::CatchUp(_)) && max_catch_up.unwrap_or(0) != 0 {
        return Err(err(forgelib::ForgeError::invalid(
            "maxCatchUp is only valid with catch_up",
        )));
    }
    Ok(opts.with_misfire_policy(policy))
}

fn schedule_info(schedule: forgelib::ScheduleInfo) -> JsScheduleInfo {
    let (kind, cron_expr) = match schedule.kind {
        forgelib::ScheduleKind::Cron(expression) => ("cron".to_string(), Some(expression)),
        _ => ("at".to_string(), None),
    };
    JsScheduleInfo {
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

fn blob_put_opts(options: Option<BlobPutOptions>) -> Result<forgelib::PutOpts> {
    let options = options.unwrap_or_default();
    if options.create_only == Some(true) && options.match_version.is_some() {
        return Err(err(forgelib::ForgeError::invalid(
            "createOnly and matchVersion are mutually exclusive",
        )));
    }
    let mut opts = forgelib::PutOpts::new();
    if let Some(value) = options.content_type {
        opts = opts.with_content_type(value);
    }
    for (name, value) in options.metadata.unwrap_or_default() {
        opts = opts.with_metadata(name, value);
    }
    if let Some(value) = options.cache_control {
        opts = opts.with_cache_control(value);
    }
    if let Some(value) = options.content_disposition {
        opts = opts.with_content_disposition(value);
    }
    if let Some(value) = options.checksum_sha256 {
        opts = opts.with_checksum_sha256(value);
    }
    match options.sse_algorithm.as_deref() {
        Some("AES256") => opts = opts.with_s3_encryption(forgelib::S3Encryption::S3Managed),
        Some("aws:kms") => {
            opts = opts.with_s3_encryption(forgelib::S3Encryption::Kms {
                key_id: options.sse_kms_key_id,
            });
        }
        Some(_) => {
            return Err(err(forgelib::ForgeError::invalid(
                "sseAlgorithm must be AES256 or aws:kms",
            )));
        }
        None if options.sse_kms_key_id.is_some() => {
            return Err(err(forgelib::ForgeError::invalid(
                "sseKmsKeyId requires sseAlgorithm aws:kms",
            )));
        }
        None => {}
    }
    if options.create_only == Some(true) {
        opts = opts.create_only();
    } else if let Some(value) = options.match_version {
        opts = opts.match_version(value);
    }
    Ok(opts)
}

fn js_blob_info(info: forgelib::BlobInfo) -> JsBlobInfo {
    JsBlobInfo {
        key: info.key,
        size: info.size as f64,
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

fn core_upload(upload: JsMultipartUpload) -> forgelib::MultipartUpload {
    let precondition = if upload.create_only {
        Some(forgelib::PutPrecondition::CreateOnly)
    } else {
        upload
            .match_version
            .map(forgelib::PutPrecondition::MatchVersion)
    };
    forgelib::MultipartUpload::new(upload.key, upload.upload_id, precondition)
}

fn js_upload(upload: forgelib::MultipartUpload) -> JsMultipartUpload {
    let (create_only, match_version) = match upload.precondition {
        Some(forgelib::PutPrecondition::CreateOnly) => (true, None),
        Some(forgelib::PutPrecondition::MatchVersion(value)) => (false, Some(value)),
        None => (false, None),
        Some(_) => (false, None),
    };
    JsMultipartUpload {
        key: upload.key,
        upload_id: upload.upload_id,
        create_only,
        match_version,
    }
}

fn migration_reports(reports: Vec<forgelib::MigrationReport>) -> Vec<JsMigrationReport> {
    reports
        .into_iter()
        .map(|report| JsMigrationReport {
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

fn js_health(report: forgelib::HealthReport) -> JsHealthReport {
    JsHealthReport {
        live: report.live,
        ready: report.ready,
        checked_at_ms: report.checked_at_ms,
        duration_ms: report.duration_ms,
        backends: report
            .backends
            .into_iter()
            .map(|backend| JsBackendHealth {
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

fn js_diagnostics(report: forgelib::DiagnosticsReport) -> JsDiagnosticsReport {
    JsDiagnosticsReport {
        ready: report.ready,
        checked_at_ms: report.checked_at_ms as f64,
        checks: report
            .checks
            .into_iter()
            .map(|check| JsDiagnosticCheck {
                name: check.name,
                status: check.status,
                message: check.message,
            })
            .collect(),
    }
}

fn js_flag_evaluation(value: forgelib::FlagEvaluation) -> JsFlagEvaluation {
    JsFlagEvaluation {
        value_json: value.value_json,
        value_type: value.value_type,
        variant: value.variant,
        reason: value.reason,
        error_code: value.error_code,
    }
}

fn core_flag_request(request: JsFlagEvaluationRequest) -> Result<forgelib::FlagEvaluationRequest> {
    let default = serde_json::from_str(&request.default_json).map_err(|_| {
        err(forgelib::ForgeError::invalid(
            "defaultJson must be valid JSON",
        ))
    })?;
    let attributes = match request.context_json {
        Some(value) => {
            serde_json::from_str::<std::collections::BTreeMap<String, serde_json::Value>>(&value)
                .map_err(|_| {
                    err(forgelib::ForgeError::invalid(
                        "contextJson must be a JSON object",
                    ))
                })?
        }
        None => std::collections::BTreeMap::new(),
    };
    let mut context = request
        .targeting_key
        .map_or_else(forgelib::EvalCtx::new, forgelib::EvalCtx::user);
    for (key, value) in attributes {
        context = context.with_field(key, value);
    }
    Ok(forgelib::FlagEvaluationRequest {
        id: request.id,
        key: request.key,
        default,
        context,
    })
}

fn js_flag_evaluation_entry(value: forgelib::FlagEvaluationEntry) -> JsFlagEvaluationEntry {
    JsFlagEvaluationEntry {
        id: value.id,
        key: value.key,
        evaluation: js_flag_evaluation(value.evaluation),
    }
}

fn js_config_snapshot(value: forgelib::ConfigSnapshot) -> JsConfigSnapshot {
    JsConfigSnapshot {
        schema_version: value.schema_version,
        created_at_ms: value.created_at_ms as f64,
        expires_at_ms: value.expires_at_ms as f64,
        secret_handling: value.secret_handling,
        config: value
            .config
            .into_iter()
            .map(|entry| JsConfigEntry {
                key: entry.key,
                value: entry.value,
            })
            .collect(),
        flags: value
            .flags
            .into_iter()
            .map(js_flag_evaluation_entry)
            .collect(),
    }
}

fn core_config_snapshot(value: JsConfigSnapshot) -> Result<forgelib::ConfigSnapshot> {
    if !value.created_at_ms.is_finite()
        || !value.expires_at_ms.is_finite()
        || value.created_at_ms < 0.0
        || value.expires_at_ms < 0.0
    {
        return Err(err(forgelib::ForgeError::invalid(
            "snapshot timestamps must be non-negative finite milliseconds",
        )));
    }
    let flags = value
        .flags
        .into_iter()
        .map(|entry| {
            let evaluation = serde_json::from_value(serde_json::json!({
                "value_json": entry.evaluation.value_json,
                "value_type": entry.evaluation.value_type,
                "variant": entry.evaluation.variant,
                "reason": entry.evaluation.reason,
                "error_code": entry.evaluation.error_code,
            }))
            .map_err(|_| {
                err(forgelib::ForgeError::invalid(
                    "snapshot flag evaluation is invalid",
                ))
            })?;
            Ok(forgelib::FlagEvaluationEntry {
                id: entry.id,
                key: entry.key,
                evaluation,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(forgelib::ConfigSnapshot {
        schema_version: value.schema_version,
        created_at_ms: value.created_at_ms as u64,
        expires_at_ms: value.expires_at_ms as u64,
        secret_handling: value.secret_handling,
        config: value
            .config
            .into_iter()
            .map(|entry| forgelib::ConfigEntry {
                key: entry.key,
                value: entry.value,
            })
            .collect(),
        flags,
    })
}

fn snapshot_secret_handling(value: &str) -> Result<forgelib::SnapshotSecretHandling> {
    match value {
        "no_secrets" => Ok(forgelib::SnapshotSecretHandling::NoSecrets),
        "application_protected" => Ok(forgelib::SnapshotSecretHandling::ApplicationProtected),
        _ => Err(err(forgelib::ForgeError::invalid(
            "secretHandling must be no_secrets or application_protected",
        ))),
    }
}

fn js_metric(sample: forgelib::MetricSample) -> JsMetricSample {
    JsMetricSample {
        name: sample.name,
        kind: sample.kind,
        labels: sample.labels,
        value: sample.value,
        count: sample
            .count
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX)),
        sum: sample.sum,
    }
}

/// Map an optional algorithm name onto [`forgelib::Algo`]. `None` keeps the token-bucket
/// default; `"token_bucket"` / `"sliding_window"` select explicitly; anything else
/// is `Invalid`.
fn parse_algo(name: Option<&str>) -> Result<forgelib::Algo> {
    match name {
        None | Some("token_bucket") => Ok(forgelib::Algo::TokenBucket),
        Some("sliding_window") => Ok(forgelib::Algo::SlidingWindow),
        Some(other) => Err(err(forgelib::ForgeError::invalid(format!(
            "unknown rate-limit algo {other:?}; expected \"token_bucket\" or \"sliding_window\""
        )))),
    }
}

fn parse_priority(value: &str) -> Result<forgelib::Priority> {
    match value {
        "low" => Ok(forgelib::Priority::Low),
        "normal" => Ok(forgelib::Priority::Normal),
        "high" => Ok(forgelib::Priority::High),
        _ => Err(err(forgelib::ForgeError::invalid(
            "priority must be low, normal, or high",
        ))),
    }
}

fn parse_job_state(value: &str) -> Result<forgelib::JobState> {
    match value {
        "queued" => Ok(forgelib::JobState::Queued),
        "delayed" => Ok(forgelib::JobState::Delayed),
        "leased" => Ok(forgelib::JobState::Leased),
        "retrying" => Ok(forgelib::JobState::Retrying),
        "succeeded" => Ok(forgelib::JobState::Succeeded),
        "dead" => Ok(forgelib::JobState::Dead),
        "cancel_requested" => Ok(forgelib::JobState::CancelRequested),
        "cancelled" => Ok(forgelib::JobState::Cancelled),
        _ => Err(err(forgelib::ForgeError::invalid("unknown job state"))),
    }
}

fn job_status_json(value: &forgelib::JobStatus) -> serde_json::Value {
    let state = match value.state {
        forgelib::JobState::Queued => "queued",
        forgelib::JobState::Delayed => "delayed",
        forgelib::JobState::Leased => "leased",
        forgelib::JobState::Retrying => "retrying",
        forgelib::JobState::Succeeded => "succeeded",
        forgelib::JobState::Dead => "dead",
        forgelib::JobState::CancelRequested => "cancel_requested",
        forgelib::JobState::Cancelled => "cancelled",
    };
    serde_json::json!({"id":value.id.to_string(),"queue":value.queue,"state":state,"attemptCount":value.attempt_count,"maxAttempts":value.max_attempts,"priority":format!("{:?}",value.priority).to_ascii_lowercase(),"concurrencyKey":value.concurrency_key,"enqueuedAtMs":epoch_ms(value.enqueued_at),"availableAtMs":epoch_ms(value.available_at),"completedAtMs":value.completed_at.map(epoch_ms)})
}

fn reservation_json(value: &forgelib::Reservation) -> serde_json::Value {
    serde_json::json!({"id":value.id.to_string(),"reservedUnits":value.reserved_units,"expiresAtMs":epoch_ms(value.expires_at),"state":format!("{:?}",value.state).to_ascii_lowercase(),"committedUnits":value.committed_units})
}

// The cross-language value DTOs (JsJob, JsDecision, JsBlobInfo, …) are generated from one
// schema shared with the Python binding; see tools/codegen/src/schema.rs. napi derives
// index.d.ts from these structs. Regenerate with the codegen tool; never hand-edit.
include!("types.generated.rs");

/// Epoch milliseconds for a `SystemTime` (saturating).
fn epoch_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn js_proxy(ticket: forgelib::ProxyPresign) -> JsProxyPresign {
    JsProxyPresign {
        url: ticket.url,
        method: ticket.method,
        key: ticket.key,
        expires_epoch: ticket.expires_epoch as f64,
        max_bytes: ticket.max_bytes as f64,
        signature: ticket.signature,
        required_headers: ticket.required_headers.into_iter().collect(),
    }
}

fn js_native(ticket: forgelib::NativePresign) -> JsNativePresign {
    JsNativePresign {
        url: ticket.url,
        method: ticket.method,
        expires_epoch: ticket.expires_epoch as f64,
        required_headers: ticket.required_headers.into_iter().collect(),
        constraints: ticket.constraints.into_iter().collect(),
    }
}

/// A Forge client: one Postgres pool, every primitive. Construct with
/// `ForgeClient.init()`, which reads `forge.toml`.
#[napi]
pub struct ForgeClient {
    forge: forgelib::Forge,
    /// Leased-but-not-settled jobs, keyed by a delivery-unique opaque receipt
    /// (not the job id), so a job redelivered to this same process gets a fresh
    /// entry instead of overwriting the in-flight one. `ack`/`nack`/`heartbeat`
    /// recover the `forgelib::Job` (whose lease fence is not part of the public
    /// surface) by receipt. Entries are evicted on settle and, as a leak backstop,
    /// once their last observed lease/heartbeat has been expired for over 24h.
    leased: Arc<Mutex<HashMap<String, forgelib::Job>>>,
    /// Monotonic counter making each dequeue's receipt unique.
    seq: Arc<std::sync::atomic::AtomicU64>,
}

impl ForgeClient {
    fn from_forge(forge: forgelib::Forge) -> Self {
        Self {
            forge,
            leased: Arc::new(Mutex::new(HashMap::new())),
            seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
}

#[napi]
impl ForgeClient {
    /// Read `forge.toml` from the current directory and instantiate the runtime from it;
    /// mirrors Rust's `Forge::init`. The file is the single source of configuration; its
    /// string values may reference the environment as `${VAR}` / `${VAR:-default}`. Migrates
    /// the system database at startup.
    #[napi(factory)]
    pub async fn init() -> Result<ForgeClient> {
        let forge = forgelib::Forge::init().await.map_err(err)?;
        Ok(ForgeClient::from_forge(forge))
    }

    /// Like `init`, but reads the `forge.toml` at `path` instead of the one in the current
    /// directory.
    #[napi(factory)]
    pub async fn init_from(path: String) -> Result<ForgeClient> {
        let forge = forgelib::Forge::init_from(path).await.map_err(err)?;
        Ok(ForgeClient::from_forge(forge))
    }

    /// Parse the canonical configuration from a TOML string.
    #[napi(factory)]
    pub async fn init_from_string(toml: String) -> Result<ForgeClient> {
        let forge = forgelib::Forge::init_from_str(&toml).await.map_err(err)?;
        Ok(ForgeClient::from_forge(forge))
    }

    /// Create a memory client with a manual clock and seeded token entropy for tests.
    #[napi(factory)]
    pub async fn init_memory_for_testing(
        toml: String,
        start_ms: f64,
        seed: u32,
    ) -> Result<ForgeClient> {
        let forge = forgelib::Forge::init_memory_for_testing(
            &toml,
            epoch_time("startMs", start_ms)?,
            seed.into(),
        )
        .await
        .map_err(err)?;
        Ok(ForgeClient::from_forge(forge))
    }

    #[napi]
    pub async fn migrate() -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::migrate().await.map_err(err)?,
        ))
    }

    #[napi]
    pub async fn migrate_from(path: String) -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::migrate_from(path).await.map_err(err)?,
        ))
    }

    #[napi]
    pub async fn migrate_from_string(toml: String) -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::migrate_from_str(&toml)
                .await
                .map_err(err)?,
        ))
    }

    #[napi]
    pub async fn migration_status() -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::migration_status().await.map_err(err)?,
        ))
    }

    #[napi]
    pub async fn migration_status_from(path: String) -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::migration_status_from(path)
                .await
                .map_err(err)?,
        ))
    }

    #[napi]
    pub async fn migration_status_from_string(toml: String) -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::migration_status_from_str(&toml)
                .await
                .map_err(err)?,
        ))
    }

    #[napi]
    pub async fn validate_schema() -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::validate_schema().await.map_err(err)?,
        ))
    }

    #[napi]
    pub async fn validate_schema_from(path: String) -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::validate_schema_from(path)
                .await
                .map_err(err)?,
        ))
    }

    #[napi]
    pub async fn validate_schema_from_string(toml: String) -> Result<Vec<JsMigrationReport>> {
        Ok(migration_reports(
            forgelib::Forge::validate_schema_from_str(&toml)
                .await
                .map_err(err)?,
        ))
    }

    /// The resolved connection string of Forge's system database — the configured
    /// `[postgres] url`, or the DSN an embedded server minted at init. Contains
    /// credentials; use it to point the app's own tables/pool at the same database
    /// (the only way to reach an embedded server from outside Forge).
    #[napi]
    pub fn postgres_url(&self) -> Result<String> {
        self.forge.postgres_url().map(str::to_string).map_err(err)
    }

    /// Idempotently stop accepting work and close owned resources within the deadline.
    #[napi]
    pub async fn close(&self, timeout_seconds: Option<f64>) -> Result<()> {
        let timeout = secs("timeoutSeconds", timeout_seconds.unwrap_or(30.0))?;
        self.forge.close(timeout).await.map_err(err)
    }

    /// Move a test-factory client's manual clock forward without sleeping.
    #[napi]
    pub fn advance_test_clock(&self, seconds: f64) -> Result<()> {
        self.forge
            .advance_test_clock(secs("seconds", seconds)?)
            .map_err(err)
    }

    /// Static provider capabilities. This performs no I/O.
    #[napi]
    pub fn backend_capabilities(&self) -> Vec<JsBackendInfo> {
        self.forge
            .backend_capabilities()
            .backends
            .into_iter()
            .map(|b| JsBackendInfo {
                primitive: b.primitive.as_str().to_string(),
                provider: b.provider.to_string(),
                durable: b.durable,
                caveats: b.caveats.to_string(),
            })
            .collect()
    }

    #[napi]
    pub fn is_live(&self) -> bool {
        self.forge.is_live()
    }

    #[napi]
    pub async fn probe(
        &self,
        deadline_seconds: Option<f64>,
        readiness_backends: Option<Vec<String>>,
    ) -> Result<JsHealthReport> {
        let readiness_backends = readiness_backends
            .unwrap_or_default()
            .iter()
            .map(|value| forgelib::Primitive::parse(value).map_err(err))
            .collect::<Result<Vec<_>>>()?;
        let options = forgelib::ProbeOptions::new()
            .with_deadline(secs("deadlineSeconds", deadline_seconds.unwrap_or(2.0))?)
            .with_readiness_backends(readiness_backends);
        self.forge.probe(options).await.map(js_health).map_err(err)
    }

    #[napi]
    pub async fn diagnostics(&self, deadline_seconds: Option<f64>) -> Result<JsDiagnosticsReport> {
        self.forge
            .diagnostics(secs("deadlineSeconds", deadline_seconds.unwrap_or(2.0))?)
            .await
            .map(js_diagnostics)
            .map_err(err)
    }

    #[napi]
    pub fn metrics_snapshot(&self) -> Vec<JsMetricSample> {
        self.forge
            .metrics_snapshot()
            .into_iter()
            .map(js_metric)
            .collect()
    }

    #[napi]
    pub fn render_prometheus(&self) -> String {
        self.forge.render_prometheus()
    }

    /// `GET key` → the value as a UTF-8 string, or `null`. The string surface is
    /// UTF-8-only; use `kvGetBytes` for values that may hold arbitrary bytes.
    #[napi]
    pub async fn kv_get(&self, key: String) -> Result<Option<String>> {
        let v = self.forge.kv().get(&key).await.map_err(err)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// `GET key` → the raw value bytes, or `null`. Lossless, unlike `kvGet`.
    #[napi]
    pub async fn kv_get_bytes(&self, key: String) -> Result<Option<Buffer>> {
        let v = self.forge.kv().get(&key).await.map_err(err)?;
        Ok(v.map(|b| Buffer::from(b.to_vec())))
    }

    /// `MGET keys` → each value as a UTF-8 string (or `null` if missing/expired), in
    /// input order. One round-trip; use instead of a per-key `kvGet` loop.
    #[napi]
    pub async fn kv_mget(&self, keys: Vec<String>) -> Result<Vec<Option<String>>> {
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let vals = self.forge.kv().mget(&refs).await.map_err(err)?;
        Ok(vals
            .into_iter()
            .map(|o| o.map(|b| String::from_utf8_lossy(&b).into_owned()))
            .collect())
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
        if_exists: Option<bool>,
    ) -> Result<bool> {
        let mut opts = forgelib::SetOpts::new();
        if let Some(t) = ttl_seconds {
            opts = opts.with_ttl(secs("ttlSeconds", t)?);
        }
        // `ifExists` (XX) takes precedence over `ifNotExists` (NX) if both are set.
        if if_exists.unwrap_or(false) {
            opts = opts.with_mode(forgelib::SetMode::IfExists);
        } else if if_not_exists.unwrap_or(false) {
            opts = opts.with_mode(forgelib::SetMode::IfNotExists);
        }
        self.forge
            .kv()
            .set(&key, forgelib::Bytes::from(value), opts)
            .await
            .map_err(err)
    }

    /// `SET key` with raw value bytes (lossless, unlike `kvSet`). Returns whether
    /// the write happened.
    #[napi]
    pub async fn kv_set_bytes(
        &self,
        key: String,
        value: Buffer,
        ttl_seconds: Option<f64>,
        if_not_exists: Option<bool>,
    ) -> Result<bool> {
        let mut opts = forgelib::SetOpts::new();
        if let Some(t) = ttl_seconds {
            opts = opts.with_ttl(secs("ttlSeconds", t)?);
        }
        if if_not_exists.unwrap_or(false) {
            opts = opts.with_mode(forgelib::SetMode::IfNotExists);
        }
        self.forge
            .kv()
            .set(&key, forgelib::Bytes::from(value.to_vec()), opts)
            .await
            .map_err(err)
    }

    /// `INCRBY key by` (atomic). Returns the new value. The counter is an i64
    /// core-side, but JS numbers are f64, so a value beyond 2^53 loses precision
    /// here (the Python binding returns the exact i64). Real counters never reach
    /// that range; if yours might, read it back losslessly via `kvGetBytes`.
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

    /// `DEL key`. Returns whether the key existed.
    #[napi]
    pub async fn kv_delete(&self, key: String) -> Result<bool> {
        self.forge.kv().delete(&key).await.map_err(err)
    }

    /// `EXISTS key`. Returns whether the key is present (and unexpired).
    #[napi]
    pub async fn kv_exists(&self, key: String) -> Result<bool> {
        self.forge.kv().exists(&key).await.map_err(err)
    }

    /// Enqueue a job (string payload). Returns the job id.
    #[napi]
    #[allow(clippy::too_many_arguments)]
    pub async fn queue_enqueue(
        &self,
        queue: String,
        payload: Buffer,
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
    ) -> Result<String> {
        let mut opts = forgelib::EnqueueOpts::new();
        if let Some(m) = max_attempts {
            opts = opts.with_max_attempts(m);
        }
        if let Some(d) = dedup_id {
            opts = opts.with_dedup_id(d);
        }
        if let Some(s) = delay_seconds {
            opts = opts.with_delay(secs("delaySeconds", s)?);
        }
        if let Some(id) = job_id {
            opts = opts.with_job_id(forgelib::JobId::parse(&id).map_err(err)?);
        }
        if let Some(traceparent) = traceparent {
            opts = opts.with_trace_context(
                forgelib::TraceContext::from_headers(
                    traceparent,
                    tracestate,
                    baggage,
                    &baggage_allowlist.unwrap_or_default(),
                )
                .map_err(err)?,
            );
        } else if tracestate.is_some() || baggage.is_some() {
            return Err(err(forgelib::ForgeError::invalid(
                "traceparent is required when tracestate or baggage is set",
            )));
        }
        if let Some(priority) = priority {
            opts = opts.with_priority(parse_priority(&priority)?);
        }
        if let Some(key) = concurrency_key {
            opts = opts.with_concurrency_key(key);
        }
        let id = self
            .forge
            .queue()
            .enqueue(
                &queue,
                forgelib::Bytes::copy_from_slice(payload.as_ref()),
                opts,
            )
            .await
            .map_err(err)?;
        Ok(id.to_string())
    }

    #[napi]
    pub async fn queue_enqueue_batch(
        &self,
        queue: String,
        items: Vec<JsBatchEnqueueItem>,
    ) -> Result<Vec<JsBatchEnqueueResult>> {
        if items.is_empty() || items.len() > forgelib::MAX_ENQUEUE_BATCH {
            return Err(err(forgelib::ForgeError::limit(
                "batch enqueue size must be in 1..=100",
            )));
        }
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let result = self
                .queue_enqueue(
                    queue.clone(),
                    item.payload,
                    item.max_attempts,
                    item.dedup_id,
                    item.delay_seconds,
                    item.job_id,
                    None,
                    None,
                    None,
                    None,
                    item.priority,
                    item.concurrency_key,
                )
                .await;
            match result {
                Ok(job_id) => results.push(JsBatchEnqueueResult {
                    job_id: Some(job_id),
                    error_code: None,
                    retryable: false,
                    message: None,
                }),
                Err(error) => {
                    let parsed = serde_json::from_str::<serde_json::Value>(&error.reason).ok();
                    results.push(JsBatchEnqueueResult {
                        job_id: None,
                        error_code: parsed
                            .as_ref()
                            .and_then(|value| value.get("code"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        retryable: parsed
                            .as_ref()
                            .and_then(|value| value.get("retryable"))
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        message: parsed
                            .as_ref()
                            .and_then(|value| value.get("message"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or(Some(error.reason)),
                    });
                }
            }
        }
        Ok(results)
    }

    /// Lease one job for `visibilitySeconds`, long-polling up to `waitSeconds`.
    /// `null` if none arrived. `ack`/`nack`/`heartbeat` it by the returned `receipt`.
    #[napi]
    pub async fn queue_dequeue(
        &self,
        queue: String,
        visibility_seconds: f64,
        wait_seconds: f64,
        concurrency_limit_per_key: Option<u32>,
    ) -> Result<Option<JsJob>> {
        let mut opts = forgelib::DequeueOpts::new()
            .with_visibility_timeout(secs("visibilitySeconds", visibility_seconds)?)
            .with_wait(secs("waitSeconds", wait_seconds)?);
        if let Some(limit) = concurrency_limit_per_key {
            opts = opts.with_concurrency_limit_per_key(limit);
        }
        match self
            .forge
            .queue()
            .dequeue(&queue, opts)
            .await
            .map_err(err)?
        {
            Some(job) => {
                let receipt = format!(
                    "{}:{}",
                    job.id,
                    self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                );
                let js = JsJob {
                    id: job.id.to_string(),
                    receipt: receipt.clone(),
                    payload: Buffer::from(job.payload.to_vec()),
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
                let mut leased = self.leased.lock().await;
                // Backstop against true leaks (dequeued, never settled): drop entries
                // whose last observed lease/heartbeat lapsed over 24h ago.
                let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
                leased.retain(|_, j| j.leased_until > cutoff);
                leased.insert(receipt, job);
                Ok(Some(js))
            }
            None => Ok(None),
        }
    }

    #[napi]
    pub async fn queue_dequeue_batch(
        &self,
        queue: String,
        max_items: u32,
        visibility_seconds: f64,
        wait_seconds: f64,
        concurrency_limit_per_key: Option<u32>,
    ) -> Result<Vec<JsJob>> {
        if max_items == 0 || max_items > forgelib::MAX_DEQUEUE_BATCH as u32 {
            return Err(err(forgelib::ForgeError::limit(
                "batch dequeue size must be in 1..=10",
            )));
        }
        let mut jobs = Vec::with_capacity(max_items as usize);
        let mut wait = wait_seconds;
        for _ in 0..max_items {
            let Some(job) = self
                .queue_dequeue(
                    queue.clone(),
                    visibility_seconds,
                    wait,
                    concurrency_limit_per_key,
                )
                .await?
            else {
                break;
            };
            jobs.push(job);
            wait = 0.0;
        }
        Ok(jobs)
    }

    /// Ack a leased job by its `receipt`. Raises `PRECONDITION` if the receipt is
    /// unknown or belongs to another client/namespace.
    #[napi]
    pub async fn queue_ack(&self, receipt: String) -> Result<()> {
        let job = self.leased.lock().await.remove(&receipt);
        let Some(job) = job else {
            return Err(err(forgelib::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        self.forge.queue().ack(&job).await.map_err(err)
    }

    /// Nack a leased job by its `receipt`; optional `retrySeconds` delays the
    /// redelivery. Raises `PRECONDITION` if the receipt is unknown (the lease was
    /// lost, stop working on this job).
    #[napi]
    pub async fn queue_nack(
        &self,
        receipt: String,
        retry_seconds: Option<f64>,
        failure_summary: Option<String>,
    ) -> Result<()> {
        let job = self.leased.lock().await.remove(&receipt);
        let Some(job) = job else {
            return Err(err(forgelib::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        let mut opts = match retry_seconds {
            Some(s) => forgelib::NackOpts::retry_in(secs("retrySeconds", s)?),
            None => forgelib::NackOpts::default(),
        };
        if let Some(summary) = failure_summary {
            opts = opts.with_failure_summary(summary);
        }
        self.forge.queue().nack(&job, opts).await.map_err(err)
    }

    /// Extend the lease on a job leased by this client (SQS ChangeMessageVisibility /
    /// beanstalkd touch) by one visibility timeout. Call before `leasedUntilMs` for a
    /// handler that may outlive its visibility window, so the job is not redelivered
    /// mid-flight. Raises `PRECONDITION` if the receipt is unknown (the lease was
    /// lost, stop working on this job).
    #[napi]
    pub async fn queue_heartbeat(&self, receipt: String) -> Result<()> {
        let job = self.leased.lock().await.get(&receipt).cloned();
        let Some(job) = job else {
            return Err(err(forgelib::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        self.forge.queue().heartbeat(&job).await.map_err(err)?;
        if let Some(stored) = self.leased.lock().await.get_mut(&receipt)
            && stored.id == job.id
            && stored.lease_token() == job.lease_token()
        {
            stored.leased_until = SystemTime::now();
        }
        Ok(())
    }

    #[napi]
    pub async fn queue_cancellation_requested(&self, receipt: String) -> Result<bool> {
        let job = self.leased.lock().await.get(&receipt).cloned();
        let Some(job) = job else {
            return Err(err(forgelib::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        self.forge
            .queue()
            .cancellation_requested(&job)
            .await
            .map_err(err)
    }

    #[napi]
    pub async fn queue_finish_cancellation(&self, receipt: String) -> Result<()> {
        let job = self.leased.lock().await.remove(&receipt);
        let Some(job) = job else {
            return Err(err(forgelib::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        self.forge
            .queue()
            .finish_cancellation(&job)
            .await
            .map_err(err)
    }

    #[napi]
    pub async fn queue_cancel(&self, job_id: String) -> Result<Option<String>> {
        let id = forgelib::JobId::parse(&job_id).map_err(err)?;
        self.forge
            .queue()
            .cancel(id)
            .await
            .map_err(err)?
            .map(|status| {
                serde_json::to_string(&job_status_json(&status))
                    .map_err(|e| napi::Error::from_reason(e.to_string()))
            })
            .transpose()
    }

    #[napi]
    pub async fn queue_status(&self, job_id: String) -> Result<Option<String>> {
        let id = forgelib::JobId::parse(&job_id).map_err(err)?;
        self.forge
            .queue()
            .status(id)
            .await
            .map_err(err)?
            .map(|status| {
                serde_json::to_string(&job_status_json(&status))
                    .map_err(|e| napi::Error::from_reason(e.to_string()))
            })
            .transpose()
    }

    #[napi]
    pub async fn queue_list_status(
        &self,
        queue: Option<String>,
        states: Option<Vec<String>>,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<String> {
        let states = states
            .unwrap_or_default()
            .into_iter()
            .map(|value| parse_job_state(&value))
            .collect::<Result<Vec<_>>>()?;
        let page = self
            .forge
            .queue()
            .list_status(forgelib::JobStatusFilter {
                queue,
                states,
                cursor: cursor.map(forgelib::Cursor::from_token),
                limit: limit.unwrap_or(50),
            })
            .await
            .map_err(err)?;
        let value = serde_json::json!({"items": page.items.iter().map(job_status_json).collect::<Vec<_>>(), "cursor": page.next_cursor.map(|value| value.token().to_string())});
        serde_json::to_string(&value).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Approximate `{visible, inFlight, delayed}` counts for a queue (SQS
    /// `GetQueueAttributes`). Pass `"<queue>.dlq"` to gauge a dead-letter backlog
    /// without leasing its jobs (no side effects, unlike dequeue-to-count).
    #[napi]
    pub async fn queue_depth(&self, queue: String) -> Result<JsQueueDepth> {
        let d = self.forge.queue().depth(&queue).await.map_err(err)?;
        Ok(JsQueueDepth {
            visible: u32::try_from(d.visible).unwrap_or(u32::MAX),
            in_flight: u32::try_from(d.in_flight).unwrap_or(u32::MAX),
            delayed: u32::try_from(d.delayed).unwrap_or(u32::MAX),
            oldest_visible_age_ms: d.oldest_visible_age_ms.map(|value| value as f64),
        })
    }

    #[napi]
    pub async fn queue_pause(&self, queue: String) -> Result<()> {
        self.forge.queue().pause(&queue).await.map_err(err)
    }

    #[napi]
    pub async fn queue_resume(&self, queue: String) -> Result<()> {
        self.forge.queue().resume(&queue).await.map_err(err)
    }

    #[napi]
    pub async fn queue_is_paused(&self, queue: String) -> Result<bool> {
        self.forge.queue().is_paused(&queue).await.map_err(err)
    }

    #[napi]
    pub async fn queue_stats(&self, queue: String) -> Result<JsQueueStats> {
        let stats = self.forge.queue().stats(&queue).await.map_err(err)?;
        Ok(JsQueueStats {
            enqueued_total: u32::try_from(stats.enqueued_total).unwrap_or(u32::MAX),
            settled_total: u32::try_from(stats.settled_total).unwrap_or(u32::MAX),
            dead_total: u32::try_from(stats.dead_total).unwrap_or(u32::MAX),
            cancelled_total: u32::try_from(stats.cancelled_total).unwrap_or(u32::MAX),
            enqueue_rate_per_minute: stats.enqueue_rate_per_minute,
            settle_rate_per_minute: stats.settle_rate_per_minute,
            oldest_visible_age_ms: stats.oldest_visible_age_ms.map(|value| value as f64),
            paused: stats.paused,
        })
    }

    #[napi]
    pub async fn queue_dead_letters(
        &self,
        queue: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<JsDeadLetterPage> {
        let page = self
            .forge
            .queue()
            .dead_letters(&queue, cursor.map(forgelib::Cursor::from_token), limit)
            .await
            .map_err(err)?;
        Ok(JsDeadLetterPage {
            items: page
                .items
                .into_iter()
                .map(|item| JsDeadLetterInfo {
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
    }

    #[napi]
    pub async fn queue_redrive(
        &self,
        job_id: String,
        destination: String,
        dedup_policy: String,
    ) -> Result<bool> {
        self.forge
            .queue()
            .redrive(
                forgelib::JobId::parse(&job_id).map_err(err)?,
                forgelib::RedriveOpts::new(destination, redrive_policy(&dedup_policy)?),
            )
            .await
            .map_err(err)
    }

    #[napi]
    pub async fn queue_redrive_batch(
        &self,
        queue: String,
        cursor: Option<String>,
        limit: u32,
        destination: String,
        dedup_policy: String,
    ) -> Result<JsRedriveBatchResult> {
        let result = self
            .forge
            .queue()
            .redrive_batch(
                &queue,
                cursor.map(forgelib::Cursor::from_token),
                limit,
                forgelib::RedriveOpts::new(destination, redrive_policy(&dedup_policy)?),
            )
            .await
            .map_err(err)?;
        Ok(JsRedriveBatchResult {
            redriven: result.redriven,
            cursor: result.next_cursor.map(|value| value.token().to_string()),
        })
    }

    #[napi]
    pub async fn queue_purge_dead_letters_dry_run(&self, queue: String) -> Result<f64> {
        Ok(self
            .forge
            .queue()
            .purge_dead_letters_dry_run(&queue)
            .await
            .map_err(err)? as f64)
    }

    #[napi]
    pub async fn queue_purge_dead_letters(
        &self,
        queue: String,
        confirmation: String,
    ) -> Result<f64> {
        Ok(self
            .forge
            .queue()
            .purge_dead_letters(&queue, &confirmation)
            .await
            .map_err(err)? as f64)
    }

    #[napi]
    pub async fn run_outbox_once(
        &self,
        batch_size: Option<u32>,
        claim_seconds: Option<f64>,
        failure_backoff_seconds: Option<f64>,
        baggage_allowlist: Option<Vec<String>>,
    ) -> Result<JsOutboxRelayReport> {
        let mut opts = forgelib::OutboxRelayOpts::new();
        if let Some(value) = batch_size {
            opts = opts.with_batch_size(value);
        }
        if let Some(value) = claim_seconds {
            opts = opts.with_claim_for(secs("claimSeconds", value)?);
        }
        if let Some(value) = failure_backoff_seconds {
            opts = opts.with_failure_backoff(secs("failureBackoffSeconds", value)?);
        }
        if let Some(value) = baggage_allowlist {
            opts = opts.with_baggage_allowlist(value);
        }
        let report = self.forge.run_outbox_once(opts).await.map_err(err)?;
        Ok(JsOutboxRelayReport {
            claimed: report.claimed,
            dispatched: report.dispatched,
            failed: report.failed,
            pending: u32::try_from(report.pending).unwrap_or(u32::MAX),
            oldest_pending_age_ms: report.oldest_pending_age_ms.map(|value| value as f64),
        })
    }

    #[napi]
    pub async fn config_set(&self, key: String, value: String) -> Result<()> {
        self.forge.config().set_raw(&key, &value).await.map_err(err)
    }

    /// Resolve a config value (env `FORGE_CFG_<KEY>` > store > `null`).
    #[napi]
    pub async fn config_get(&self, key: String) -> Result<Option<String>> {
        self.forge.config().get_raw(&key).await.map_err(err)
    }

    /// Resolve up to 256 exact config keys in input order.
    #[napi]
    pub async fn config_get_many(&self, keys: Vec<String>) -> Result<Vec<JsConfigEntry>> {
        Ok(self
            .forge
            .config()
            .get_many_raw(&keys)
            .await
            .map_err(err)?
            .into_iter()
            .map(|entry| JsConfigEntry {
                key: entry.key,
                value: entry.value,
            })
            .collect())
    }

    /// Delete a stored config value. Env `FORGE_CFG_<KEY>` still shadows reads.
    #[napi]
    pub async fn config_delete(&self, key: String) -> Result<bool> {
        self.forge.config().delete_raw(&key).await.map_err(err)
    }

    /// Set a percentage-rollout flag (`0..=100`).
    #[napi]
    pub async fn set_flag_percent(&self, key: String, percent: u8) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forgelib::FlagRule::Percent(percent))
            .await
            .map_err(err)
    }

    /// Evaluate a boolean flag for `targetingKey`. Never throws; resolves to
    /// `defaultValue` on any failure.
    #[napi]
    pub async fn flag(
        &self,
        key: String,
        default_value: bool,
        targeting_key: Option<String>,
    ) -> bool {
        let ctx = match targeting_key {
            Some(k) => forgelib::EvalCtx::user(k),
            None => forgelib::EvalCtx::new(),
        };
        self.forge.config().flag(&key, default_value, &ctx).await
    }

    #[napi]
    pub async fn flag_details(
        &self,
        key: String,
        default_json: String,
        targeting_key: Option<String>,
    ) -> Result<JsFlagEvaluation> {
        let default = serde_json::from_str(&default_json).map_err(|_| {
            err(forgelib::ForgeError::invalid(
                "defaultJson must be valid JSON",
            ))
        })?;
        let ctx = targeting_key.map_or_else(forgelib::EvalCtx::new, forgelib::EvalCtx::user);
        Ok(js_flag_evaluation(
            self.forge.config().flag_details(&key, &default, &ctx).await,
        ))
    }

    /// Evaluate up to 256 typed flags in request order with one durable-backend read.
    #[napi]
    pub async fn flag_details_many(
        &self,
        requests: Vec<JsFlagEvaluationRequest>,
    ) -> Result<Vec<JsFlagEvaluationEntry>> {
        let requests = requests
            .into_iter()
            .map(core_flag_request)
            .collect::<Result<Vec<_>>>()?;
        Ok(self
            .forge
            .config()
            .flag_details_many(&requests)
            .await
            .map_err(err)?
            .into_iter()
            .map(js_flag_evaluation_entry)
            .collect())
    }

    /// Capture an expiring, read-only view of only the requested config and flags.
    #[napi]
    pub async fn config_snapshot(
        &self,
        config_keys: Vec<String>,
        flag_requests: Vec<JsFlagEvaluationRequest>,
        max_stale_seconds: f64,
        secret_handling: String,
    ) -> Result<JsConfigSnapshot> {
        let flag_requests = flag_requests
            .into_iter()
            .map(core_flag_request)
            .collect::<Result<Vec<_>>>()?;
        let snapshot = self
            .forge
            .config()
            .snapshot(
                &config_keys,
                &flag_requests,
                secs("maxStaleSeconds", max_stale_seconds)?,
                snapshot_secret_handling(&secret_handling)?,
            )
            .await
            .map_err(err)?;
        Ok(js_config_snapshot(snapshot))
    }

    /// Validate and encode a portable config snapshot without backend I/O.
    #[napi]
    pub fn encode_config_snapshot(&self, snapshot: JsConfigSnapshot) -> Result<Buffer> {
        Ok(core_config_snapshot(snapshot)?
            .encode()
            .map_err(err)?
            .into())
    }

    /// Decode and validate a portable config snapshot without backend I/O.
    #[napi]
    pub fn decode_config_snapshot(&self, encoded: Buffer) -> Result<JsConfigSnapshot> {
        Ok(js_config_snapshot(
            forgelib::ConfigSnapshot::decode(&encoded).map_err(err)?,
        ))
    }

    /// Atomic check-and-consume: `max` per `perSeconds`.
    /// `failOpen` overrides what happens on a backend error: omit for the instance
    /// default, `true` to allow, `false` to deny. `algo` selects the algorithm:
    /// `"token_bucket"` (default) or `"sliding_window"`.
    #[napi]
    // N-API exposes scalar arguments so JavaScript callers keep an idiomatic flat method.
    #[allow(clippy::too_many_arguments)]
    pub async fn rate_limit_check(
        &self,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
        fail_open: Option<bool>,
        algo: Option<String>,
        cost: Option<u32>,
    ) -> Result<JsDecision> {
        let algo = parse_algo(algo.as_deref())?;
        let limit =
            forgelib::Limit::per_duration(max, secs("perSeconds", per_seconds)?).with_algo(algo);
        let fm = match fail_open {
            None => forgelib::FailMode::Default,
            Some(true) => forgelib::FailMode::Open,
            Some(false) => forgelib::FailMode::Closed,
        };
        let d = self
            .forge
            .ratelimit()
            .check_cost_with(&bucket, &key, limit, cost.unwrap_or(1), fm)
            .await
            .map_err(err)?;
        Ok(JsDecision {
            allowed: d.allowed,
            limit: d.limit,
            remaining: d.remaining,
            reset_after_seconds: d.reset_after.as_secs_f64(),
            retry_after_seconds: d.retry_after.map(|x| x.as_secs_f64()),
        })
    }

    #[napi]
    // N-API exposes scalar arguments so JavaScript callers keep an idiomatic flat method.
    #[allow(clippy::too_many_arguments)]
    pub async fn rate_limit_reserve(
        &self,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
        cost: u32,
        ttl_seconds: f64,
        algo: Option<String>,
    ) -> Result<Option<String>> {
        let limit = forgelib::Limit::per_duration(max, secs("perSeconds", per_seconds)?)
            .with_algo(parse_algo(algo.as_deref())?);
        self.forge
            .ratelimit()
            .reserve(&bucket, &key, limit, cost, secs("ttlSeconds", ttl_seconds)?)
            .await
            .map_err(err)?
            .map(|value| {
                serde_json::to_string(&reservation_json(&value))
                    .map_err(|e| napi::Error::from_reason(e.to_string()))
            })
            .transpose()
    }

    #[napi]
    pub async fn rate_limit_commit(
        &self,
        reservation_id: String,
        actual_units: u32,
    ) -> Result<String> {
        let id = forgelib::parse_reservation_id(&reservation_id).map_err(err)?;
        let value = self
            .forge
            .ratelimit()
            .commit(id, actual_units)
            .await
            .map_err(err)?;
        serde_json::to_string(&reservation_json(&value))
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn rate_limit_release(&self, reservation_id: String) -> Result<String> {
        let id = forgelib::parse_reservation_id(&reservation_id).map_err(err)?;
        let value = self.forge.ratelimit().release(id).await.map_err(err)?;
        serde_json::to_string(&reservation_json(&value))
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn blob_put(
        &self,
        key: String,
        data: String,
        content_type: Option<String>,
    ) -> Result<()> {
        let mut opts = forgelib::PutOpts::new();
        if let Some(ct) = content_type {
            opts = opts.with_content_type(ct);
        }
        self.forge
            .blob()
            .put(&key, forgelib::Bytes::from(data), opts)
            .await
            .map_err(err)
    }

    #[napi]
    pub async fn blob_put_bytes(
        &self,
        key: String,
        data: Buffer,
        content_type: Option<String>,
    ) -> Result<()> {
        let mut opts = forgelib::PutOpts::new();
        if let Some(ct) = content_type {
            opts = opts.with_content_type(ct);
        }
        self.forge
            .blob()
            .put(&key, forgelib::Bytes::from(data.to_vec()), opts)
            .await
            .map_err(err)
    }

    /// Fetch an object as a UTF-8 string, or `null`.
    #[napi]
    pub async fn blob_get(&self, key: String) -> Result<Option<String>> {
        let v = self.forge.blob().get(&key).await.map_err(err)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Fetch an object as raw bytes, or `null`.
    #[napi]
    pub async fn blob_get_bytes(&self, key: String) -> Result<Option<Buffer>> {
        let v = self.forge.blob().get(&key).await.map_err(err)?;
        Ok(v.map(|b| Buffer::from(b.to_vec())))
    }

    #[napi]
    pub async fn blob_get_if(
        &self,
        key: String,
        if_match: Option<String>,
        if_none_match: Option<String>,
    ) -> Result<JsConditionalBlobGet> {
        let result = self
            .forge
            .blob()
            .get_if(&key, if_match.as_deref(), if_none_match.as_deref())
            .await
            .map_err(err)?;
        Ok(match result {
            forgelib::ConditionalGet::Missing => JsConditionalBlobGet {
                state: "missing".to_string(),
                body: None,
                etag: None,
            },
            forgelib::ConditionalGet::NotModified { etag } => JsConditionalBlobGet {
                state: "not_modified".to_string(),
                body: None,
                etag: Some(etag),
            },
            forgelib::ConditionalGet::Found { body, etag } => JsConditionalBlobGet {
                state: "found".to_string(),
                body: Some(Buffer::from(body.to_vec())),
                etag: Some(etag),
            },
            _ => return Err(napi::Error::from_reason("unknown conditional blob state")),
        })
    }

    /// A presigned download URL (needs a `signingSecret` at connect).
    #[napi]
    pub async fn blob_presign_download(
        &self,
        key: String,
        expires_seconds: f64,
    ) -> Result<JsProxyPresign> {
        self.forge
            .blob()
            .presign_download(&key, secs("expiresSeconds", expires_seconds)?)
            .await
            .map(js_proxy)
            .map_err(err)
    }

    /// A presigned upload (PUT) URL, capped at `maxBytes` (needs a `signingSecret`).
    #[napi]
    pub async fn blob_presign_upload(
        &self,
        key: String,
        expires_seconds: f64,
        max_bytes: f64,
    ) -> Result<JsProxyPresign> {
        self.forge
            .blob()
            .presign_upload(
                &key,
                secs("expiresSeconds", expires_seconds)?,
                bytes("maxBytes", max_bytes)?,
            )
            .await
            .map(js_proxy)
            .map_err(err)
    }

    /// Native S3 presigned GET. Its URL is a bearer credential; do not log its query.
    #[napi]
    pub async fn blob_presign_native_get(
        &self,
        key: String,
        expires_seconds: f64,
    ) -> Result<JsNativePresign> {
        self.forge
            .blob()
            .presign_native_get(&key, secs("expiresSeconds", expires_seconds)?)
            .await
            .map(js_native)
            .map_err(err)
    }

    /// Native S3 presigned PUT. No portable maximum-body-size guarantee is provided.
    #[napi]
    pub async fn blob_presign_native_put(
        &self,
        key: String,
        expires_seconds: f64,
        options: Option<BlobPutOptions>,
    ) -> Result<JsNativePresign> {
        let opts = blob_put_opts(options)?;
        self.forge
            .blob()
            .presign_native_put(&key, secs("expiresSeconds", expires_seconds)?, opts)
            .await
            .map(js_native)
            .map_err(err)
    }

    /// Verify a presigned URL's query params (needs a `signingSecret`). Returns
    /// `true` iff the signature is valid and the URL has not expired; `false` for a
    /// bad signature or an expired URL. Throws on no signing secret or a bad method.
    #[napi]
    pub async fn blob_verify_presign(
        &self,
        method: String,
        key: String,
        expires_epoch: f64,
        max_bytes: f64,
        sig: String,
    ) -> Result<bool> {
        self.forge
            .blob()
            .verify_presigned(
                &method,
                &key,
                expires_epoch as i64,
                bytes("maxBytes", max_bytes)?,
                &sig,
            )
            .await
            .map_err(err)
    }

    /// The stored content type for an object, or `null` if it does not exist.
    #[napi]
    pub async fn blob_content_type(&self, key: String) -> Result<Option<String>> {
        Ok(self
            .forge
            .blob()
            .head(&key)
            .await
            .map_err(err)?
            .map(|i| i.content_type))
    }

    /// Idempotently delete an object.
    #[napi]
    pub async fn blob_delete(&self, key: String) -> Result<()> {
        self.forge.blob().delete(&key).await.map_err(err)
    }

    /// argon2id hash of `plain` (a PHC string), to store in your users table.
    #[napi]
    pub async fn hash_password(&self, plain: String) -> Result<String> {
        let h = self.forge.auth().hash_password(&plain).await.map_err(err)?;
        Ok(h.as_str().to_string())
    }

    /// Constant-time verify of `plain` against a stored PHC `hash`.
    #[napi]
    pub async fn verify_password(&self, plain: String, hash: String) -> Result<bool> {
        self.forge
            .auth()
            .verify_password(&plain, &forgelib::PhcString::new(hash))
            .await
            .map_err(err)
    }

    /// Whether a stored PHC `hash` should be re-hashed (its argon2id params are below
    /// the current Forge baseline). Call after a successful `verifyPassword`; if `true`,
    /// re-hash the plaintext and persist it. Transparent upgrade, no forced reset.
    #[napi]
    pub fn needs_rehash(&self, hash: String) -> bool {
        self.forge
            .auth()
            .needs_rehash(&forgelib::PhcString::new(hash))
    }

    /// Create a session for `userId`; returns the opaque token (shown once).
    /// Optional sliding `idleSeconds` and hard `absoluteSeconds` deadlines.
    #[napi]
    pub async fn create_session(
        &self,
        user_id: String,
        idle_seconds: Option<f64>,
        absolute_seconds: Option<f64>,
    ) -> Result<String> {
        let mut opts = forgelib::SessionOpts::new();
        if let Some(s) = idle_seconds {
            opts = opts.with_idle_timeout(secs("idleSeconds", s)?);
        }
        if let Some(s) = absolute_seconds {
            opts = opts.with_absolute_timeout(secs("absoluteSeconds", s)?);
        }
        let t = self
            .forge
            .auth()
            .create_session(&user_id, opts)
            .await
            .map_err(err)?;
        Ok(t.as_str().to_string())
    }

    /// Validate a session token; returns the `userId`, or `null`.
    #[napi]
    pub async fn validate_session(&self, token: String) -> Result<Option<String>> {
        Ok(self
            .forge
            .auth()
            .validate_session(&token)
            .await
            .map_err(err)?
            .map(|s| s.user_id))
    }

    /// Revoke a single session by token (log out this device). Idempotent.
    #[napi]
    pub async fn revoke_session(&self, token: String) -> Result<()> {
        self.forge.auth().revoke_session(&token).await.map_err(err)
    }

    /// Revoke every session for `userId` (log out everywhere). Returns the count.
    #[napi]
    pub async fn revoke_all_sessions(&self, user_id: String) -> Result<u32> {
        let n = self
            .forge
            .auth()
            .revoke_all_sessions(&user_id)
            .await
            .map_err(err)?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Mint an `fk_` API key for `ownerId`; the `secret` is shown once.
    #[napi]
    pub async fn create_api_key(&self, owner_id: String, label: String) -> Result<JsApiKey> {
        let k = self
            .forge
            .auth()
            .create_api_key(&owner_id, &label)
            .await
            .map_err(err)?;
        Ok(JsApiKey {
            id: k.id,
            secret: k.secret.as_str().to_string(),
            label: k.label,
            created_at_ms: epoch_ms(k.created_at),
            expires_at_ms: k.expires_at.map(epoch_ms),
            scopes: k.scopes,
            metadata: k.metadata,
        })
    }

    /// Mint a bounded, optionally expiring API key with application-owned scopes and metadata.
    #[napi]
    pub async fn create_api_key_with(
        &self,
        owner_id: String,
        label: String,
        expires_in_seconds: Option<f64>,
        scopes: Option<Vec<String>>,
        metadata: Option<HashMap<String, String>>,
    ) -> Result<JsApiKey> {
        let mut opts = forgelib::ApiKeyOpts::new()
            .with_scopes(scopes.unwrap_or_default())
            .with_metadata(metadata.unwrap_or_default());
        if let Some(value) = expires_in_seconds {
            opts = opts.with_expires_in(secs("expiresInSeconds", value)?);
        }
        let k = self
            .forge
            .auth()
            .create_api_key_with(&owner_id, &label, opts)
            .await
            .map_err(err)?;
        Ok(JsApiKey {
            id: k.id,
            secret: k.secret.as_str().to_string(),
            label: k.label,
            created_at_ms: epoch_ms(k.created_at),
            expires_at_ms: k.expires_at.map(epoch_ms),
            scopes: k.scopes,
            metadata: k.metadata,
        })
    }

    /// Verify an API key; returns its full non-secret metadata, or `null`.
    #[napi]
    pub async fn verify_api_key(&self, key: String) -> Result<Option<JsApiKeyInfo>> {
        Ok(self
            .forge
            .auth()
            .verify_api_key(&key)
            .await
            .map_err(err)?
            .map(|i| JsApiKeyInfo {
                id: i.id,
                owner_id: i.owner_id,
                label: i.label,
                expires_at_ms: i.expires_at.map(epoch_ms),
                scopes: i.scopes,
                metadata: i.metadata,
            }))
    }

    /// Mint a single-use token scoped to `purpose` (e.g. `"password-reset"`), expiring
    /// after `ttlSeconds`; returns the opaque token (shown once). Deliver it out of
    /// band (email link, SMS); Forge does not send anything.
    #[napi]
    pub async fn create_token(
        &self,
        user_id: String,
        purpose: String,
        ttl_seconds: f64,
        payload: Option<Buffer>,
    ) -> Result<String> {
        let t = self
            .forge
            .auth()
            .create_token_with_payload(
                &user_id,
                &purpose,
                secs("ttlSeconds", ttl_seconds)?,
                payload
                    .map(|value| forgelib::Bytes::copy_from_slice(value.as_ref()))
                    .unwrap_or_default(),
            )
            .await
            .map_err(err)?;
        Ok(t.as_str().to_string())
    }

    #[napi]
    pub async fn create_token_with_payload(
        &self,
        user_id: String,
        purpose: String,
        ttl_seconds: f64,
        payload: Buffer,
    ) -> Result<String> {
        let token = self
            .forge
            .auth()
            .create_token_with_payload(
                &user_id,
                &purpose,
                secs("ttlSeconds", ttl_seconds)?,
                forgelib::Bytes::copy_from_slice(payload.as_ref()),
            )
            .await
            .map_err(err)?;
        Ok(token.as_str().to_string())
    }

    /// Atomically consume a token minted for `purpose`; returns its `userId`, or `null`
    /// when unknown/expired/already consumed. A live token presented with the wrong
    /// `purpose` is left intact.
    #[napi]
    pub async fn consume_token(
        &self,
        token: String,
        purpose: String,
    ) -> Result<Option<JsTokenConsumption>> {
        Ok(self
            .forge
            .auth()
            .consume_token_with_payload(&token, &purpose)
            .await
            .map_err(err)?
            .map(|value| JsTokenConsumption {
                user_id: value.user_id,
                payload: value.payload.to_vec().into(),
            }))
    }

    #[napi]
    pub async fn consume_token_with_payload(
        &self,
        token: String,
        purpose: String,
    ) -> Result<Option<JsTokenConsumption>> {
        Ok(self
            .forge
            .auth()
            .consume_token_with_payload(&token, &purpose)
            .await
            .map_err(err)?
            .map(|value| JsTokenConsumption {
                user_id: value.user_id,
                payload: value.payload.to_vec().into(),
            }))
    }

    /// Schedule a one-shot enqueue at `whenEpochMs`; returns the future JobId.
    #[napi]
    pub async fn schedule_at(
        &self,
        when_epoch_ms: f64,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
        misfire_policy: Option<String>,
        max_catch_up: Option<u32>,
    ) -> Result<String> {
        let when = UNIX_EPOCH + Duration::from_millis(when_epoch_ms.max(0.0) as u64);
        let id = self
            .forge
            .schedule()
            .at(
                when,
                &queue,
                forgelib::Bytes::from(payload),
                schedule_opts(max_attempts, misfire_policy, max_catch_up)?,
            )
            .await
            .map_err(err)?;
        Ok(id.to_string())
    }

    /// Upsert a recurring cron schedule by name. `maxAttempts` overrides the delivery
    /// attempts of the job each tick enqueues (omit for the queue default of 5).
    #[napi]
    #[allow(clippy::too_many_arguments)]
    pub async fn schedule_cron(
        &self,
        name: String,
        expr: String,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
        misfire_policy: Option<String>,
        max_catch_up: Option<u32>,
    ) -> Result<()> {
        self.forge
            .schedule()
            .cron(
                &name,
                &expr,
                &queue,
                forgelib::Bytes::from(payload),
                schedule_opts(max_attempts, misfire_policy, max_catch_up)?,
            )
            .await
            .map_err(err)
    }

    /// Fire all due schedules once; returns how many jobs were enqueued. Run on an
    /// interval (e.g. every 30s) to drive the scheduler from Node.
    #[napi]
    pub async fn run_scheduler_once(&self) -> Result<u32> {
        let n = self.forge.run_scheduler_once().await.map_err(err)?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Run periodic housekeeping once: sweep expired kv keys, settled/dead queue
    /// jobs, stale ratelimit buckets, and expired sessions. Call on an interval
    /// alongside `runSchedulerOnce` (mirrors the Rust `Forge::maintain` loop).
    #[napi]
    pub async fn maintain(&self) -> Result<()> {
        self.forge.maintain().await.map_err(err)
    }

    /// Publish a payload to a realtime topic (fire-and-forget).
    #[napi]
    pub async fn pubsub_publish(&self, topic: String, payload: String) -> Result<()> {
        self.forge
            .pubsub()
            .publish(&topic, forgelib::Bytes::from(payload))
            .await
            .map_err(err)
    }

    /// Subscribe to a realtime topic, returning a handle whose `next()` yields each
    /// payload published after this resolves (or `null` when the stream ends).
    /// Subscriptions share one per-process listener connection; drop the handle to
    /// unsubscribe (the channel is released once it has no remaining subscribers).
    #[napi]
    pub async fn pubsub_subscribe(&self, topic: String) -> Result<JsSubscription> {
        let sub = self.forge.pubsub().subscribe(&topic).await.map_err(err)?;
        let (closed_tx, _) = tokio::sync::watch::channel(false);
        Ok(JsSubscription {
            inner: Arc::new(Mutex::new(sub)),
            closed_tx: Arc::new(closed_tx),
        })
    }

    /// The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. `LISTEN` on this with
    /// a native Postgres client to receive what `pubsub_publish(topic, …)` sends.
    #[napi]
    pub fn pubsub_channel(&self, topic: String) -> Result<String> {
        self.forge.pubsub().channel_for(&topic).map_err(err)
    }

    /// `EXPIRE key ttlSeconds`. Sets/replaces the TTL on a live key; `false` if absent.
    #[napi]
    pub async fn kv_expire(&self, key: String, ttl_seconds: f64) -> Result<bool> {
        self.forge
            .kv()
            .expire(&key, secs("ttlSeconds", ttl_seconds)?)
            .await
            .map_err(err)
    }

    /// Atomic compare-and-swap (string values). Writes `newValue` iff the current value
    /// equals `old` (`old` omitted means "expected absent/expired"). Returns success.
    #[napi]
    pub async fn kv_compare_and_swap(
        &self,
        key: String,
        old: Option<String>,
        new_value: String,
    ) -> Result<bool> {
        self.forge
            .kv()
            .compare_and_swap(
                &key,
                old.map(forgelib::Bytes::from),
                forgelib::Bytes::from(new_value),
            )
            .await
            .map_err(err)
    }

    /// `SCAN prefix*` with cursor pagination. Pass `cursor` from the previous page
    /// (omit for the first); the returned `cursor` is `null` when iteration is done.
    #[napi]
    pub async fn kv_scan_page(
        &self,
        prefix: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<JsScanPage> {
        let cur = cursor.map(forgelib::Cursor::from_token);
        let (keys, next) = self
            .forge
            .kv()
            .scan(&prefix, cur, limit)
            .await
            .map_err(err)?;
        Ok(JsScanPage {
            keys,
            cursor: next.map(|c| c.token().to_string()),
        })
    }

    /// `HeadObject`: full metadata (size, content type, etag, last-modified, user
    /// metadata), or `null` if the object does not exist.
    #[napi]
    pub async fn blob_head(&self, key: String) -> Result<Option<JsBlobInfo>> {
        Ok(self
            .forge
            .blob()
            .head(&key)
            .await
            .map_err(err)?
            .map(js_blob_info))
    }

    #[napi]
    pub async fn blob_copy(
        &self,
        source: String,
        destination: String,
        options: Option<BlobPutOptions>,
    ) -> Result<JsBlobInfo> {
        let opts = blob_put_opts(options)?;
        self.forge
            .blob()
            .copy(&source, &destination, opts)
            .await
            .map(js_blob_info)
            .map_err(err)
    }

    #[napi]
    pub async fn blob_create_multipart(
        &self,
        key: String,
        options: Option<BlobPutOptions>,
    ) -> Result<JsMultipartUpload> {
        let opts = blob_put_opts(options)?;
        self.forge
            .blob()
            .create_multipart(&key, opts)
            .await
            .map(js_upload)
            .map_err(err)
    }

    #[napi]
    pub async fn blob_upload_part(
        &self,
        upload: JsMultipartUpload,
        part_number: u32,
        body: Buffer,
    ) -> Result<JsMultipartPart> {
        self.forge
            .blob()
            .upload_part(
                &core_upload(upload),
                part_number,
                forgelib::Bytes::from(body.to_vec()),
            )
            .await
            .map(|part| JsMultipartPart {
                part_number: part.part_number,
                etag: part.etag,
                size: part.size as f64,
            })
            .map_err(err)
    }

    #[napi]
    pub async fn blob_complete_multipart(
        &self,
        upload: JsMultipartUpload,
        parts: Vec<JsMultipartPart>,
    ) -> Result<JsBlobInfo> {
        let parts = parts
            .into_iter()
            .map(|part| {
                Ok(forgelib::MultipartPart::new(
                    part.part_number,
                    part.etag,
                    bytes("part.size", part.size)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        self.forge
            .blob()
            .complete_multipart(&core_upload(upload), parts)
            .await
            .map(js_blob_info)
            .map_err(err)
    }

    #[napi]
    pub async fn blob_abort_multipart(&self, upload: JsMultipartUpload) -> Result<()> {
        self.forge
            .blob()
            .abort_multipart(&core_upload(upload))
            .await
            .map_err(err)
    }

    #[napi]
    pub async fn blob_verify_checksum_sha256(
        &self,
        key: String,
        expected_hex: String,
    ) -> Result<bool> {
        self.forge
            .blob()
            .verify_checksum_sha256(&key, &expected_hex)
            .await
            .map_err(err)
    }

    /// `ListObjectsV2`: up to `limit` objects under `prefix`, lexicographic, with cursor
    /// pagination. Pass `cursor` from the previous page (omit for the first).
    #[napi]
    pub async fn blob_list(
        &self,
        prefix: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<JsBlobPage> {
        let cur = cursor.map(forgelib::Cursor::from_token);
        let page = self
            .forge
            .blob()
            .list(&prefix, cur, limit)
            .await
            .map_err(err)?;
        Ok(JsBlobPage {
            items: page
                .items
                .into_iter()
                .map(|i| JsBlobSummary {
                    key: i.key,
                    size: i.size as f64,
                    etag: i.etag,
                    last_modified_ms: epoch_ms(i.last_modified),
                })
                .collect(),
            cursor: page.next.map(|c| c.token().to_string()),
        })
    }

    /// Store an object (binary body) with optional content type and user metadata.
    #[napi]
    pub async fn blob_put_object(
        &self,
        key: String,
        data: Buffer,
        options: Option<BlobPutOptions>,
    ) -> Result<()> {
        let opts = blob_put_opts(options)?;
        self.forge
            .blob()
            .put(&key, forgelib::Bytes::from(data.to_vec()), opts)
            .await
            .map_err(err)
    }

    /// Stream a file into the configured blob backend without loading it into JS memory.
    #[napi]
    pub async fn blob_put_file(
        &self,
        key: String,
        path: String,
        options: Option<BlobPutOptions>,
    ) -> Result<()> {
        let file = tokio::fs::File::open(&path).await.map_err(|error| {
            napi::Error::from_reason(format!("could not open blob input file: {error}"))
        })?;
        let size = file
            .metadata()
            .await
            .map_err(|error| {
                napi::Error::from_reason(format!("could not stat blob input file: {error}"))
            })?
            .len();
        let opts = blob_put_opts(options)?;
        self.forge
            .blob()
            .put_stream(&key, Box::pin(file), size, opts)
            .await
            .map_err(err)
    }

    /// Fetch an inclusive byte range.
    #[napi]
    pub async fn blob_get_range(
        &self,
        key: String,
        start: f64,
        end: f64,
    ) -> Result<Option<Buffer>> {
        let start = bytes("start", start)?;
        let end = bytes("end", end)?;
        self.forge
            .blob()
            .get_range(&key, start, end)
            .await
            .map(|value| value.map(|body| Buffer::from(body.to_vec())))
            .map_err(err)
    }

    /// Cancel a schedule by name. `true` if one was removed, `false` if none existed.
    #[napi]
    pub async fn schedule_cancel(&self, name: String) -> Result<bool> {
        self.forge.schedule().cancel(&name).await.map_err(err)
    }

    #[napi]
    pub async fn schedule_inspect(&self, name: String) -> Result<Option<JsScheduleInfo>> {
        self.forge
            .schedule()
            .inspect(&name)
            .await
            .map(|value| value.map(schedule_info))
            .map_err(err)
    }

    #[napi]
    pub async fn schedule_pause(&self, name: String) -> Result<bool> {
        self.forge.schedule().pause(&name).await.map_err(err)
    }

    #[napi]
    pub async fn schedule_resume(&self, name: String) -> Result<bool> {
        self.forge.schedule().resume(&name).await.map_err(err)
    }

    #[napi]
    pub async fn scheduler_diagnostics(&self) -> Result<JsSchedulerDiagnostics> {
        let value = self.forge.schedule().diagnostics().await.map_err(err)?;
        Ok(JsSchedulerDiagnostics {
            lag_ms: value.lag.map(|lag| lag.as_secs_f64() * 1000.0),
            last_successful_tick_ms: value.last_successful_tick.map(epoch_ms),
            due_count: u32::try_from(value.due_count).unwrap_or(u32::MAX),
            enqueue_failures: u32::try_from(value.enqueue_failures).unwrap_or(u32::MAX),
        })
    }

    /// Cancel a one-shot scheduled by `scheduleAt`, by the JobId it returned. `true`
    /// if it was still pending and removed, `false` if it already fired or never
    /// existed. (Send-later recall / disappearing-message cancellation.)
    #[napi]
    pub async fn schedule_cancel_at(&self, job_id: String) -> Result<bool> {
        self.forge
            .schedule()
            .cancel(&format!("at:{job_id}"))
            .await
            .map_err(err)
    }

    /// List registered schedules (crons and pending one-shots), ordered by name, up
    /// to `limit` per page (default 100) plus an opaque next-page `cursor` (`null`
    /// when done). Pass the returned `cursor` back to page through a large backlog.
    #[napi]
    pub async fn schedule_list(
        &self,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<JsSchedulePage> {
        let cur = cursor.map(forgelib::Cursor::from_token);
        let (items, next) = self
            .forge
            .schedule()
            .list(cur, limit.unwrap_or(100))
            .await
            .map_err(err)?;
        let items = items.into_iter().map(schedule_info).collect();
        Ok(JsSchedulePage {
            items,
            cursor: next.map(|c| c.token().to_string()),
        })
    }

    /// Set a flag to always-on.
    #[napi]
    pub async fn set_flag_on(&self, key: String) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forgelib::FlagRule::On)
            .await
            .map_err(err)
    }

    /// Set a flag to always-off.
    #[napi]
    pub async fn set_flag_off(&self, key: String) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forgelib::FlagRule::Off)
            .await
            .map_err(err)
    }

    /// Set a flag to an allow-list of targeting keys.
    #[napi]
    pub async fn set_flag_allow_list(&self, key: String, entries: Vec<String>) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forgelib::FlagRule::AllowList(entries))
            .await
            .map_err(err)
    }

    #[napi]
    pub async fn set_flag_value(
        &self,
        key: String,
        value_json: String,
        variant: String,
    ) -> Result<()> {
        let value = serde_json::from_str(&value_json).map_err(|_| {
            err(forgelib::ForgeError::invalid(
                "valueJson must be valid JSON",
            ))
        })?;
        self.forge
            .config()
            .set_flag(&key, forgelib::FlagRule::Value { value, variant })
            .await
            .map_err(err)
    }

    /// Delete a flag rule. Later `flag` calls fall back to their caller default.
    #[napi]
    pub async fn delete_flag(&self, key: String) -> Result<bool> {
        self.forge.config().delete_flag(&key).await.map_err(err)
    }

    /// Validate a session token; returns full session metadata (user id + times), or
    /// `null`. Use `validateSession` when only the user id is needed.
    #[napi]
    pub async fn validate_session_info(&self, token: String) -> Result<Option<JsSession>> {
        Ok(self
            .forge
            .auth()
            .validate_session(&token)
            .await
            .map_err(err)?
            .map(|s| JsSession {
                user_id: s.user_id,
                created_at_ms: epoch_ms(s.created_at),
                expires_at_ms: epoch_ms(s.expires_at),
            }))
    }

    /// Revoke an API key by its (non-secret) id. `true` if one was removed.
    #[napi]
    pub async fn revoke_api_key(&self, id: String) -> Result<bool> {
        self.forge.auth().revoke_api_key(&id).await.map_err(err)
    }
}

/// A live pubsub subscription handle. Drive it as a JS async iterator: call `next()`
/// in a loop until it resolves to `null` (the stream ended). Dropping the handle
/// unsubscribes (subscriptions share one per-process listener connection).
#[napi]
pub struct JsSubscription {
    inner: Arc<Mutex<forgelib::Subscription>>,
    /// Flipped by `close`. A `watch` channel rather than the mutex, so `close` can
    /// interrupt a `next` that is parked on the stream while holding the lock —
    /// with only the mutex, `close` deadlocked until the next message arrived.
    closed_tx: Arc<tokio::sync::watch::Sender<bool>>,
}

#[napi]
impl JsSubscription {
    /// The next published payload as raw bytes, or `null` when the stream ends.
    #[napi]
    pub async fn next(&self) -> Result<Option<Buffer>> {
        let mut closed_rx = self.closed_tx.subscribe();
        let mut inner = self.inner.lock().await;
        tokio::select! {
            // `wait_for` is level-triggered: it also returns when close() already ran.
            _ = closed_rx.wait_for(|closed| *closed) => Ok(None),
            item = inner.next() => match item {
                Some(Ok(b)) => Ok(Some(Buffer::from(b.to_vec()))),
                Some(Err(e)) => Err(err(e)),
                None => Ok(None),
            },
        }
    }

    /// Unsubscribe now, releasing the broadcast receiver deterministically instead
    /// of waiting for GC. Any pending `next()` resolves to `null` immediately;
    /// idempotent; subsequent `next()` calls return `null`. A GraphQL server should
    /// call this when a client's WebSocket closes.
    #[napi]
    pub async fn close(&self) {
        let _ = self.closed_tx.send(true);
        // A parked next() wakes on the send and releases the lock promptly.
        let mut inner = self.inner.lock().await;
        *inner = futures_util::stream::empty().boxed();
    }
}

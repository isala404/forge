use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use tracing::Instrument as _;

use crate::auth::{
    ApiKey, ApiKeyInfo, ApiKeyOpts, Auth, OneTimeToken, PhcString, Session, SessionOpts,
    SessionToken, TokenConsumption,
};
use crate::blob::{
    Blob, BlobInfo, BlobReader, ConditionalGet, ListPage, MultipartPart, MultipartUpload,
    NativePresign, ProxyPresign, PutOpts,
};
use crate::config_store::{ConfigStore, EvalCtx, FlagEvaluation, FlagRule};
use crate::error::{ForgeError, Result};
use crate::kv::{Kv, SetOpts};
use crate::obs::Observability;
use crate::pubsub::{Pubsub, Subscription};
use crate::queue::{
    DeadLetterPage, DequeueOpts, EnqueueOpts, Job, JobId, JobStatus, JobStatusFilter,
    JobStatusPage, NackOpts, Queue, QueueDepth, QueueStats, RedriveBatchResult, RedriveOpts,
};
use crate::ratelimit::{Decision, FailMode, Limit, RateLimit, Reservation};
use crate::schedule::{Schedule, ScheduleInfo, ScheduleOpts, SchedulerDiagnostics};
use crate::types::Cursor;
use uuid::Uuid;

fn open(closing: &AtomicBool) -> Result<()> {
    if closing.load(Ordering::Acquire) {
        Err(ForgeError::precondition("Forge is shutting down")
            .with_context("lifecycle.reject_new_work", None))
    } else {
        Ok(())
    }
}

macro_rules! gate {
    ($name:ident, $trait:path, $primitive:literal) => {
        pub(crate) struct $name {
            inner: Arc<dyn $trait>,
            closing: Arc<AtomicBool>,
            obs: Arc<Observability>,
        }

        impl $name {
            pub(crate) fn new(
                inner: Arc<dyn $trait>,
                closing: Arc<AtomicBool>,
                obs: Arc<Observability>,
            ) -> Self {
                Self {
                    inner,
                    closing,
                    obs,
                }
            }

            async fn observe<T>(
                &self,
                operation: &'static str,
                future: impl std::future::Future<Output = Result<T>>,
            ) -> Result<T> {
                self.obs
                    .operation($primitive, operation, future)
                    .await
                    .map_err(|error| {
                        error.with_context(format!("{}.{operation}", $primitive), None)
                    })
            }
        }
    };
}

gate!(GatedKv, Kv, "kv");

#[async_trait]
impl Kv for GatedKv {
    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        open(&self.closing)?;
        self.observe("get", self.inner.get(key)).await
    }
    async fn mget(&self, keys: &[&str]) -> Result<Vec<Option<Bytes>>> {
        open(&self.closing)?;
        self.observe("mget", self.inner.mget(keys)).await
    }
    async fn set(&self, key: &str, value: Bytes, opts: SetOpts) -> Result<bool> {
        open(&self.closing)?;
        self.observe("set", self.inner.set(key, value, opts)).await
    }
    async fn delete(&self, key: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("delete", self.inner.delete(key)).await
    }
    async fn exists(&self, key: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("exists", self.inner.exists(key)).await
    }
    async fn incr(&self, key: &str, by: i64) -> Result<i64> {
        open(&self.closing)?;
        self.observe("incr", self.inner.incr(key, by)).await
    }
    async fn expire(&self, key: &str, ttl: Duration) -> Result<bool> {
        open(&self.closing)?;
        self.observe("expire", self.inner.expire(key, ttl)).await
    }
    async fn compare_and_swap(&self, key: &str, old: Option<Bytes>, new: Bytes) -> Result<bool> {
        open(&self.closing)?;
        self.observe(
            "compare_and_swap",
            self.inner.compare_and_swap(key, old, new),
        )
        .await
    }
    async fn scan(
        &self,
        prefix: &str,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<String>, Option<Cursor>)> {
        open(&self.closing)?;
        self.observe("scan", self.inner.scan(prefix, cursor, limit))
            .await
    }
}

gate!(GatedQueue, Queue, "queue");

#[async_trait]
impl Queue for GatedQueue {
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId> {
        open(&self.closing)?;
        let span = tracing::info_span!(
            "forge.messaging.send",
            messaging.system = "forge",
            messaging.operation.name = "send",
            messaging.destination.name = %queue,
            messaging.message.body.size = payload.len(),
        );
        #[cfg(feature = "otel")]
        if let Some(context) = &opts.trace_context {
            context.apply_to_span(&span, !opts.delay.is_zero());
        }
        self.observe(
            "enqueue",
            self.inner.enqueue(queue, payload, opts).instrument(span),
        )
        .await
    }
    async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>> {
        open(&self.closing)?;
        let span = tracing::info_span!(
            "forge.messaging.receive",
            messaging.system = "forge",
            messaging.operation.name = "receive",
            messaging.destination.name = %queue,
        );
        self.observe("dequeue", self.inner.dequeue(queue, opts).instrument(span))
            .await
    }
    // Settlement remains available while workers drain so leases are not stranded.
    async fn ack(&self, job: &Job) -> Result<()> {
        let span = tracing::info_span!(
            "forge.messaging.settle",
            messaging.system = "forge",
            messaging.operation.name = "settle",
            messaging.message.delivery_count = job.attempt,
            forge.settlement = "ack",
        );
        let result = self
            .observe("ack", self.inner.ack(job).instrument(span))
            .await;
        if result
            .as_ref()
            .is_err_and(|error| error.code() == "PRECONDITION")
        {
            self.obs
                .counter("forge_queue_lease_loss_total", &[("operation", "ack")], 1);
        }
        result
    }
    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()> {
        let span = tracing::info_span!(
            "forge.messaging.settle",
            messaging.system = "forge",
            messaging.operation.name = "settle",
            messaging.message.delivery_count = job.attempt,
            forge.settlement = "nack",
        );
        let result = self
            .observe("nack", self.inner.nack(job, opts).instrument(span))
            .await;
        if result.is_ok() && job.attempt >= job.max_attempts {
            self.obs
                .counter("forge_queue_dead_letter_transitions_total", &[], 1);
        }
        result
    }
    async fn heartbeat(&self, job: &Job) -> Result<()> {
        let result = self.observe("heartbeat", self.inner.heartbeat(job)).await;
        if result
            .as_ref()
            .is_err_and(|error| error.code() == "PRECONDITION")
        {
            self.obs.counter(
                "forge_queue_lease_loss_total",
                &[("operation", "heartbeat")],
                1,
            );
        }
        result
    }
    async fn cancellation_requested(&self, job: &Job) -> Result<bool> {
        self.observe(
            "cancellation_requested",
            self.inner.cancellation_requested(job),
        )
        .await
    }
    async fn cancel(&self, id: JobId) -> Result<Option<JobStatus>> {
        open(&self.closing)?;
        self.observe("cancel", self.inner.cancel(id)).await
    }
    async fn finish_cancellation(&self, job: &Job) -> Result<()> {
        self.observe("finish_cancellation", self.inner.finish_cancellation(job))
            .await
    }
    async fn status(&self, id: JobId) -> Result<Option<JobStatus>> {
        open(&self.closing)?;
        self.observe("status", self.inner.status(id)).await
    }
    async fn list_status(&self, filter: JobStatusFilter) -> Result<JobStatusPage> {
        open(&self.closing)?;
        self.observe("list_status", self.inner.list_status(filter))
            .await
    }
    async fn depth(&self, queue: &str) -> Result<QueueDepth> {
        open(&self.closing)?;
        let depth = self.observe("depth", self.inner.depth(queue)).await?;
        self.obs.gauge(
            "forge_queue_depth",
            &[("state", "visible")],
            depth.visible as f64,
        );
        self.obs.gauge(
            "forge_queue_depth",
            &[("state", "in_flight")],
            depth.in_flight as f64,
        );
        self.obs.gauge(
            "forge_queue_depth",
            &[("state", "delayed")],
            depth.delayed as f64,
        );
        if let Some(age) = depth.oldest_visible_age_ms {
            self.obs.gauge(
                "forge_queue_oldest_visible_age_seconds",
                &[],
                age as f64 / 1000.0,
            );
        }
        Ok(depth)
    }
    async fn pause(&self, queue: &str) -> Result<()> {
        open(&self.closing)?;
        self.observe("pause", self.inner.pause(queue)).await
    }
    async fn resume(&self, queue: &str) -> Result<()> {
        open(&self.closing)?;
        self.observe("resume", self.inner.resume(queue)).await
    }
    async fn is_paused(&self, queue: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("is_paused", self.inner.is_paused(queue)).await
    }
    async fn stats(&self, queue: &str) -> Result<QueueStats> {
        open(&self.closing)?;
        self.observe("stats", self.inner.stats(queue)).await
    }
    async fn dead_letters(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
    ) -> Result<DeadLetterPage> {
        open(&self.closing)?;
        self.observe(
            "dead_letters",
            self.inner.dead_letters(queue, cursor, limit),
        )
        .await
    }
    async fn redrive(&self, job_id: JobId, opts: RedriveOpts) -> Result<bool> {
        open(&self.closing)?;
        let redriven = self
            .observe("redrive", self.inner.redrive(job_id, opts))
            .await?;
        if redriven {
            self.obs.counter("forge_queue_redrives_total", &[], 1);
        }
        Ok(redriven)
    }
    async fn redrive_batch(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
        opts: RedriveOpts,
    ) -> Result<RedriveBatchResult> {
        open(&self.closing)?;
        let result = self
            .observe(
                "redrive_batch",
                self.inner.redrive_batch(queue, cursor, limit, opts),
            )
            .await?;
        self.obs.counter(
            "forge_queue_redrives_total",
            &[],
            u64::from(result.redriven),
        );
        Ok(result)
    }
    async fn purge_dead_letters_dry_run(&self, queue: &str) -> Result<u64> {
        open(&self.closing)?;
        self.observe(
            "purge_dead_letters_dry_run",
            self.inner.purge_dead_letters_dry_run(queue),
        )
        .await
    }
    async fn purge_dead_letters(&self, queue: &str, confirmation: &str) -> Result<u64> {
        open(&self.closing)?;
        self.observe(
            "purge_dead_letters",
            self.inner.purge_dead_letters(queue, confirmation),
        )
        .await
    }
}

gate!(GatedConfig, ConfigStore, "config");

#[async_trait]
impl ConfigStore for GatedConfig {
    async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        open(&self.closing)?;
        let value = self.observe("get_raw", self.inner.get_raw(key)).await;
        self.obs
            .counter("forge_config_reads_total", &[("source", "backend")], 1);
        value
    }
    async fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        open(&self.closing)?;
        self.observe("set_raw", self.inner.set_raw(key, value))
            .await
    }
    async fn flag(&self, key: &str, default: bool, ctx: &EvalCtx) -> bool {
        if open(&self.closing).is_err() {
            default
        } else {
            let started = std::time::Instant::now();
            let value = self.inner.flag(key, default, ctx).await;
            self.obs.counter(
                "forge_operations_total",
                &[
                    ("primitive", "config"),
                    ("operation", "flag"),
                    ("outcome", "ok"),
                ],
                1,
            );
            self.obs.histogram(
                "forge_operation_duration_seconds",
                &[("primitive", "config"), ("operation", "flag")],
                started.elapsed().as_secs_f64(),
            );
            value
        }
    }
    async fn flag_details(
        &self,
        key: &str,
        default: &serde_json::Value,
        ctx: &EvalCtx,
    ) -> FlagEvaluation {
        if open(&self.closing).is_err() {
            FlagEvaluation::new(default, None, "default_closed", Some("PRECONDITION".into()))
        } else {
            self.inner.flag_details(key, default, ctx).await
        }
    }
    async fn set_flag(&self, key: &str, rule: FlagRule) -> Result<()> {
        open(&self.closing)?;
        self.observe("set_flag", self.inner.set_flag(key, rule))
            .await
    }
    async fn delete_raw(&self, key: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("delete_raw", self.inner.delete_raw(key)).await
    }
    async fn delete_flag(&self, key: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("delete_flag", self.inner.delete_flag(key))
            .await
    }
}

gate!(GatedRateLimit, RateLimit, "ratelimit");

#[async_trait]
impl RateLimit for GatedRateLimit {
    async fn check_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        fail: FailMode,
    ) -> Result<Decision> {
        open(&self.closing)?;
        let decision = self
            .observe("check", self.inner.check_with(bucket, key, limit, fail))
            .await?;
        self.obs.counter(
            "forge_ratelimit_decisions_total",
            &[(
                "decision",
                if decision.allowed {
                    "allowed"
                } else {
                    "denied"
                },
            )],
            1,
        );
        Ok(decision)
    }
    async fn check_cost_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        cost: u32,
        fail: FailMode,
    ) -> Result<Decision> {
        open(&self.closing)?;
        self.observe(
            "check_cost",
            self.inner.check_cost_with(bucket, key, limit, cost, fail),
        )
        .await
    }
    async fn reserve(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        units: u32,
        ttl: Duration,
    ) -> Result<Option<Reservation>> {
        open(&self.closing)?;
        self.observe(
            "reserve",
            self.inner.reserve(bucket, key, limit, units, ttl),
        )
        .await
    }
    async fn commit(&self, reservation_id: Uuid, actual_units: u32) -> Result<Reservation> {
        open(&self.closing)?;
        self.observe("commit", self.inner.commit(reservation_id, actual_units))
            .await
    }
    async fn release(&self, reservation_id: Uuid) -> Result<Reservation> {
        open(&self.closing)?;
        self.observe("release", self.inner.release(reservation_id))
            .await
    }
}

gate!(GatedBlob, Blob, "blob");

impl GatedBlob {
    fn operation_span(operation: &'static str) -> tracing::Span {
        tracing::info_span!(
            "forge.object_store.operation",
            forge.blob.operation = operation,
        )
    }
}

#[async_trait]
impl Blob for GatedBlob {
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()> {
        open(&self.closing)?;
        let size = data.len();
        let result = self
            .observe(
                "put",
                self.inner
                    .put(key, data, opts)
                    .instrument(Self::operation_span("put")),
            )
            .await;
        if result.is_ok() {
            self.obs.histogram(
                "forge_blob_transfer_bytes",
                &[("direction", "upload")],
                size as f64,
            );
        }
        result
    }
    async fn put_stream(
        &self,
        key: &str,
        reader: BlobReader,
        content_length: u64,
        opts: PutOpts,
    ) -> Result<()> {
        open(&self.closing)?;
        let result = self
            .observe(
                "put_stream",
                self.inner
                    .put_stream(key, reader, content_length, opts)
                    .instrument(Self::operation_span("put_stream")),
            )
            .await;
        if result.is_ok() {
            self.obs.histogram(
                "forge_blob_transfer_bytes",
                &[("direction", "upload")],
                content_length as f64,
            );
        }
        result
    }
    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        open(&self.closing)?;
        let value = self
            .observe(
                "get",
                self.inner.get(key).instrument(Self::operation_span("get")),
            )
            .await?;
        if let Some(bytes) = &value {
            self.obs.histogram(
                "forge_blob_transfer_bytes",
                &[("direction", "download")],
                bytes.len() as f64,
            );
        }
        Ok(value)
    }
    async fn get_if(
        &self,
        key: &str,
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<ConditionalGet> {
        open(&self.closing)?;
        let value = self
            .observe(
                "get_if",
                self.inner
                    .get_if(key, if_match, if_none_match)
                    .instrument(Self::operation_span("get_if")),
            )
            .await?;
        if let ConditionalGet::Found { body, .. } = &value {
            self.obs.histogram(
                "forge_blob_transfer_bytes",
                &[("direction", "download")],
                body.len() as f64,
            );
        }
        Ok(value)
    }
    async fn open(&self, key: &str) -> Result<Option<BlobReader>> {
        open(&self.closing)?;
        self.observe(
            "open",
            self.inner
                .open(key)
                .instrument(Self::operation_span("open")),
        )
        .await
    }
    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Option<Bytes>> {
        open(&self.closing)?;
        let value = self
            .observe(
                "get_range",
                self.inner
                    .get_range(key, start, end)
                    .instrument(Self::operation_span("get_range")),
            )
            .await?;
        if let Some(bytes) = &value {
            self.obs.histogram(
                "forge_blob_transfer_bytes",
                &[("direction", "download")],
                bytes.len() as f64,
            );
        }
        Ok(value)
    }
    async fn head(&self, key: &str) -> Result<Option<BlobInfo>> {
        open(&self.closing)?;
        self.observe(
            "head",
            self.inner
                .head(key)
                .instrument(Self::operation_span("head")),
        )
        .await
    }
    async fn delete(&self, key: &str) -> Result<()> {
        open(&self.closing)?;
        self.observe(
            "delete",
            self.inner
                .delete(key)
                .instrument(Self::operation_span("delete")),
        )
        .await
    }
    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage> {
        open(&self.closing)?;
        self.observe(
            "list",
            self.inner
                .list(prefix, cursor, limit)
                .instrument(Self::operation_span("list")),
        )
        .await
    }
    async fn copy(&self, source: &str, destination: &str, opts: PutOpts) -> Result<BlobInfo> {
        open(&self.closing)?;
        self.observe(
            "copy",
            self.inner
                .copy(source, destination, opts)
                .instrument(Self::operation_span("copy")),
        )
        .await
    }
    async fn create_multipart(&self, key: &str, opts: PutOpts) -> Result<MultipartUpload> {
        open(&self.closing)?;
        self.observe(
            "create_multipart",
            self.inner
                .create_multipart(key, opts)
                .instrument(Self::operation_span("create_multipart")),
        )
        .await
    }
    async fn upload_part(
        &self,
        upload: &MultipartUpload,
        part_number: u32,
        body: Bytes,
    ) -> Result<MultipartPart> {
        open(&self.closing)?;
        let size = body.len();
        let result = self
            .observe(
                "upload_part",
                self.inner
                    .upload_part(upload, part_number, body)
                    .instrument(Self::operation_span("upload_part")),
            )
            .await;
        if result.is_ok() {
            self.obs.histogram(
                "forge_blob_transfer_bytes",
                &[("direction", "upload")],
                size as f64,
            );
        }
        result
    }
    async fn complete_multipart(
        &self,
        upload: &MultipartUpload,
        parts: Vec<MultipartPart>,
    ) -> Result<BlobInfo> {
        open(&self.closing)?;
        self.observe(
            "complete_multipart",
            self.inner
                .complete_multipart(upload, parts)
                .instrument(Self::operation_span("complete_multipart")),
        )
        .await
    }
    async fn abort_multipart(&self, upload: &MultipartUpload) -> Result<()> {
        open(&self.closing)?;
        self.observe(
            "abort_multipart",
            self.inner
                .abort_multipart(upload)
                .instrument(Self::operation_span("abort_multipart")),
        )
        .await
    }
    async fn verify_checksum_sha256(&self, key: &str, expected_hex: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe(
            "verify_checksum_sha256",
            self.inner
                .verify_checksum_sha256(key, expected_hex)
                .instrument(Self::operation_span("verify_checksum_sha256")),
        )
        .await
    }
    async fn presign_upload(
        &self,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<ProxyPresign> {
        open(&self.closing)?;
        self.observe(
            "presign_upload",
            self.inner.presign_upload(key, expires, max_bytes),
        )
        .await
    }
    async fn presign_download(&self, key: &str, expires: Duration) -> Result<ProxyPresign> {
        open(&self.closing)?;
        self.observe(
            "presign_download",
            self.inner.presign_download(key, expires),
        )
        .await
    }
    async fn presign_native_get(&self, key: &str, expires: Duration) -> Result<NativePresign> {
        open(&self.closing)?;
        self.observe(
            "presign_native_get",
            self.inner.presign_native_get(key, expires),
        )
        .await
    }
    async fn presign_native_put(
        &self,
        key: &str,
        expires: Duration,
        opts: PutOpts,
    ) -> Result<NativePresign> {
        open(&self.closing)?;
        self.observe(
            "presign_native_put",
            self.inner.presign_native_put(key, expires, opts),
        )
        .await
    }
    async fn verify_presigned(
        &self,
        method: &str,
        key: &str,
        expires_epoch: i64,
        max_bytes: u64,
        sig: &str,
    ) -> Result<bool> {
        open(&self.closing)?;
        self.observe(
            "verify_presigned",
            self.inner
                .verify_presigned(method, key, expires_epoch, max_bytes, sig),
        )
        .await
    }
}

gate!(GatedAuth, Auth, "auth");

impl GatedAuth {
    fn record_verification(&self, verified: bool) {
        self.obs.counter(
            "forge_auth_verifications_total",
            &[("outcome", if verified { "accepted" } else { "rejected" })],
            1,
        );
    }
}

#[async_trait]
impl Auth for GatedAuth {
    async fn hash_password(&self, plain: &str) -> Result<PhcString> {
        open(&self.closing)?;
        self.observe("hash_password", self.inner.hash_password(plain))
            .await
    }
    async fn verify_password(&self, plain: &str, hash: &PhcString) -> Result<bool> {
        open(&self.closing)?;
        let verified = self
            .observe("verify_password", self.inner.verify_password(plain, hash))
            .await?;
        self.record_verification(verified);
        Ok(verified)
    }
    fn needs_rehash(&self, hash: &PhcString) -> bool {
        self.inner.needs_rehash(hash)
    }
    async fn create_session(&self, user_id: &str, opts: SessionOpts) -> Result<SessionToken> {
        open(&self.closing)?;
        self.observe("create_session", self.inner.create_session(user_id, opts))
            .await
    }
    async fn validate_session(&self, token: &str) -> Result<Option<Session>> {
        open(&self.closing)?;
        self.observe("validate_session", self.inner.validate_session(token))
            .await
    }
    async fn revoke_session(&self, token: &str) -> Result<()> {
        open(&self.closing)?;
        self.observe("revoke_session", self.inner.revoke_session(token))
            .await
    }
    async fn revoke_all_sessions(&self, user_id: &str) -> Result<u64> {
        open(&self.closing)?;
        self.observe(
            "revoke_all_sessions",
            self.inner.revoke_all_sessions(user_id),
        )
        .await
    }
    async fn create_api_key(&self, owner_id: &str, label: &str) -> Result<ApiKey> {
        open(&self.closing)?;
        self.observe("create_api_key", self.inner.create_api_key(owner_id, label))
            .await
    }
    async fn create_api_key_with(
        &self,
        owner_id: &str,
        label: &str,
        opts: ApiKeyOpts,
    ) -> Result<ApiKey> {
        open(&self.closing)?;
        self.observe(
            "create_api_key",
            self.inner.create_api_key_with(owner_id, label, opts),
        )
        .await
    }
    async fn verify_api_key(&self, key: &str) -> Result<Option<ApiKeyInfo>> {
        open(&self.closing)?;
        let value = self
            .observe("verify_api_key", self.inner.verify_api_key(key))
            .await?;
        self.record_verification(value.is_some());
        Ok(value)
    }
    async fn revoke_api_key(&self, key_id: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("revoke_api_key", self.inner.revoke_api_key(key_id))
            .await
    }
    async fn create_token(
        &self,
        user_id: &str,
        purpose: &str,
        ttl: Duration,
    ) -> Result<OneTimeToken> {
        open(&self.closing)?;
        self.observe(
            "create_token",
            self.inner.create_token(user_id, purpose, ttl),
        )
        .await
    }
    async fn create_token_with_payload(
        &self,
        user_id: &str,
        purpose: &str,
        ttl: Duration,
        payload: Bytes,
    ) -> Result<OneTimeToken> {
        open(&self.closing)?;
        self.observe(
            "create_token",
            self.inner
                .create_token_with_payload(user_id, purpose, ttl, payload),
        )
        .await
    }
    async fn consume_token(&self, token: &str, purpose: &str) -> Result<Option<String>> {
        open(&self.closing)?;
        let value = self
            .observe("consume_token", self.inner.consume_token(token, purpose))
            .await?;
        self.record_verification(value.is_some());
        Ok(value)
    }
    async fn consume_token_with_payload(
        &self,
        token: &str,
        purpose: &str,
    ) -> Result<Option<TokenConsumption>> {
        open(&self.closing)?;
        let value = self
            .observe(
                "consume_token",
                self.inner.consume_token_with_payload(token, purpose),
            )
            .await?;
        self.record_verification(value.is_some());
        Ok(value)
    }
}

gate!(GatedSchedule, Schedule, "schedule");

#[async_trait]
impl Schedule for GatedSchedule {
    async fn cron(
        &self,
        name: &str,
        expr: &str,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<()> {
        open(&self.closing)?;
        self.observe("cron", self.inner.cron(name, expr, queue, payload, opts))
            .await
    }
    async fn at(
        &self,
        when: SystemTime,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<JobId> {
        open(&self.closing)?;
        self.observe("at", self.inner.at(when, queue, payload, opts))
            .await
    }
    async fn cancel(&self, name: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("cancel", self.inner.cancel(name)).await
    }
    async fn inspect(&self, name: &str) -> Result<Option<ScheduleInfo>> {
        open(&self.closing)?;
        self.observe("inspect", self.inner.inspect(name)).await
    }
    async fn pause(&self, name: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("pause", self.inner.pause(name)).await
    }
    async fn resume(&self, name: &str) -> Result<bool> {
        open(&self.closing)?;
        self.observe("resume", self.inner.resume(name)).await
    }
    async fn diagnostics(&self) -> Result<SchedulerDiagnostics> {
        open(&self.closing)?;
        self.observe("diagnostics", self.inner.diagnostics()).await
    }
    async fn cancel_at(&self, job_id: JobId) -> Result<bool> {
        open(&self.closing)?;
        self.observe("cancel_at", self.inner.cancel_at(job_id))
            .await
    }
    async fn list(
        &self,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<ScheduleInfo>, Option<Cursor>)> {
        open(&self.closing)?;
        self.observe("list", self.inner.list(cursor, limit)).await
    }
    async fn process_due(&self) -> Result<u64> {
        open(&self.closing)?;
        let span = tracing::info_span!(
            "forge.messaging.scheduler",
            messaging.system = "forge",
            messaging.operation.name = "send",
        );
        let processed = self
            .observe("process_due", self.inner.process_due().instrument(span))
            .await?;
        let diagnostics = self.inner.diagnostics().await?;
        self.obs.gauge(
            "forge_scheduler_lag_seconds",
            &[],
            diagnostics.lag.map_or(0.0, |lag| lag.as_secs_f64()),
        );
        self.obs
            .gauge("forge_scheduler_due", &[], diagnostics.due_count as f64);
        self.obs.gauge(
            "forge_scheduler_enqueue_failures",
            &[],
            diagnostics.enqueue_failures as f64,
        );
        self.obs
            .counter("forge_scheduler_dispatch_total", &[], processed);
        Ok(processed)
    }
}

pub(crate) struct GatedPubsub {
    inner: Arc<dyn Pubsub>,
    closing: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Sender<bool>,
    obs: Arc<Observability>,
}

impl GatedPubsub {
    pub(crate) fn new(
        inner: Arc<dyn Pubsub>,
        closing: Arc<AtomicBool>,
        shutdown: tokio::sync::watch::Sender<bool>,
        obs: Arc<Observability>,
    ) -> Self {
        Self {
            inner,
            closing,
            shutdown,
            obs,
        }
    }

    async fn observe<T>(
        &self,
        operation: &'static str,
        future: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.obs.operation("pubsub", operation, future).await
    }
}

#[async_trait]
impl Pubsub for GatedPubsub {
    fn channel_for(&self, topic: &str) -> Result<String> {
        open(&self.closing)?;
        self.inner.channel_for(topic)
    }
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<()> {
        open(&self.closing)?;
        self.observe("publish", self.inner.publish(topic, payload))
            .await
    }
    async fn subscribe(&self, topic: &str) -> Result<Subscription> {
        open(&self.closing)?;
        let stream = self
            .observe("subscribe", self.inner.subscribe(topic))
            .await?;
        open(&self.closing)?;
        let mut shutdown = self.shutdown.subscribe();
        Ok(stream
            .take_until(async move {
                let _ = shutdown.wait_for(|closed| *closed).await;
            })
            .boxed())
    }
}

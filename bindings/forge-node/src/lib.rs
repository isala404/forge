//! Node.js bindings for Forge via napi-rs.
//!
//! Exposes a representative slice of every primitive (kv, queue, config, ratelimit,
//! blob, auth, schedule) to JavaScript. Async Rust methods become JS `Promise`s
//! (snake_case → camelCase). The queue is exposed as raw `enqueue`/`dequeue`/`ack`/
//! `nack`: leased jobs are held Rust-side in a map and referenced from JS by id, so
//! the opaque lease fence never crosses the boundary.

use futures_util::StreamExt;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// A stable, machine-readable code for each `ForgeError` variant, so JS callers can
/// branch on the failure class (prefixed onto the error message in [`err`]).
fn code_of(e: &forge::ForgeError) -> &'static str {
    match e {
        forge::ForgeError::NotFound => "NOT_FOUND",
        forge::ForgeError::Invalid(_) => "INVALID",
        forge::ForgeError::Limit(_) => "LIMIT",
        forge::ForgeError::Precondition(_) => "PRECONDITION",
        forge::ForgeError::Unavailable(_) => "UNAVAILABLE",
        forge::ForgeError::Config(_) => "CONFIG",
        _ => "BACKEND",
    }
}

fn err(e: forge::ForgeError) -> napi::Error {
    napi::Error::from_reason(format!("{}: {}", code_of(&e), e))
}

/// Convert an `f64` seconds value into a `Duration`, raising `Invalid` on a
/// negative or non-finite input. Zero passes straight through so the core applies
/// its own validation: bindings convert and pass through, they never clamp or
/// silently coerce a caller's out-of-range value (P0-5).
fn secs(field: &str, value: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| {
        err(forge::ForgeError::invalid(format!(
            "{field} must be a non-negative number of seconds"
        )))
    })
}

/// A leased job handed to JavaScript. Settle it with `ack`/`nack`/`heartbeat`
/// using the opaque, delivery-unique `receipt` (NOT `id`, which is stable across
/// redeliveries and is the natural idempotency key).
#[napi(object)]
pub struct JsJob {
    pub id: String,
    /// Delivery-unique handle for ack/nack/heartbeat (SQS ReceiptHandle).
    pub receipt: String,
    pub payload: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub leased_until_ms: f64,
    pub queue: String,
}

/// A rate-limit decision (maps onto the IETF RateLimit header fields).
#[napi(object)]
pub struct JsDecision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    /// Seconds until the limit fully resets (the IETF `RateLimit-Reset` value).
    pub reset_after_seconds: f64,
    pub retry_after_seconds: Option<f64>,
}

/// A freshly minted API key. `secret` is shown exactly once.
#[napi(object)]
pub struct JsApiKey {
    pub id: String,
    pub secret: String,
    pub label: String,
    pub created_at_ms: f64,
}

/// Approximate queue depth (SQS `ApproximateNumberOfMessages{,NotVisible,Delayed}`).
#[napi(object)]
pub struct JsQueueDepth {
    pub visible: u32,
    pub in_flight: u32,
    pub delayed: u32,
}

/// One page of `kvScanPage`: the keys plus an opaque `cursor` for the next page
/// (`null` when iteration is complete).
#[napi(object)]
pub struct JsScanPage {
    pub keys: Vec<String>,
    pub cursor: Option<String>,
}

/// Object metadata (S3 `HeadObject`). `lastModifiedMs` is epoch milliseconds.
#[napi(object)]
pub struct JsBlobInfo {
    pub key: String,
    pub size: f64,
    pub content_type: String,
    pub etag: String,
    pub last_modified_ms: f64,
    pub metadata: HashMap<String, String>,
}

/// One page of `blobList`: the objects plus an opaque next-page `cursor`.
#[napi(object)]
pub struct JsBlobPage {
    pub items: Vec<JsBlobInfo>,
    pub cursor: Option<String>,
}

/// A registered schedule (`scheduleList`). `kind` is `"cron"` or `"at"`; `cronExpr`
/// is set only for crons. Times are epoch milliseconds.
#[napi(object)]
pub struct JsScheduleInfo {
    pub name: String,
    pub kind: String,
    pub cron_expr: Option<String>,
    pub queue: String,
    pub next_run_ms: f64,
    pub last_run_ms: Option<f64>,
}

/// A validated session's metadata (`validateSessionInfo`). Times are epoch ms.
#[napi(object)]
pub struct JsSession {
    pub user_id: String,
    pub created_at_ms: f64,
    pub expires_at_ms: f64,
}

/// Non-secret API-key metadata (`verifyApiKeyInfo`).
#[napi(object)]
pub struct JsApiKeyInfo {
    pub id: String,
    pub owner_id: String,
    pub label: String,
}

/// One line of `backendReport`: which provider powers a primitive.
#[napi(object)]
pub struct JsBackendInfo {
    pub primitive: String,
    pub provider: String,
    pub durable: bool,
    pub caveats: String,
}

/// Connection options for `ForgeClient.connectWith` — the full per-deployment surface
/// (every field optional; omitted fields take Forge's defaults).
#[napi(object)]
#[derive(Default)]
pub struct JsConnectOptions {
    pub signing_secret: Option<String>,
    pub kv_namespace: Option<String>,
    pub max_connections: Option<u32>,
    pub run_migrations: Option<bool>,
    pub blob_base_url: Option<String>,
    /// Set to store blob bytes on a local directory instead of in Postgres `BYTEA`.
    pub filesystem_blob_root: Option<String>,
}

/// Epoch milliseconds for a `SystemTime` (saturating).
fn epoch_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// A Forge client: one Postgres pool, every primitive. Construct with
/// `ForgeClient.connect(url)`.
#[napi]
pub struct ForgeClient {
    forge: forge::Forge,
    /// Leased-but-not-settled jobs, keyed by a delivery-unique opaque receipt
    /// (not the job id), so a job redelivered to this same process gets a fresh
    /// entry instead of overwriting the in-flight one. `ack`/`nack`/`heartbeat`
    /// recover the `forge::Job` (whose lease fence is not part of the public
    /// surface) by receipt. Entries are evicted on settle and, as a leak backstop,
    /// once their original lease has been expired for over 24h.
    leased: Arc<Mutex<HashMap<String, forge::Job>>>,
    /// Monotonic counter making each dequeue's receipt unique.
    seq: Arc<std::sync::atomic::AtomicU64>,
}

impl ForgeClient {
    fn from_forge(forge: forge::Forge) -> Self {
        Self {
            forge,
            leased: Arc::new(Mutex::new(HashMap::new())),
            seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
}

#[napi]
impl ForgeClient {
    /// Connect, run migrations, and ping — mirrors `Forge::init`. Pass
    /// `signingSecret` to enable presigned blob URLs.
    #[napi(factory)]
    pub async fn connect(
        postgres_url: String,
        signing_secret: Option<String>,
    ) -> Result<ForgeClient> {
        let mut cfg = forge::ForgeConfig::new(postgres_url);
        if let Some(secret) = signing_secret {
            cfg = cfg.with_blob_signing_secret(secret);
        }
        let forge = forge::Forge::init(cfg).await.map_err(err)?;
        Ok(ForgeClient::from_forge(forge))
    }

    /// Connect with the full per-deployment option surface (namespace, pool size,
    /// migration toggle, blob backend, …) instead of just a URL + signing secret.
    #[napi(factory)]
    pub async fn connect_with(
        postgres_url: String,
        options: JsConnectOptions,
    ) -> Result<ForgeClient> {
        let mut cfg = forge::ForgeConfig::new(postgres_url);
        if let Some(s) = options.signing_secret {
            cfg = cfg.with_blob_signing_secret(s);
        }
        if let Some(ns) = options.kv_namespace {
            cfg = cfg.with_kv_namespace(ns);
        }
        if let Some(n) = options.max_connections {
            cfg = cfg.with_max_connections(n);
        }
        if options.run_migrations == Some(false) {
            cfg = cfg.without_migrations();
        }
        if let Some(base) = options.blob_base_url {
            cfg = cfg.with_blob_base_url(base);
        }
        if let Some(root) = options.filesystem_blob_root {
            cfg = cfg.with_filesystem_blob(root);
        }
        let forge = forge::Forge::init(cfg).await.map_err(err)?;
        Ok(ForgeClient::from_forge(forge))
    }

    /// A backend report: which provider powers each primitive (for health pages/logs).
    #[napi]
    pub fn backend_report(&self) -> Vec<JsBackendInfo> {
        self.forge
            .backend_report()
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

    /// `GET key` → the value as a UTF-8 string, or `null`. The string surface is
    /// UTF-8-only; use `kvGetBytes` for values that may hold arbitrary bytes.
    #[napi]
    pub async fn kv_get(&self, key: String) -> Result<Option<String>> {
        let v = self.forge.kv().get(&key).await.map_err(err)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// `GET key` → the raw value bytes, or `null`. Lossless, unlike `kvGet` (P0-4).
    #[napi]
    pub async fn kv_get_bytes(&self, key: String) -> Result<Option<Buffer>> {
        let v = self.forge.kv().get(&key).await.map_err(err)?;
        Ok(v.map(|b| Buffer::from(b.to_vec())))
    }

    /// `MGET keys` → each value as a UTF-8 string (or `null` if missing/expired), in
    /// input order. One round-trip — use instead of a per-key `kvGet` loop.
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
    ) -> Result<bool> {
        let mut opts = forge::SetOpts::new();
        if let Some(t) = ttl_seconds {
            opts = opts.with_ttl(secs("ttlSeconds", t)?);
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
        let mut opts = forge::SetOpts::new();
        if let Some(t) = ttl_seconds {
            opts = opts.with_ttl(secs("ttlSeconds", t)?);
        }
        if if_not_exists.unwrap_or(false) {
            opts = opts.with_mode(forge::SetMode::IfNotExists);
        }
        self.forge
            .kv()
            .set(&key, forge::Bytes::from(value.to_vec()), opts)
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
    pub async fn queue_enqueue(
        &self,
        queue: String,
        payload: String,
        max_attempts: Option<u32>,
        dedup_id: Option<String>,
    ) -> Result<String> {
        let mut opts = forge::EnqueueOpts::new();
        if let Some(m) = max_attempts {
            opts = opts.with_max_attempts(m);
        }
        if let Some(d) = dedup_id {
            opts = opts.with_dedup_id(d);
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
            .with_visibility_timeout(secs("visibilitySeconds", visibility_seconds)?)
            .with_wait(secs("waitSeconds", wait_seconds)?);
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
                    payload: String::from_utf8_lossy(&job.payload).into_owned(),
                    attempt: job.attempt,
                    max_attempts: job.max_attempts,
                    leased_until_ms: epoch_ms(job.leased_until),
                    queue: job.queue.clone(),
                };
                let mut leased = self.leased.lock().await;
                // Backstop against true leaks (dequeued, never settled): drop entries
                // whose lease lapsed over 24h ago. The grace is far longer than any
                // heartbeat window, so a heartbeated job is never evicted mid-flight.
                let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
                leased.retain(|_, j| j.leased_until > cutoff);
                leased.insert(receipt, job);
                Ok(Some(js))
            }
            None => Ok(None),
        }
    }

    /// Ack a leased job by its `receipt` (idempotent: a no-op if already settled).
    #[napi]
    pub async fn queue_ack(&self, receipt: String) -> Result<()> {
        let job = self.leased.lock().await.remove(&receipt);
        if let Some(job) = job {
            self.forge.queue().ack(&job).await.map_err(err)?;
        }
        Ok(())
    }

    /// Nack a leased job by its `receipt`; optional `retrySeconds` delays the
    /// redelivery. Raises `PRECONDITION` if the receipt is unknown (the lease was
    /// lost — stop working on this job).
    #[napi]
    pub async fn queue_nack(&self, receipt: String, retry_seconds: Option<f64>) -> Result<()> {
        let job = self.leased.lock().await.remove(&receipt);
        let Some(job) = job else {
            return Err(err(forge::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        let opts = match retry_seconds {
            Some(s) => forge::NackOpts::retry_in(secs("retrySeconds", s)?),
            None => forge::NackOpts::default(),
        };
        self.forge.queue().nack(&job, opts).await.map_err(err)
    }

    /// Extend the lease on a job leased by this client (SQS ChangeMessageVisibility /
    /// beanstalkd touch) by one visibility timeout. Call before `leasedUntilMs` for a
    /// handler that may outlive its visibility window, so the job is not redelivered
    /// mid-flight. Raises `PRECONDITION` if the receipt is unknown (the lease was
    /// lost — stop working on this job).
    #[napi]
    pub async fn queue_heartbeat(&self, receipt: String) -> Result<()> {
        let job = self.leased.lock().await.get(&receipt).cloned();
        let Some(job) = job else {
            return Err(err(forge::ForgeError::precondition(
                "unknown receipt: the lease was lost",
            )));
        };
        self.forge.queue().heartbeat(&job).await.map_err(err)
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
        })
    }

    /// Store a config value (`set_raw`).
    #[napi]
    pub async fn config_set(&self, key: String, value: String) -> Result<()> {
        self.forge.config().set_raw(&key, &value).await.map_err(err)
    }

    /// Resolve a config value (env `FORGE_CFG_<KEY>` > store > `null`).
    #[napi]
    pub async fn config_get(&self, key: String) -> Result<Option<String>> {
        self.forge.config().get_raw(&key).await.map_err(err)
    }

    /// Set a percentage-rollout flag (`0..=100`).
    #[napi]
    pub async fn set_flag_percent(&self, key: String, percent: u8) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forge::FlagRule::Percent(percent))
            .await
            .map_err(err)
    }

    /// Evaluate a boolean flag for `targetingKey`. Never throws — resolves to
    /// `defaultValue` on any failure.
    #[napi]
    pub async fn flag(
        &self,
        key: String,
        default_value: bool,
        targeting_key: Option<String>,
    ) -> bool {
        let ctx = match targeting_key {
            Some(k) => forge::EvalCtx::user(k),
            None => forge::EvalCtx::new(),
        };
        self.forge.config().flag(&key, default_value, &ctx).await
    }

    /// Atomic check-and-consume: `max` per `perSeconds` (token bucket).
    /// `failOpen` overrides what happens on a backend error: omit for the instance
    /// default, `true` to allow, `false` to deny.
    #[napi]
    pub async fn rate_limit_check(
        &self,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
        fail_open: Option<bool>,
    ) -> Result<JsDecision> {
        let limit = forge::Limit::per_duration(max, secs("perSeconds", per_seconds)?);
        let fm = match fail_open {
            None => forge::FailMode::Default,
            Some(true) => forge::FailMode::Open,
            Some(false) => forge::FailMode::Closed,
        };
        let d = self
            .forge
            .ratelimit()
            .check_with(&bucket, &key, limit, fm)
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

    /// Store an object (string body).
    #[napi]
    pub async fn blob_put(
        &self,
        key: String,
        data: String,
        content_type: Option<String>,
    ) -> Result<()> {
        let mut opts = forge::PutOpts::new();
        if let Some(ct) = content_type {
            opts = opts.with_content_type(ct);
        }
        self.forge
            .blob()
            .put(&key, forge::Bytes::from(data), opts)
            .await
            .map_err(err)
    }

    /// Store an object (binary body).
    #[napi]
    pub async fn blob_put_bytes(
        &self,
        key: String,
        data: Buffer,
        content_type: Option<String>,
    ) -> Result<()> {
        let mut opts = forge::PutOpts::new();
        if let Some(ct) = content_type {
            opts = opts.with_content_type(ct);
        }
        self.forge
            .blob()
            .put(&key, forge::Bytes::from(data.to_vec()), opts)
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

    /// A presigned download URL (needs a `signingSecret` at connect).
    #[napi]
    pub async fn blob_presign_download(&self, key: String, expires_seconds: f64) -> Result<String> {
        self.forge
            .blob()
            .presign_download(&key, secs("expiresSeconds", expires_seconds)?)
            .await
            .map_err(err)
    }

    /// A presigned upload (PUT) URL, capped at `maxBytes` (needs a `signingSecret`).
    #[napi]
    pub async fn blob_presign_upload(
        &self,
        key: String,
        expires_seconds: f64,
        max_bytes: f64,
    ) -> Result<String> {
        self.forge
            .blob()
            .presign_upload(
                &key,
                secs("expiresSeconds", expires_seconds)?,
                max_bytes.max(0.0) as u64,
            )
            .await
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
                max_bytes.max(0.0) as u64,
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

    /// Delete an object; returns whether it existed.
    #[napi]
    pub async fn blob_delete(&self, key: String) -> Result<bool> {
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
            .verify_password(&plain, &forge::PhcString::new(hash))
            .await
            .map_err(err)
    }

    /// Whether a stored PHC `hash` should be re-hashed (its argon2id params are below
    /// the current Forge baseline). Call after a successful `verifyPassword`; if `true`,
    /// re-hash the plaintext and persist it — transparent upgrade, no forced reset.
    #[napi]
    pub fn needs_rehash(&self, hash: String) -> bool {
        self.forge.auth().needs_rehash(&forge::PhcString::new(hash))
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
        let mut opts = forge::SessionOpts::new();
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
        })
    }

    /// Verify an API key; returns the `ownerId`, or `null`.
    #[napi]
    pub async fn verify_api_key(&self, key: String) -> Result<Option<String>> {
        Ok(self
            .forge
            .auth()
            .verify_api_key(&key)
            .await
            .map_err(err)?
            .map(|i| i.owner_id))
    }

    /// Schedule a one-shot enqueue at `whenEpochMs`; returns the future JobId.
    #[napi]
    pub async fn schedule_at(
        &self,
        when_epoch_ms: f64,
        queue: String,
        payload: String,
    ) -> Result<String> {
        let when = UNIX_EPOCH + Duration::from_millis(when_epoch_ms.max(0.0) as u64);
        let id = self
            .forge
            .schedule()
            .at(when, &queue, forge::Bytes::from(payload))
            .await
            .map_err(err)?;
        Ok(id.to_string())
    }

    /// Upsert a recurring cron schedule by name.
    #[napi]
    pub async fn schedule_cron(
        &self,
        name: String,
        expr: String,
        queue: String,
        payload: String,
    ) -> Result<()> {
        self.forge
            .schedule()
            .cron(&name, &expr, &queue, forge::Bytes::from(payload))
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
            .publish(&topic, forge::Bytes::from(payload))
            .await
            .map_err(err)
    }

    /// Subscribe to a realtime topic, returning a handle whose `next()` yields each
    /// payload published *after* this resolves (or `null` when the stream ends).
    /// Subscriptions share one per-process listener connection; drop the handle to
    /// unsubscribe (the channel is released once it has no remaining subscribers).
    #[napi]
    pub async fn pubsub_subscribe(&self, topic: String) -> Result<JsSubscription> {
        let sub = self.forge.pubsub().subscribe(&topic).await.map_err(err)?;
        Ok(JsSubscription {
            inner: Arc::new(Mutex::new(sub)),
        })
    }

    /// The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. `LISTEN` on this with
    /// a native Postgres client to receive what `pubsub_publish(topic, …)` sends.
    #[napi]
    pub fn pubsub_channel(&self, topic: String) -> String {
        forge::pubsub::channel_for(&topic)
    }

    // --- kv parity -----------------------------------------------------------------

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
                old.map(forge::Bytes::from),
                forge::Bytes::from(new_value),
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
        let cur = cursor.map(forge::Cursor::from_token);
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

    // --- blob parity ---------------------------------------------------------------

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
            .map(|i| JsBlobInfo {
                key: i.key,
                size: i.size as f64,
                content_type: i.content_type,
                etag: i.etag,
                last_modified_ms: epoch_ms(i.last_modified),
                metadata: i.metadata.into_iter().collect(),
            }))
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
        let cur = cursor.map(forge::Cursor::from_token);
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
                .map(|i| JsBlobInfo {
                    key: i.key,
                    size: i.size as f64,
                    content_type: i.content_type,
                    etag: i.etag,
                    last_modified_ms: epoch_ms(i.last_modified),
                    metadata: i.metadata.into_iter().collect(),
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
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
    ) -> Result<()> {
        let mut opts = forge::PutOpts::new();
        if let Some(ct) = content_type {
            opts = opts.with_content_type(ct);
        }
        if let Some(meta) = metadata {
            for (k, v) in meta {
                opts = opts.with_metadata(k, v);
            }
        }
        self.forge
            .blob()
            .put(&key, forge::Bytes::from(data.to_vec()), opts)
            .await
            .map_err(err)
    }

    // --- schedule parity -----------------------------------------------------------

    /// Cancel a schedule by name. `true` if one was removed, `false` if none existed.
    #[napi]
    pub async fn schedule_cancel(&self, name: String) -> Result<bool> {
        self.forge.schedule().cancel(&name).await.map_err(err)
    }

    /// List every registered schedule (crons and pending one-shots).
    #[napi]
    pub async fn schedule_list(&self) -> Result<Vec<JsScheduleInfo>> {
        let items = self.forge.schedule().list().await.map_err(err)?;
        Ok(items
            .into_iter()
            .map(|s| {
                let (kind, cron_expr) = match s.kind {
                    forge::ScheduleKind::Cron(e) => ("cron".to_string(), Some(e)),
                    _ => ("at".to_string(), None),
                };
                JsScheduleInfo {
                    name: s.name,
                    kind,
                    cron_expr,
                    queue: s.queue,
                    next_run_ms: epoch_ms(s.next_run),
                    last_run_ms: s.last_run.map(epoch_ms),
                }
            })
            .collect())
    }

    // --- config flag parity --------------------------------------------------------

    /// Set a flag to always-on.
    #[napi]
    pub async fn set_flag_on(&self, key: String) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forge::FlagRule::On)
            .await
            .map_err(err)
    }

    /// Set a flag to always-off.
    #[napi]
    pub async fn set_flag_off(&self, key: String) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forge::FlagRule::Off)
            .await
            .map_err(err)
    }

    /// Set a flag to an allow-list of targeting keys.
    #[napi]
    pub async fn set_flag_allow_list(&self, key: String, entries: Vec<String>) -> Result<()> {
        self.forge
            .config()
            .set_flag(&key, forge::FlagRule::AllowList(entries))
            .await
            .map_err(err)
    }

    // --- auth parity ---------------------------------------------------------------

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

    /// Verify an API key; returns full non-secret metadata (id, owner, label), or
    /// `null`. Use `verifyApiKey` when only the owner id is needed.
    #[napi]
    pub async fn verify_api_key_info(&self, key: String) -> Result<Option<JsApiKeyInfo>> {
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
    inner: Arc<Mutex<forge::Subscription>>,
}

#[napi]
impl JsSubscription {
    /// The next published payload as raw bytes, or `null` when the stream ends.
    #[napi]
    pub async fn next(&self) -> Result<Option<Buffer>> {
        let mut inner = self.inner.lock().await;
        match inner.next().await {
            Some(Ok(b)) => Ok(Some(Buffer::from(b.to_vec()))),
            Some(Err(e)) => Err(err(e)),
            None => Ok(None),
        }
    }
}

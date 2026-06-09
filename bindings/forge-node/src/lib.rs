//! Node.js bindings for Forge via napi-rs.
//!
//! Exposes a representative slice of every primitive (kv, queue, config, ratelimit,
//! blob, auth, schedule) to JavaScript. Async Rust methods become JS `Promise`s
//! (snake_case → camelCase). The queue is exposed as raw `enqueue`/`dequeue`/`ack`/
//! `nack`: leased jobs are held Rust-side in a map and referenced from JS by id, so
//! the opaque lease fence never crosses the boundary.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

/// A rate-limit decision (maps onto the IETF RateLimit header fields).
#[napi(object)]
pub struct JsDecision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    pub retry_after_seconds: Option<f64>,
}

/// A freshly minted API key. `secret` is shown exactly once.
#[napi(object)]
pub struct JsApiKey {
    pub id: String,
    pub secret: String,
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

    // ---- config + flags ------------------------------------------------------

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

    // ---- ratelimit -----------------------------------------------------------

    /// Atomic check-and-consume: `max` per `perSeconds` (token bucket).
    #[napi]
    pub async fn rate_limit_check(
        &self,
        bucket: String,
        key: String,
        max: u32,
        per_seconds: f64,
    ) -> Result<JsDecision> {
        let limit = forge::Limit::per_duration(max, Duration::from_secs_f64(per_seconds.max(1.0)));
        let d = self
            .forge
            .ratelimit()
            .check(&bucket, &key, limit)
            .await
            .map_err(err)?;
        Ok(JsDecision {
            allowed: d.allowed,
            limit: d.limit,
            remaining: d.remaining,
            retry_after_seconds: d.retry_after.map(|x| x.as_secs_f64()),
        })
    }

    // ---- blob ----------------------------------------------------------------

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

    /// Fetch an object as a UTF-8 string, or `null`.
    #[napi]
    pub async fn blob_get(&self, key: String) -> Result<Option<String>> {
        let v = self.forge.blob().get(&key).await.map_err(err)?;
        Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// A presigned download URL (needs a `signingSecret` at connect).
    #[napi]
    pub async fn blob_presign_download(&self, key: String, expires_seconds: f64) -> Result<String> {
        self.forge
            .blob()
            .presign_download(&key, Duration::from_secs_f64(expires_seconds.max(1.0)))
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
                Duration::from_secs_f64(expires_seconds.max(1.0)),
                max_bytes.max(0.0) as u64,
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

    // ---- auth ----------------------------------------------------------------

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
            opts = opts.with_idle_timeout(Duration::from_secs_f64(s.max(1.0)));
        }
        if let Some(s) = absolute_seconds {
            opts = opts.with_absolute_timeout(Duration::from_secs_f64(s.max(1.0)));
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

    // ---- schedule ------------------------------------------------------------

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

    // ---- pubsub --------------------------------------------------------------

    /// Publish a payload to a realtime topic (fire-and-forget).
    #[napi]
    pub async fn pubsub_publish(&self, topic: String, payload: String) -> Result<()> {
        self.forge
            .pubsub()
            .publish(&topic, forge::Bytes::from(payload))
            .await
            .map_err(err)
    }

    /// The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. `LISTEN` on this with
    /// a native Postgres client to receive what `pubsub_publish(topic, …)` sends.
    #[napi]
    pub fn pubsub_channel(&self, topic: String) -> String {
        forge::pubsub::channel_for(&topic)
    }
}

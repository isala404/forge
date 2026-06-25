//! Reusable conformance kit for Forge backends.
//!
//! The cross-language contract is encoded as a declarative scenario matrix
//! (`src/conformance/scenarios/*.json`). This module embeds that matrix and runs it
//! against any [`crate::Forge`] handed to it through a [`ForgeFactory`], turning
//! the contract into an executable conformance suite that an external backend
//! author can run against their own implementation.
//!
//! Build a `Forge` over your primitives, implement [`ForgeFactory`], call
//! [`run_all`], and read the returned [`Report`].
// Scenario JSON is trusted, compiled-in input, so this harness keeps the lenient
// lint posture of the original internal runner. `unreachable_patterns` is allowed
// because the interpreter keeps wildcard arms for the `#[non_exhaustive]`
// ScheduleKind and ForgeError enums — mandatory when this kit was a separate crate,
// and kept as forward-compat fallbacks now that it lives inside `forge`.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    unreachable_patterns
)]

use async_trait::async_trait;
use bytes::Bytes;
use crate::{Cursor, Forge, ForgeError, Limit, PutOpts, ScheduleKind, SetMode, SetOpts, Subscription};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The scenario matrix, embedded so the kit is self-contained — external users
/// need no particular filesystem layout. Each entry is `(primitive, json)`.
const SCENARIOS: &[(&str, &str)] = &[
    ("kv", include_str!("scenarios/kv.json")),
    ("queue", include_str!("scenarios/queue.json")),
    ("config", include_str!("scenarios/config.json")),
    ("ratelimit", include_str!("scenarios/ratelimit.json")),
    ("schedule", include_str!("scenarios/schedule.json")),
    ("auth", include_str!("scenarios/auth.json")),
    ("blob", include_str!("scenarios/blob.json")),
    ("pubsub", include_str!("scenarios/pubsub.json")),
];

/// Abstracts how a namespaced [`crate::Forge`] is built for a scenario run.
///
/// A single scenario may touch several namespaces (the isolation scenarios do).
/// Every `Forge` a single factory returns must share one backing store, so those
/// scenarios are meaningful — two namespaces on the same store must not see each
/// other's state, but they must agree on what was written when they should.
#[async_trait]
pub trait ForgeFactory: Send + Sync {
    /// Build (or fetch) a Forge bound to `namespace` (empty = default).
    async fn forge(&self, namespace: &str) -> Result<Forge, String>;
}

/// One failed scenario.
pub struct Failure {
    pub primitive: String,
    pub scenario: String,
    pub error: String,
}

/// The outcome of a conformance run.
pub struct Report {
    pub passed: usize,
    pub failures: Vec<Failure>,
}

impl Report {
    /// True when no scenario failed.
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conformance: {} passed, {} failed", self.passed, self.failures.len())?;
        for fail in &self.failures {
            write!(f, "\n  FAIL {}/{}: {}", fail.primitive, fail.scenario, fail.error)?;
        }
        Ok(())
    }
}

/// Run the entire embedded scenario matrix against `factory`, collecting every
/// failure. The kit reports truthfully — it has no notion of "known gaps"; an
/// external backend's gaps are the caller's concern.
pub async fn run_all(factory: &dyn ForgeFactory) -> Report {
    run_matching(factory, None).await
}

/// Run only the scenarios for `primitive` (e.g. `"kv"`).
pub async fn run_primitive(factory: &dyn ForgeFactory, primitive: &str) -> Report {
    run_matching(factory, Some(primitive)).await
}

async fn run_matching(factory: &dyn ForgeFactory, only: Option<&str>) -> Report {
    let mut report = Report { passed: 0, failures: Vec::new() };
    for (primitive, json_text) in SCENARIOS {
        if only.is_some_and(|p| p != *primitive) {
            continue;
        }
        let doc: Value = serde_json::from_str(json_text)
            .unwrap_or_else(|e| panic!("invalid embedded scenario json for {primitive}: {e}"));
        let doc_primitive = doc["primitive"].as_str().unwrap_or(primitive).to_string();
        for scenario in doc["scenarios"].as_array().unwrap() {
            let name = scenario["name"].as_str().unwrap().to_string();
            match run_scenario(factory, scenario).await {
                Ok(()) => report.passed += 1,
                Err(error) => report.failures.push(Failure {
                    primitive: doc_primitive.clone(),
                    scenario: name,
                    error,
                }),
            }
        }
    }
    report
}

/// Run a single scenario (a `{"name", "steps"}` object) against `factory`.
///
/// Exposed so an external driver (or the internal `pg-tests` runner) can apply
/// its own per-scenario bookkeeping around the kit's interpreter.
pub async fn run_one(factory: &dyn ForgeFactory, scenario: &Value) -> Result<(), String> {
    run_scenario(factory, scenario).await
}

async fn run_scenario(factory: &dyn ForgeFactory, scenario: &Value) -> Result<(), String> {
    // One Forge per namespace, all sharing the factory's backing store.
    let mut forges: HashMap<String, Forge> = HashMap::new();
    let mut captures: HashMap<String, Value> = HashMap::new();

    // Leased jobs held for ack/nack, keyed by the receipt the runner returns from
    // dequeue (the job id today; an opaque receipt once P0-1 lands).
    let mut leased: HashMap<String, crate::Job> = HashMap::new();

    // Live pubsub subscriptions held across steps, keyed by the `as` capture name a
    // `pubsub.subscribe` step gives them; a later `pubsub.receive` reads the next message
    // off the named stream. Declared after `forges` so it drops first — the subscription's
    // teardown can still message its broker (held by the owning Forge) before that drops.
    let mut subscriptions: HashMap<String, Subscription> = HashMap::new();

    let steps = scenario["steps"].as_array().ok_or("scenario has no steps")?;
    for (i, step) in steps.iter().enumerate() {
        let op = step["op"].as_str().ok_or("step missing op")?;
        let ns = step.get("namespace").and_then(Value::as_str).unwrap_or("");
        if !forges.contains_key(ns) {
            let forge = factory.forge(ns).await?;
            forges.insert(ns.to_string(), forge);
        }
        let forge = &forges[ns];

        let capture_as = step.get("as").and_then(Value::as_str);
        let args = resolve(step.get("args").cloned().unwrap_or_else(|| json!({})), &captures);
        let outcome =
            dispatch(forge, &mut leased, &mut subscriptions, capture_as, op, &args).await;

        if let (Some(name), Ok(v)) = (capture_as, &outcome) {
            captures.insert(name.to_string(), v.clone());
        }

        match step.get("expect") {
            Some(expect) => check(expect, &outcome).map_err(|e| format!("step {i} ({op}): {e}"))?,
            None => {
                if let Err(e) = outcome {
                    return Err(format!("step {i} ({op}): unexpected error {}", error_code(&e)));
                }
            }
        }
    }
    Ok(())
}

/// Map a canonical op + args onto the Forge API; normalize the result to a `Value`.
async fn dispatch(
    forge: &Forge,
    leased: &mut HashMap<String, crate::Job>,
    subscriptions: &mut HashMap<String, Subscription>,
    capture_as: Option<&str>,
    op: &str,
    args: &Value,
) -> Result<Value, ForgeError> {
    match op {
        "kv.set" | "kv.set_bytes" => {
            let mut opts = SetOpts::new();
            if let Some(ttl) = args.get("ttl_seconds").and_then(Value::as_f64) {
                opts = opts.with_ttl(Duration::from_secs_f64(ttl));
            }
            if args.get("if_exists").and_then(Value::as_bool) == Some(true) {
                opts = opts.with_mode(SetMode::IfExists);
            } else if args.get("if_not_exists").and_then(Value::as_bool) == Some(true) {
                opts = opts.with_mode(SetMode::IfNotExists);
            }
            forge
                .kv()
                .set(arg_str(args, "key"), arg_bytes(args, "value"), opts)
                .await
                .map(Value::Bool)
        }
        "kv.get" | "kv.get_bytes" => {
            Ok(bytes_opt_to_value(forge.kv().get(arg_str(args, "key")).await?))
        }
        "kv.exists" => forge.kv().exists(arg_str(args, "key")).await.map(Value::Bool),
        "kv.delete" => forge.kv().delete(arg_str(args, "key")).await.map(Value::Bool),
        "kv.incr" => forge
            .kv()
            .incr(arg_str(args, "key"), args["by"].as_i64().unwrap())
            .await
            .map(|n| json!(n)),
        "kv.compare_and_swap" => {
            let old = args.get("old").filter(|v| !v.is_null()).map(value_to_bytes);
            forge
                .kv()
                .compare_and_swap(arg_str(args, "key"), old, arg_bytes(args, "new"))
                .await
                .map(Value::Bool)
        }
        "kv.scan_page" => {
            let cursor = args
                .get("cursor")
                .and_then(Value::as_str)
                .map(Cursor::from_token);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32;
            let (keys, next) = forge.kv().scan(arg_str(args, "prefix"), cursor, limit).await?;
            Ok(json!({
                "keys": keys,
                "cursor": next.map(|c| c.token().to_string()),
            }))
        }
        "ratelimit.check" => {
            let algo = match args.get("algo").and_then(|a| a.as_str()) {
                None | Some("token_bucket") => crate::Algo::TokenBucket,
                Some("sliding_window") => crate::Algo::SlidingWindow,
                Some(other) => {
                    return Err(ForgeError::invalid(format!("unknown rate-limit algo {other:?}")));
                }
            };
            let limit = Limit::per_duration(
                args["max"].as_u64().unwrap() as u32,
                Duration::from_secs_f64(args["per_seconds"].as_f64().unwrap()),
            )
            .with_algo(algo);
            let d = forge
                .ratelimit()
                .check(arg_str(args, "bucket"), arg_str(args, "key"), limit)
                .await?;
            Ok(json!({
                "allowed": d.allowed,
                "limit": d.limit,
                "remaining": d.remaining,
                "reset_after_seconds": d.reset_after.as_secs_f64(),
                "retry_after_seconds": d.retry_after.map(|x| x.as_secs_f64()),
            }))
        }
        "schedule.at" => {
            let ms = args["when_epoch_ms"].as_u64().unwrap();
            let when = UNIX_EPOCH + Duration::from_millis(ms);
            forge
                .schedule()
                .at(
                    when,
                    arg_str(args, "queue"),
                    arg_bytes(args, "payload"),
                    schedule_opts(args),
                )
                .await
                .map(|id| json!(id.to_string()))
        }
        "schedule.cron" => {
            forge
                .schedule()
                .cron(
                    arg_str(args, "name"),
                    arg_str(args, "expr"),
                    arg_str(args, "queue"),
                    arg_bytes(args, "payload"),
                    schedule_opts(args),
                )
                .await
                .map(|()| Value::Null)
        }
        "schedule.cancel" => forge
            .schedule()
            .cancel(arg_str(args, "name"))
            .await
            .map(Value::Bool),
        "schedule.cancel_at" => forge
            .schedule()
            .cancel(&format!("at:{}", arg_str(args, "job_id")))
            .await
            .map(Value::Bool),
        "schedule.list" => {
            let cursor = args.get("cursor").and_then(Value::as_str).map(Cursor::from_token);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32;
            let (infos, next) = forge.schedule().list(cursor, limit).await?;
            let arr: Vec<Value> = infos
                .iter()
                .map(|s| {
                    let (kind, cron_expr) = match &s.kind {
                        ScheduleKind::Cron(e) => ("cron", Some(e.clone())),
                        ScheduleKind::At => ("at", None),
                        _ => ("unknown", None),
                    };
                    json!({
                        "name": s.name,
                        "kind": kind,
                        "queue": s.queue,
                        "next_run_ms": epoch_ms(s.next_run),
                        "last_run_ms": s.last_run.map(epoch_ms),
                        "cron_expr": cron_expr,
                    })
                })
                .collect();
            Ok(json!({ "items": arr, "cursor": next.map(|c| c.token().to_string()) }))
        }
        "schedule.tick" => forge
            .run_scheduler_once()
            .await
            .map(|n| json!(n)),
        "queue.enqueue" => {
            let mut opts = crate::EnqueueOpts::new();
            if let Some(m) = args.get("max_attempts").and_then(Value::as_u64) {
                opts = opts.with_max_attempts(m as u32);
            }
            if let Some(d) = args.get("dedup_id").and_then(Value::as_str) {
                opts = opts.with_dedup_id(d);
            }
            if let Some(s) = args.get("delay_seconds").and_then(Value::as_f64) {
                opts = opts.with_delay(Duration::from_secs_f64(s));
            }
            forge
                .queue()
                .enqueue(arg_str(args, "queue"), arg_bytes(args, "payload"), opts)
                .await
                .map(|id| json!(id.to_string()))
        }
        "queue.dequeue" => {
            let mut opts = crate::DequeueOpts::new();
            if let Some(w) = args.get("wait_seconds").and_then(Value::as_f64) {
                opts = opts.with_wait(Duration::from_secs_f64(w));
            }
            if let Some(v) = args.get("visibility_seconds").and_then(Value::as_f64) {
                opts = opts.with_visibility_timeout(Duration::from_secs_f64(v));
            }
            match forge.queue().dequeue(arg_str(args, "queue"), opts).await? {
                None => Ok(Value::Null),
                Some(job) => {
                    let id = job.id.to_string();
                    // The runner is single-delivery, so receipt == id is sufficient here;
                    // the bindings mint a delivery-unique receipt of their own.
                    let receipt = id.clone();
                    let payload = bytes_opt_to_value(Some(job.payload.clone()));
                    let attempt = job.attempt;
                    let max_attempts = job.max_attempts;
                    leased.insert(receipt.clone(), job);
                    Ok(json!({ "id": id, "receipt": receipt, "payload": payload, "attempt": attempt, "max_attempts": max_attempts }))
                }
            }
        }
        "queue.ack" => match leased.remove(arg_str(args, "receipt")) {
            Some(job) => forge.queue().ack(&job).await.map(|()| Value::Null),
            None => Ok(Value::Null),
        },
        "queue.nack" => {
            let opts = match args.get("retry_seconds").and_then(Value::as_f64) {
                Some(s) => crate::NackOpts::retry_in(Duration::from_secs_f64(s)),
                None => crate::NackOpts::default(),
            };
            match leased.remove(arg_str(args, "receipt")) {
                Some(job) => forge.queue().nack(&job, opts).await.map(|()| Value::Null),
                // Mirror the bindings: an unknown receipt means the lease was lost.
                None => Err(ForgeError::precondition("unknown receipt: the lease was lost")),
            }
        }
        "queue.depth" => {
            let d = forge.queue().depth(arg_str(args, "queue")).await?;
            Ok(json!({ "visible": d.visible, "in_flight": d.in_flight, "delayed": d.delayed }))
        }
        "config.set" => forge
            .config()
            .set_raw(arg_str(args, "key"), arg_str(args, "value"))
            .await
            .map(|()| Value::Null),
        "config.get" => Ok(match forge.config().get_raw(arg_str(args, "key")).await? {
            Some(s) => Value::String(s),
            None => Value::Null,
        }),
        "config.flag" => {
            let ctx = match args.get("targeting_key").and_then(Value::as_str) {
                Some(k) => crate::EvalCtx::user(k),
                None => crate::EvalCtx::new(),
            };
            let def = args.get("default").and_then(Value::as_bool).unwrap_or(false);
            Ok(Value::Bool(
                forge.config().flag(arg_str(args, "key"), def, &ctx).await,
            ))
        }
        "config.set_flag_on" => forge
            .config()
            .set_flag(arg_str(args, "key"), crate::FlagRule::On)
            .await
            .map(|()| Value::Null),
        "config.set_flag_off" => forge
            .config()
            .set_flag(arg_str(args, "key"), crate::FlagRule::Off)
            .await
            .map(|()| Value::Null),
        "auth.create_session" => {
            let token = forge
                .auth()
                .create_session(arg_str(args, "user_id"), crate::SessionOpts::default())
                .await?;
            Ok(json!(token.as_str()))
        }
        "auth.validate_session" => Ok(
            match forge.auth().validate_session(arg_str(args, "token")).await? {
                Some(s) => json!(s.user_id),
                None => Value::Null,
            },
        ),
        "auth.revoke_session" => forge
            .auth()
            .revoke_session(arg_str(args, "token"))
            .await
            .map(|()| Value::Null),
        "auth.create_api_key" => {
            let k = forge
                .auth()
                .create_api_key(arg_str(args, "owner_id"), arg_str(args, "label"))
                .await?;
            Ok(json!({
                "id": k.id,
                "secret": k.secret.as_str(),
                "label": k.label,
                "created_at_ms": epoch_ms(k.created_at),
            }))
        }
        "auth.verify_api_key" => Ok(match forge.auth().verify_api_key(arg_str(args, "key")).await? {
            Some(info) => json!(info.owner_id),
            None => Value::Null,
        }),
        "blob.put" => {
            let mut opts = PutOpts::new();
            if let Some(ct) = args.get("content_type").and_then(Value::as_str) {
                opts = opts.with_content_type(ct);
            }
            if let Some(meta) = args.get("metadata").and_then(Value::as_object) {
                for (k, v) in meta {
                    if let Some(vs) = v.as_str() {
                        opts = opts.with_metadata(k, vs);
                    }
                }
            }
            forge
                .blob()
                .put(arg_str(args, "key"), arg_bytes(args, "value"), opts)
                .await
                .map(|()| Value::Null)
        }
        "blob.get" => Ok(bytes_opt_to_value(forge.blob().get(arg_str(args, "key")).await?)),
        "blob.head" => Ok(match forge.blob().head(arg_str(args, "key")).await? {
            None => Value::Null,
            Some(info) => json!({
                "key": info.key,
                "size": info.size,
                "content_type": info.content_type,
                "etag": info.etag,
                "metadata": info.metadata,
                "last_modified_ms": epoch_ms(info.last_modified),
            }),
        }),
        "blob.delete" => forge.blob().delete(arg_str(args, "key")).await.map(Value::Bool),
        "blob.list" => {
            let cursor = args.get("cursor").and_then(Value::as_str).map(Cursor::from_token);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32;
            let page = forge.blob().list(arg_str(args, "prefix"), cursor, limit).await?;
            let keys: Vec<Value> = page.items.iter().map(|i| json!(i.key)).collect();
            let items: Vec<Value> = page
                .items
                .iter()
                .map(|i| {
                    json!({
                        "key": i.key,
                        "size": i.size,
                        "content_type": i.content_type,
                        "etag": i.etag,
                    })
                })
                .collect();
            Ok(json!({
                "keys": keys,
                "items": items,
                "cursor": page.next.map(|c| c.token().to_string()),
            }))
        }
        "blob.presign_download" => {
            let key = arg_str(args, "key");
            let expires = Duration::from_secs_f64(args["expires_seconds"].as_f64().unwrap());
            let url = forge.blob().presign_download(key, expires).await?;
            Ok(presign_to_value(&url, key, "GET"))
        }
        "blob.presign_upload" => {
            let key = arg_str(args, "key");
            let expires = Duration::from_secs_f64(args["expires_seconds"].as_f64().unwrap());
            let max_bytes = args["max_bytes"].as_u64().unwrap();
            let url = forge.blob().presign_upload(key, expires, max_bytes).await?;
            Ok(presign_to_value(&url, key, "PUT"))
        }
        "blob.verify_presigned" => forge
            .blob()
            .verify_presigned(
                arg_str(args, "method"),
                arg_str(args, "key"),
                args["expires_epoch"].as_i64().unwrap(),
                args["max_bytes"].as_u64().unwrap(),
                arg_str(args, "sig"),
            )
            .await
            .map(Value::Bool),
        "pubsub.publish" => forge
            .pubsub()
            .publish(arg_str(args, "topic"), arg_bytes(args, "payload"))
            .await
            .map(|()| Value::Null),
        "pubsub.subscribe" => {
            let name = capture_as.ok_or_else(|| {
                ForgeError::invalid("pubsub.subscribe requires an \"as\" capture name")
            })?;
            let sub = forge.pubsub().subscribe(arg_str(args, "topic")).await?;
            subscriptions.insert(name.to_string(), sub);
            Ok(json!({ "subscribed": true }))
        }
        "pubsub.receive" => {
            let from = arg_str(args, "from");
            let sub = subscriptions.get_mut(from).ok_or_else(|| {
                ForgeError::invalid(format!("pubsub.receive: no subscription captured as {from:?}"))
            })?;
            // Bounded wait so a missing delivery fails the scenario as a timeout instead of
            // hanging the whole suite.
            match tokio::time::timeout(Duration::from_secs(2), sub.next()).await {
                Err(_elapsed) => Ok(json!({ "timeout": true })),
                Ok(None) => Ok(Value::Null),
                Ok(Some(Ok(payload))) => Ok(bytes_opt_to_value(Some(payload))),
                Ok(Some(Err(e))) => Err(e),
            }
        }
        other => panic!("conformance runner has no dispatch for op {other:?}"),
    }
}

fn epoch_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Decompose a presigned URL into the signed params `verify_presigned` needs, so a
/// scenario can `$ref` them straight into a verify step. The original `key`/`method` are
/// passed through rather than re-parsed (the key is percent-encoded in the URL).
fn presign_to_value(url: &str, key: &str, method: &str) -> Value {
    let mut expires_epoch: i64 = 0;
    let mut max_bytes: u64 = 0;
    let mut sig = String::new();
    if let Some((_, query)) = url.split_once('?') {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "expires" => expires_epoch = v.parse().unwrap_or(0),
                    "max_bytes" => max_bytes = v.parse().unwrap_or(0),
                    "sig" => sig = v.to_string(),
                    _ => {}
                }
            }
        }
    }
    json!({
        "url": url,
        "key": key,
        "method": method,
        "expires_epoch": expires_epoch,
        "max_bytes": max_bytes,
        "sig": sig,
    })
}

fn arg_str<'a>(args: &'a Value, name: &str) -> &'a str {
    args[name]
        .as_str()
        .unwrap_or_else(|| panic!("arg {name:?} must be a string"))
}

fn arg_bytes(args: &Value, name: &str) -> Bytes {
    value_to_bytes(&args[name])
}

fn schedule_opts(args: &Value) -> crate::ScheduleOpts {
    let mut opts = crate::ScheduleOpts::new();
    if let Some(m) = args.get("max_attempts").and_then(Value::as_u64) {
        opts = opts.with_max_attempts(m as u32);
    }
    opts
}

/// A byte-valued argument is either a UTF-8 string or `{"$bytes": [..]}`.
fn value_to_bytes(v: &Value) -> Bytes {
    if let Some(s) = v.as_str() {
        return Bytes::from(s.as_bytes().to_vec());
    }
    if let Some(arr) = v.get("$bytes").and_then(Value::as_array) {
        let bytes: Vec<u8> = arr.iter().map(|n| n.as_u64().unwrap() as u8).collect();
        return Bytes::from(bytes);
    }
    panic!("cannot read bytes from {v}")
}

fn bytes_opt_to_value(b: Option<Bytes>) -> Value {
    match b {
        None => Value::Null,
        Some(b) => json!({ "$bytes": b.iter().map(|x| u64::from(*x)).collect::<Vec<_>>() }),
    }
}

/// Replace `{"$ref": "name.path"}` nodes with captured step results, and
/// `{"$now_ms": <offset>}` nodes with the current epoch ms plus the offset (so a
/// time-relative schedule scenario can express "now" / "now + N ms" portably).
fn resolve(v: Value, captures: &HashMap<String, Value>) -> Value {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(path)) = map.get("$ref") {
                return resolve_ref(path, captures);
            }
            if let Some(offset) = map.get("$now_ms").and_then(Value::as_i64) {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                return json!(now + offset);
            }
            Value::Object(map.into_iter().map(|(k, x)| (k, resolve(x, captures))).collect())
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(|x| resolve(x, captures)).collect()),
        other => other,
    }
}

fn resolve_ref(path: &str, captures: &HashMap<String, Value>) -> Value {
    let mut parts = path.split('.');
    let head = parts.next().unwrap();
    let mut cur = captures
        .get(head)
        .unwrap_or_else(|| panic!("$ref to unknown capture {head:?}"))
        .clone();
    for p in parts {
        cur = cur
            .get(p)
            .unwrap_or_else(|| panic!("$ref path {path:?} missing field {p:?}"))
            .clone();
    }
    cur
}

fn check(expect: &Value, outcome: &Result<Value, ForgeError>) -> Result<(), String> {
    if let Some(code) = expect.get("error").and_then(Value::as_str) {
        return match outcome {
            Err(e) if error_code(e) == code => Ok(()),
            Err(e) => Err(format!("expected error {code}, got {}", error_code(e))),
            Ok(v) => Err(format!("expected error {code}, got value {v}")),
        };
    }
    let actual = match outcome {
        Ok(v) => v,
        Err(e) => return Err(format!("expected a value, got error {}", error_code(e))),
    };
    if let Some(exp) = expect.get("value") {
        return check_value(exp, actual);
    }
    if let Some(exp) = expect.get("bytes") {
        return check_bytes(exp, actual);
    }
    if let Some(exp) = expect.get("shape") {
        return if deep_match(exp, actual) {
            Ok(())
        } else {
            Err(format!("shape mismatch: expected {exp}, got {actual}"))
        };
    }
    Err("expect block has none of value/bytes/shape/error".into())
}

fn check_value(exp: &Value, actual: &Value) -> Result<(), String> {
    // A string expectation against a byte return reconciles via UTF-8.
    if let (Some(s), Some(bytes)) = (exp.as_str(), as_bytes_value(actual)) {
        let got = String::from_utf8(bytes).map_err(|_| "byte value is not UTF-8".to_string())?;
        return if got == s {
            Ok(())
        } else {
            Err(format!("expected {s:?}, got {got:?}"))
        };
    }
    if deep_match(exp, actual) {
        Ok(())
    } else {
        Err(format!("expected {exp}, got {actual}"))
    }
}

fn check_bytes(exp: &Value, actual: &Value) -> Result<(), String> {
    let want = exp
        .get("$bytes")
        .and_then(Value::as_array)
        .ok_or("bytes expectation must be {\"$bytes\": [..]}")?;
    match as_bytes_value(actual) {
        Some(got) => {
            let want: Vec<u8> = want.iter().map(|n| n.as_u64().unwrap() as u8).collect();
            if got == want {
                Ok(())
            } else {
                Err(format!("byte mismatch: expected {want:?}, got {got:?}"))
            }
        }
        None => Err(format!("expected a byte value, got {actual}")),
    }
}

fn as_bytes_value(v: &Value) -> Option<Vec<u8>> {
    v.get("$bytes")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(|n| n.as_u64().unwrap_or(0) as u8).collect())
}

/// Structural match with matcher support (`$type`, `$approx`, `$bytes`).
fn deep_match(exp: &Value, actual: &Value) -> bool {
    // A string expectation reconciles against a byte value via UTF-8 (queue/kv payloads).
    if let (Some(s), Some(bytes)) = (exp.as_str(), as_bytes_value(actual)) {
        return String::from_utf8(bytes).map(|g| g == s).unwrap_or(false);
    }
    if let Some(obj) = exp.as_object() {
        if let Some(t) = obj.get("$type").and_then(Value::as_str) {
            return type_matches(t, actual);
        }
        if let Some(n) = obj.get("$approx").and_then(Value::as_f64) {
            let tol = obj.get("tol").and_then(Value::as_f64).unwrap_or(0.0);
            return actual.as_f64().is_some_and(|a| (a - n).abs() <= tol);
        }
        if obj.contains_key("$bytes") {
            return as_bytes_value(exp) == as_bytes_value(actual);
        }
        let Some(aobj) = actual.as_object() else {
            return false;
        };
        return obj
            .iter()
            .all(|(k, v)| aobj.get(k).is_some_and(|av| deep_match(v, av)));
    }
    if let Some(arr) = exp.as_array() {
        let Some(aarr) = actual.as_array() else {
            return false;
        };
        return arr.len() == aarr.len() && arr.iter().zip(aarr).all(|(e, a)| deep_match(e, a));
    }
    exp == actual
}

fn type_matches(t: &str, actual: &Value) -> bool {
    match t {
        "string" => actual.is_string(),
        "number" => actual.is_number(),
        "boolean" => actual.is_boolean(),
        "array" => actual.is_array(),
        "object" => actual.is_object(),
        "null" => actual.is_null(),
        other => panic!("unknown $type matcher {other:?}"),
    }
}

fn error_code(e: &ForgeError) -> &'static str {
    match e {
        ForgeError::Config(_) => "Config",
        ForgeError::Unavailable(_) => "Unavailable",
        ForgeError::NotFound => "NotFound",
        ForgeError::Precondition(_) => "Precondition",
        ForgeError::Limit(_) => "Limit",
        ForgeError::Invalid(_) => "Invalid",
        ForgeError::Backend { .. } => "Backend",
        _ => "Backend",
    }
}

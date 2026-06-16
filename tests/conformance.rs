//! Cross-language conformance runner — Rust side.
//!
//! Reads `conformance/scenarios/*.json` and runs each scenario against a fresh
//! throwaway Forge, asserting the observable result matches the canonical
//! contract. The Node and Python runners execute the *same* JSON. See
//! `conformance/README.md`.
//!
//! Rust is the reference shape, so `known_gaps.json` lists no `rust` gaps and
//! this test asserts every scenario passes. A failure here means either a real
//! core bug or a scenario that does not match the implemented contract.
//!
//! Run with: `cargo test --features pg-tests --test conformance` (needs
//! `TEST_DATABASE_URL`).
#![cfg(feature = "pg-tests")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::print_stdout
)]

use bytes::Bytes;
use forge::testing::TestDatabase;
use forge::{Cursor, Forge, ForgeConfig, ForgeError, Limit, ScheduleKind, SetMode, SetOpts};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCENARIO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/scenarios");
const GAPS_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/known_gaps.json");

#[tokio::test]
async fn conformance_rust() {
    let gaps = load_rust_gaps();
    let mut passed = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for file in scenario_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let doc: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("invalid scenario json {}: {e}", file.display()));
        let primitive = doc["primitive"].as_str().unwrap().to_string();
        for scenario in doc["scenarios"].as_array().unwrap() {
            let name = scenario["name"].as_str().unwrap().to_string();
            let key = (primitive.clone(), name.clone());
            let result = run_scenario(scenario).await;
            let expected_fail = gaps.contains(&key);
            match (result, expected_fail) {
                (Ok(()), false) => {
                    passed += 1;
                    println!("PASS  {primitive}/{name}");
                }
                (Err(e), true) => {
                    passed += 1;
                    println!("XFAIL {primitive}/{name}: {e}");
                }
                (Ok(()), true) => problems.push(format!(
                    "{primitive}/{name}: PASSED but is a registered rust gap — remove it from known_gaps.json"
                )),
                (Err(e), false) => problems.push(format!("{primitive}/{name}: {e}")),
            }
        }
    }

    println!("\nconformance(rust): {passed} ok, {} unexpected", problems.len());
    assert!(
        problems.is_empty(),
        "unexpected conformance results:\n  {}",
        problems.join("\n  ")
    );
}

/// `(primitive, scenario)` pairs registered as expected-fail for the `rust` runner.
fn load_rust_gaps() -> std::collections::HashSet<(String, String)> {
    let text = std::fs::read_to_string(GAPS_FILE).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let mut set = std::collections::HashSet::new();
    for gap in doc["gaps"].as_array().unwrap() {
        let langs = gap["languages"].as_array().unwrap();
        if langs.iter().any(|l| l.as_str() == Some("rust")) {
            set.insert((
                gap["primitive"].as_str().unwrap().to_string(),
                gap["scenario"].as_str().unwrap().to_string(),
            ));
        }
    }
    set
}

fn scenario_files() -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(SCENARIO_DIR)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

async fn run_scenario(scenario: &Value) -> Result<(), String> {
    let db = TestDatabase::new().await.map_err(|e| format!("db setup: {e}"))?;
    // One Forge per namespace, all on this scenario's database.
    let mut forges: HashMap<String, Forge> = HashMap::new();
    let mut captures: HashMap<String, Value> = HashMap::new();

    // Leased jobs held for ack/nack, keyed by the receipt the runner returns from
    // dequeue (the job id today; an opaque receipt once P0-1 lands).
    let mut leased: HashMap<String, forge::Job> = HashMap::new();

    let steps = scenario["steps"].as_array().ok_or("scenario has no steps")?;
    for (i, step) in steps.iter().enumerate() {
        let op = step["op"].as_str().ok_or("step missing op")?;
        let ns = step.get("namespace").and_then(Value::as_str).unwrap_or("");
        if !forges.contains_key(ns) {
            let forge = Forge::init(ForgeConfig::new(db.url()).with_kv_namespace(ns))
                .await
                .map_err(|e| format!("forge init (ns {ns:?}): {e}"))?;
            forges.insert(ns.to_string(), forge);
        }
        let forge = &forges[ns];

        let args = resolve(step.get("args").cloned().unwrap_or_else(|| json!({})), &captures);
        let outcome = dispatch(forge, &mut leased, op, &args).await;

        if let Some(name) = step.get("as").and_then(Value::as_str) {
            if let Ok(v) = &outcome {
                captures.insert(name.to_string(), v.clone());
            }
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
    leased: &mut HashMap<String, forge::Job>,
    op: &str,
    args: &Value,
) -> Result<Value, ForgeError> {
    match op {
        "kv.set" => {
            let mut opts = SetOpts::new();
            if let Some(ttl) = args.get("ttl_seconds").and_then(Value::as_f64) {
                opts = opts.with_ttl(Duration::from_secs_f64(ttl));
            }
            if args.get("if_not_exists").and_then(Value::as_bool) == Some(true) {
                opts = opts.with_mode(SetMode::IfNotExists);
            }
            forge
                .kv()
                .set(arg_str(args, "key"), arg_bytes(args, "value"), opts)
                .await
                .map(Value::Bool)
        }
        "kv.get" => Ok(bytes_opt_to_value(forge.kv().get(arg_str(args, "key")).await?)),
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
            let limit = Limit::per_duration(
                args["max"].as_u64().unwrap() as u32,
                Duration::from_secs_f64(args["per_seconds"].as_f64().unwrap()),
            );
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
                .at(when, arg_str(args, "queue"), arg_bytes(args, "payload"))
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
                )
                .await
                .map(|()| Value::Null)
        }
        "schedule.cancel" => forge
            .schedule()
            .cancel(arg_str(args, "name"))
            .await
            .map(Value::Bool),
        "schedule.list" => {
            let infos = forge.schedule().list().await?;
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
            Ok(Value::Array(arr))
        }
        "queue.enqueue" => {
            let mut opts = forge::EnqueueOpts::new();
            if let Some(m) = args.get("max_attempts").and_then(Value::as_u64) {
                opts = opts.with_max_attempts(m as u32);
            }
            if let Some(d) = args.get("dedup_id").and_then(Value::as_str) {
                opts = opts.with_dedup_id(d);
            }
            forge
                .queue()
                .enqueue(arg_str(args, "queue"), arg_bytes(args, "payload"), opts)
                .await
                .map(|id| json!(id.to_string()))
        }
        "queue.dequeue" => {
            let mut opts = forge::DequeueOpts::new();
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
                    let payload = bytes_opt_to_value(Some(job.payload.clone()));
                    let attempt = job.attempt;
                    leased.insert(id.clone(), job);
                    Ok(json!({ "id": id, "payload": payload, "attempt": attempt }))
                }
            }
        }
        "queue.ack" => match leased.remove(arg_str(args, "receipt")) {
            Some(job) => forge.queue().ack(&job).await.map(|()| Value::Null),
            None => Ok(Value::Null),
        },
        "queue.nack" => {
            let opts = match args.get("retry_seconds").and_then(Value::as_f64) {
                Some(s) => forge::NackOpts::retry_in(Duration::from_secs_f64(s)),
                None => forge::NackOpts::default(),
            };
            match leased.remove(arg_str(args, "receipt")) {
                Some(job) => forge.queue().nack(&job, opts).await.map(|()| Value::Null),
                None => Ok(Value::Null),
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
                Some(k) => forge::EvalCtx::user(k),
                None => forge::EvalCtx::new(),
            };
            let def = args.get("default").and_then(Value::as_bool).unwrap_or(false);
            Ok(Value::Bool(
                forge.config().flag(arg_str(args, "key"), def, &ctx).await,
            ))
        }
        "config.set_flag_on" => forge
            .config()
            .set_flag(arg_str(args, "key"), forge::FlagRule::On)
            .await
            .map(|()| Value::Null),
        "config.set_flag_off" => forge
            .config()
            .set_flag(arg_str(args, "key"), forge::FlagRule::Off)
            .await
            .map(|()| Value::Null),
        "auth.create_session" => {
            let token = forge
                .auth()
                .create_session(arg_str(args, "user_id"), forge::SessionOpts::default())
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
        other => panic!("rust conformance runner has no dispatch for op {other:?}"),
    }
}

fn epoch_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

// ---- argument helpers ----

fn arg_str<'a>(args: &'a Value, name: &str) -> &'a str {
    args[name]
        .as_str()
        .unwrap_or_else(|| panic!("arg {name:?} must be a string"))
}

fn arg_bytes(args: &Value, name: &str) -> Bytes {
    value_to_bytes(&args[name])
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

/// Replace `{"$ref": "name.path"}` nodes with captured step results.
fn resolve(v: Value, captures: &HashMap<String, Value>) -> Value {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(path)) = map.get("$ref") {
                return resolve_ref(path, captures);
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

// ---- expectation checking ----

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

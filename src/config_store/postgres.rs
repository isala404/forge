use super::{
    CACHE_TTL_SECS, ConfigEntry, ConfigStore, EvalCtx, FlagEvaluation, FlagEvaluationEntry,
    FlagEvaluationRequest, FlagRule, MAX_ALLOWLIST_ENTRIES, MAX_KEY_BYTES, MAX_VALUE_BYTES,
    capture_environment_overrides, check_bulk_len, environment_override,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::util::key_hash;
use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use sqlx::types::Json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::Instrument;
use tracing::field::Empty;

const CACHE_TTL: Duration = Duration::from_secs(CACHE_TTL_SECS);
/// Largest allowed `AllowList` entry, in bytes. Over => `Limit`.
const MAX_ALLOWLIST_ENTRY_BYTES: usize = 256;
/// Cap on per-process cache entries, so a caller deriving config/flag keys from
/// user input can't grow the map without bound.
const MAX_CACHE_ENTRIES: usize = 10_000;

struct Cached<T> {
    value: T,
    fetched: Instant,
}

/// Bound a cache before inserting: purge stale entries, then reset entirely if a
/// flood of fresh distinct keys is still at the cap (it is only a cache).
fn cap_cache<T>(cache: &mut HashMap<String, Cached<T>>) {
    if cache.len() < MAX_CACHE_ENTRIES {
        return;
    }
    cache.retain(|_, e| e.fetched.elapsed() < CACHE_TTL);
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.clear();
    }
}

/// Read a fresh cached value (within the TTL), cloned out. A poisoned lock, or a stale or
/// absent entry, reads as a miss (the caller falls through to the DB), never a panic.
fn cache_get<T: Clone>(
    cache: &Mutex<HashMap<String, Cached<T>>>,
    key: &str,
) -> Option<(T, Duration)> {
    let cache = cache.lock().ok()?;
    let entry = cache.get(key)?;
    let age = entry.fetched.elapsed();
    (age < CACHE_TTL).then(|| (entry.value.clone(), age))
}

/// Insert into a bounded cache (purging first if at the cap). A poisoned lock skips the
/// write silently; the cache is only an optimization, never a source of truth.
fn cache_put<T>(cache: &Mutex<HashMap<String, Cached<T>>>, key: &str, value: T) {
    if let Ok(mut cache) = cache.lock() {
        cap_cache(&mut cache);
        cache.insert(
            key.to_string(),
            Cached {
                value,
                fetched: Instant::now(),
            },
        );
    }
}

pub(crate) struct PgConfig {
    pool: PgPool,
    /// Namespace prefix applied to stored config/flag keys, so apps sharing a
    /// database don't collide. Empty = no prefix. The per-instance cache and the
    /// `FORGE_CFG_<KEY>` env override use the caller (logical) key.
    namespace: String,
    environment: HashMap<String, String>,
    values: Arc<Mutex<HashMap<String, Cached<Option<String>>>>>,
    flags: Arc<Mutex<HashMap<String, Cached<Option<FlagRule>>>>>,
    stop_invalidation: watch::Sender<bool>,
}

impl PgConfig {
    pub(crate) fn new(pool: PgPool, namespace: String) -> Self {
        let values = Arc::new(Mutex::new(HashMap::new()));
        let flags = Arc::new(Mutex::new(HashMap::new()));
        let (stop_invalidation, stop) = watch::channel(false);
        tokio::spawn(run_invalidation_listener(
            pool.clone(),
            key_hash(&namespace),
            values.clone(),
            flags.clone(),
            stop,
        ));
        Self {
            pool,
            namespace,
            environment: capture_environment_overrides(),
            values,
            flags,
            stop_invalidation,
        }
    }

    /// Stored key for a caller key, applying the namespace prefix.
    fn physical(&self, key: &str) -> String {
        crate::util::namespaced(&self.namespace, key)
    }

    /// Fetch the stored raw value, using the cache. Errors only on a real backend fault.
    async fn fetch_value(&self, key: &str) -> Result<Option<String>> {
        if let Some((hit, _)) = cache_get(&self.values, key) {
            return Ok(hit);
        }
        let value = sqlx::query_scalar!(
            "SELECT value FROM forge_config WHERE key = $1",
            self.physical(key)
        )
        .fetch_optional(&self.pool)
        .await?;
        cache_put(&self.values, key, value.clone());
        Ok(value)
    }

    async fn fetch_flag(&self, key: &str) -> Result<Option<FlagRule>> {
        if let Some((hit, _)) = cache_get(&self.flags, key) {
            return Ok(hit);
        }
        let rule = sqlx::query_scalar!(
            r#"SELECT rule AS "rule!: Json<FlagRule>" FROM forge_flags WHERE key = $1"#,
            self.physical(key)
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|j| j.0);
        cache_put(&self.flags, key, rule.clone());
        Ok(rule)
    }
}

impl Drop for PgConfig {
    fn drop(&mut self) {
        let _ = self.stop_invalidation.send(true);
    }
}

const INVALIDATION_CHANNEL: &str = "forge_config_invalidate";

fn invalidate_matching<T>(cache: &Mutex<HashMap<String, Cached<T>>>, hash: &str) {
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|key, _| key_hash(key) != hash);
    }
}

async fn run_invalidation_listener(
    pool: PgPool,
    namespace_hash: String,
    values: Arc<Mutex<HashMap<String, Cached<Option<String>>>>>,
    flags: Arc<Mutex<HashMap<String, Cached<Option<FlagRule>>>>>,
    mut stop: watch::Receiver<bool>,
) {
    let mut retry = Duration::from_millis(100);
    loop {
        if *stop.borrow() {
            return;
        }
        let connected = tokio::select! {
            result = PgListener::connect_with(&pool) => result,
            _ = stop.changed() => return,
        };
        let mut listener = match connected {
            Ok(listener) => listener,
            Err(_) => {
                tokio::select! {
                    _ = tokio::time::sleep(retry) => {},
                    _ = stop.changed() => return,
                }
                retry = (retry * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        if listener.listen(INVALIDATION_CHANNEL).await.is_err() {
            continue;
        }
        retry = Duration::from_millis(100);
        loop {
            let notification = tokio::select! {
                result = listener.recv() => result,
                _ = stop.changed() => return,
            };
            let Ok(notification) = notification else {
                break;
            };
            let mut parts = notification.payload().split(':');
            let Some(incoming_namespace) = parts.next() else {
                continue;
            };
            let Some(kind) = parts.next() else { continue };
            let Some(hash) = parts.next() else { continue };
            if parts.next().is_some() || incoming_namespace != namespace_hash {
                continue;
            }
            match kind {
                "value" => invalidate_matching(&values, hash),
                "flag" => invalidate_matching(&flags, hash),
                _ => continue,
            }
        }
    }
}

fn invalidation_payload(namespace: &str, kind: &str, key: &str) -> String {
    format!("{}:{kind}:{}", key_hash(namespace), key_hash(key))
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(ForgeError::invalid("config key must not be empty"));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(ForgeError::invalid(format!(
            "config key is {} bytes; max is {MAX_KEY_BYTES}",
            key.len()
        )));
    }
    Ok(())
}

fn check_rule(rule: &FlagRule) -> Result<()> {
    match rule {
        FlagRule::Percent(p) if *p > 100 => {
            Err(ForgeError::invalid("Percent(p) must have p in 0..=100"))
        }
        FlagRule::AllowList(list) if list.len() > MAX_ALLOWLIST_ENTRIES => {
            Err(ForgeError::limit(format!(
                "AllowList has {} entries; max is {MAX_ALLOWLIST_ENTRIES}",
                list.len()
            )))
        }
        FlagRule::AllowList(list) if list.iter().any(|e| e.len() > MAX_ALLOWLIST_ENTRY_BYTES) => {
            Err(ForgeError::limit(format!(
                "an AllowList entry exceeds {MAX_ALLOWLIST_ENTRY_BYTES} bytes"
            )))
        }
        FlagRule::Value { value, variant }
            if value.to_string().len() > MAX_VALUE_BYTES || variant.len() > 128 =>
        {
            Err(ForgeError::limit(
                "typed flag value exceeds 64 KiB or variant exceeds 128 bytes",
            ))
        }
        _ => Ok(()),
    }
}

/// Stable bucket in `0..100` for `(flag_key, targeting_key)`. Uses the crate's
/// sha256-based hash so the bucket is identical across instances and deploys (never
/// `DefaultHasher`, whose seed is per-process).
fn stable_bucket(flag_key: &str, targeting_key: &str) -> u32 {
    let hex = crate::util::sha256_hex(format!("{flag_key}:{targeting_key}").as_bytes());
    let prefix = hex.get(..8).unwrap_or("0");
    u32::from_str_radix(prefix, 16).unwrap_or(0) % 100
}

/// Evaluate a resolved rule against the context. Returns the boolean and an
/// OpenFeature-style reason for obs.
fn evaluate(key: &str, rule: &FlagRule, default: bool, ctx: &EvalCtx) -> (bool, &'static str) {
    match rule {
        FlagRule::On => (true, "static"),
        FlagRule::Off => (false, "static"),
        FlagRule::Percent(p) => match &ctx.targeting_key {
            Some(k) => {
                if stable_bucket(key, k) < u32::from(*p) {
                    (true, "percent_in")
                } else {
                    (false, "percent_out")
                }
            }
            None => (default, "default_no_key"),
        },
        FlagRule::AllowList(list) => match &ctx.targeting_key {
            Some(k) if list.iter().any(|e| e == k) => (true, "targeting_match"),
            _ => (false, "targeting_miss"),
        },
        FlagRule::Value { value, .. } => value
            .as_bool()
            .map_or((default, "default_type_mismatch"), |value| {
                (value, "static")
            }),
    }
}

fn evaluate_details(
    key: &str,
    rule: &FlagRule,
    default: &serde_json::Value,
    ctx: &EvalCtx,
) -> FlagEvaluation {
    match rule {
        FlagRule::Value { value, variant } => {
            FlagEvaluation::new(value, Some(variant.clone()), "static", None)
        }
        FlagRule::On => FlagEvaluation::new(
            &serde_json::Value::Bool(true),
            Some("on".into()),
            "static",
            None,
        ),
        FlagRule::Off => FlagEvaluation::new(
            &serde_json::Value::Bool(false),
            Some("off".into()),
            "static",
            None,
        ),
        FlagRule::Percent(percent) => match &ctx.targeting_key {
            Some(targeting_key) => {
                let enabled = stable_bucket(key, targeting_key) < u32::from(*percent);
                FlagEvaluation::new(
                    &serde_json::Value::Bool(enabled),
                    Some(if enabled { "on" } else { "off" }.into()),
                    if enabled { "percent_in" } else { "percent_out" },
                    None,
                )
            }
            None => FlagEvaluation::new(default, None, "default_no_key", None),
        },
        FlagRule::AllowList(entries) => match &ctx.targeting_key {
            Some(targeting_key) => {
                let enabled = entries.iter().any(|entry| entry == targeting_key);
                FlagEvaluation::new(
                    &serde_json::Value::Bool(enabled),
                    Some(if enabled { "on" } else { "off" }.into()),
                    if enabled {
                        "targeting_match"
                    } else {
                        "targeting_miss"
                    },
                    None,
                )
            }
            None => FlagEvaluation::new(
                &serde_json::Value::Bool(false),
                Some("off".into()),
                "targeting_miss",
                None,
            ),
        },
    }
}

#[async_trait]
impl ConfigStore for PgConfig {
    async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        let span = tracing::info_span!(
            "forge.config.get_raw",
            config.key_hash = %key_hash(key),
            config.source = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("config", "get_raw", span, async move {
            check_key(key)?;
            // env `FORGE_CFG_<KEY>` wins over the store, even when set to empty (12-factor).
            if let Some(value) = environment_override(&self.environment, key) {
                tracing::Span::current().record("config.source", "env");
                return Ok(Some(value.to_string()));
            }
            let value = self.fetch_value(key).await?;
            tracing::Span::current().record(
                "config.source",
                if value.is_some() { "store" } else { "unset" },
            );
            Ok(value)
        })
        .await
    }

    async fn get_many_raw(&self, keys: &[String]) -> Result<Vec<ConfigEntry>> {
        check_bulk_len(keys.len())?;
        for key in keys {
            check_key(key)?;
        }
        let store_keys: Vec<&String> = keys
            .iter()
            .filter(|key| environment_override(&self.environment, key).is_none())
            .collect();
        let physical: Vec<String> = store_keys.iter().map(|key| self.physical(key)).collect();
        let values: HashMap<String, String> = if physical.is_empty() {
            HashMap::new()
        } else {
            sqlx::query!(
                "SELECT key, value FROM forge_config WHERE key = ANY($1)",
                &physical
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| (row.key, row.value))
            .collect()
        };
        for key in store_keys {
            cache_put(&self.values, key, values.get(&self.physical(key)).cloned());
        }
        Ok(keys
            .iter()
            .map(|key| ConfigEntry {
                key: key.clone(),
                value: environment_override(&self.environment, key)
                    .map(str::to_string)
                    .or_else(|| values.get(&self.physical(key)).cloned()),
            })
            .collect())
    }

    async fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        let span = tracing::info_span!(
            "forge.config.set_raw",
            config.key_hash = %key_hash(key),
            config.value_bytes = value.len(),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("config", "set_raw", span, async move {
            check_key(key)?;
            if value.len() > MAX_VALUE_BYTES {
                return Err(ForgeError::limit(format!(
                    "config value is {} bytes; max is {MAX_VALUE_BYTES}",
                    value.len()
                )));
            }
            sqlx::query!(
                "INSERT INTO forge_config (key, value) VALUES ($1, $2) \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
                self.physical(key),
                value,
            )
            .execute(&self.pool)
            .await?;
            sqlx::query!(
                "SELECT pg_notify('forge_config_invalidate', $1)",
                invalidation_payload(&self.namespace, "value", key),
            )
            .execute(&self.pool)
            .await?;
            // Read-your-writes locally; other instances converge within the cache TTL.
            cache_put(&self.values, key, Some(value.to_string()));
            Ok(())
        })
        .await
    }

    async fn flag(&self, key: &str, default: bool, ctx: &EvalCtx) -> bool {
        let span = tracing::info_span!(
            "forge.config.flag",
            config.key_hash = %key_hash(key),
            flag.result = Empty,
            flag.reason = Empty,
        );
        async move {
            let (result, reason) = match self.fetch_flag(key).await {
                Ok(Some(rule)) => evaluate(key, &rule, default, ctx),
                Ok(None) => (default, "default_missing"),
                Err(e) => {
                    tracing::warn!(error = %e, "flag lookup failed; resolving to default");
                    (default, "default_error")
                }
            };
            let s = tracing::Span::current();
            s.record("flag.result", result);
            s.record("flag.reason", reason);
            result
        }
        .instrument(span)
        .await
    }

    async fn flag_details(
        &self,
        key: &str,
        default: &serde_json::Value,
        ctx: &EvalCtx,
    ) -> FlagEvaluation {
        match self.fetch_flag(key).await {
            Ok(Some(rule)) => evaluate_details(key, &rule, default, ctx),
            Ok(None) => FlagEvaluation::new(default, None, "default_missing", None),
            Err(error) => FlagEvaluation::new(
                default,
                None,
                "default_error",
                Some(crate::obs::error_variant(&error).to_ascii_uppercase()),
            ),
        }
    }

    async fn flag_details_many(
        &self,
        requests: &[FlagEvaluationRequest],
    ) -> Result<Vec<FlagEvaluationEntry>> {
        check_bulk_len(requests.len())?;

        let mut physical = Vec::new();
        for request in requests {
            if check_key(&request.key).is_ok() {
                physical.push(self.physical(&request.key));
            }
        }
        physical.sort();
        physical.dedup();
        let fetched: Result<HashMap<String, FlagRule>> = if physical.is_empty() {
            Ok(HashMap::new())
        } else {
            sqlx::query!(
                r#"SELECT key, rule AS "rule!: Json<FlagRule>" FROM forge_flags WHERE key = ANY($1)"#,
                &physical
            )
                .fetch_all(&self.pool)
                .await
                .map_err(ForgeError::from_sqlx)
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| (row.key, row.rule.0))
                        .collect()
                })
        };
        let rules = fetched.as_ref().ok();
        let fetch_error_code = fetched
            .as_ref()
            .err()
            .map(|error| obs::error_variant(error).to_ascii_uppercase());
        if let Some(rules) = &rules {
            for request in requests {
                if check_key(&request.key).is_ok() {
                    cache_put(
                        &self.flags,
                        &request.key,
                        rules.get(&self.physical(&request.key)).cloned(),
                    );
                }
            }
        }

        Ok(requests
            .iter()
            .map(|request| {
                let evaluation = if check_key(&request.key).is_err() {
                    FlagEvaluation::new(
                        &request.default,
                        None,
                        "default_error",
                        Some("INVALID".into()),
                    )
                } else if let Some(rules) = &rules {
                    rules.get(&self.physical(&request.key)).map_or_else(
                        || FlagEvaluation::new(&request.default, None, "default_missing", None),
                        |rule| {
                            evaluate_details(&request.key, rule, &request.default, &request.context)
                        },
                    )
                } else {
                    FlagEvaluation::new(
                        &request.default,
                        None,
                        "default_error",
                        fetch_error_code.clone(),
                    )
                };
                FlagEvaluationEntry {
                    id: request.id.clone(),
                    key: request.key.clone(),
                    evaluation,
                }
            })
            .collect())
    }

    async fn set_flag(&self, key: &str, rule: FlagRule) -> Result<()> {
        let span = tracing::info_span!(
            "forge.config.set_flag",
            config.key_hash = %key_hash(key),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("config", "set_flag", span, async move {
            check_key(key)?;
            check_rule(&rule)?;
            sqlx::query!(
                "INSERT INTO forge_flags (key, rule) VALUES ($1, $2) \
                 ON CONFLICT (key) DO UPDATE SET rule = EXCLUDED.rule",
                self.physical(key),
                Json(&rule) as _,
            )
            .execute(&self.pool)
            .await?;
            sqlx::query!(
                "SELECT pg_notify('forge_config_invalidate', $1)",
                invalidation_payload(&self.namespace, "flag", key),
            )
            .execute(&self.pool)
            .await?;
            cache_put(&self.flags, key, Some(rule));
            Ok(())
        })
        .await
    }

    async fn delete_raw(&self, key: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.config.delete_raw",
            config.key_hash = %key_hash(key),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("config", "delete_raw", span, async move {
            check_key(key)?;
            let removed = sqlx::query!(
                "DELETE FROM forge_config WHERE key = $1",
                self.physical(key)
            )
            .execute(&self.pool)
            .await?
            .rows_affected()
                > 0;
            sqlx::query!(
                "SELECT pg_notify('forge_config_invalidate', $1)",
                invalidation_payload(&self.namespace, "value", key),
            )
            .execute(&self.pool)
            .await?;
            // Cache the absence locally; other instances converge within the cache TTL.
            cache_put(&self.values, key, None);
            Ok(removed)
        })
        .await
    }

    async fn delete_flag(&self, key: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.config.delete_flag",
            config.key_hash = %key_hash(key),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("config", "delete_flag", span, async move {
            check_key(key)?;
            let removed =
                sqlx::query!("DELETE FROM forge_flags WHERE key = $1", self.physical(key))
                    .execute(&self.pool)
                    .await?
                    .rows_affected()
                    > 0;
            sqlx::query!(
                "SELECT pg_notify('forge_config_invalidate', $1)",
                invalidation_payload(&self.namespace, "flag", key),
            )
            .execute(&self.pool)
            .await?;
            cache_put(&self.flags, key, None);
            Ok(removed)
        })
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn percent_bucket_is_stable_and_namespaced() {
        let a = stable_bucket("flag_a", "user-1");
        assert_eq!(
            a,
            stable_bucket("flag_a", "user-1"),
            "same inputs => same bucket"
        );
        assert!(a < 100);
        // Namespacing: different flag keys must produce independent buckets for the same user.
        assert_ne!(
            stable_bucket("flag_a", "user-1"),
            stable_bucket("flag_b", "user-1")
        );
    }

    #[test]
    fn evaluate_covers_the_resolution_table() {
        let with = EvalCtx::user("u");
        let without = EvalCtx::new();
        assert!(evaluate("f", &FlagRule::On, false, &with).0);
        assert!(!evaluate("f", &FlagRule::Off, true, &with).0);
        assert!(evaluate("f", &FlagRule::Percent(100), false, &with).0);
        assert!(!evaluate("f", &FlagRule::Percent(0), true, &with).0);
        // Percent with no key falls back to the caller default.
        assert!(evaluate("f", &FlagRule::Percent(50), true, &without).0);
        assert!(!evaluate("f", &FlagRule::Percent(50), false, &without).0);
        let list = FlagRule::AllowList(vec!["u".into()]);
        assert!(evaluate("f", &list, false, &with).0);
        assert!(!evaluate("f", &list, true, &without).0);
    }

    #[test]
    fn check_rule_bounds() {
        assert!(check_rule(&FlagRule::Percent(100)).is_ok());
        assert!(matches!(
            check_rule(&FlagRule::Percent(101)),
            Err(ForgeError::Invalid(_))
        ));
        let big = FlagRule::AllowList(vec!["x".to_string(); MAX_ALLOWLIST_ENTRIES + 1]);
        assert!(matches!(check_rule(&big), Err(ForgeError::Limit(_))));
    }
}

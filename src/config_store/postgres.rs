use super::{
    CACHE_TTL_SECS, ConfigStore, EvalCtx, FlagRule, MAX_ALLOWLIST_ENTRIES, MAX_KEY_BYTES,
    MAX_VALUE_BYTES,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::util::key_hash;
use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::types::Json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
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
fn cache_get<T: Clone>(cache: &Mutex<HashMap<String, Cached<T>>>, key: &str) -> Option<T> {
    let cache = cache.lock().ok()?;
    let entry = cache.get(key)?;
    (entry.fetched.elapsed() < CACHE_TTL).then(|| entry.value.clone())
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
    values: Mutex<HashMap<String, Cached<Option<String>>>>,
    flags: Mutex<HashMap<String, Cached<Option<FlagRule>>>>,
}

impl PgConfig {
    pub(crate) fn new(pool: PgPool, namespace: String) -> Self {
        Self {
            pool,
            namespace,
            values: Mutex::new(HashMap::new()),
            flags: Mutex::new(HashMap::new()),
        }
    }

    /// Stored key for a caller key, applying the namespace prefix.
    fn physical(&self, key: &str) -> String {
        crate::util::namespaced(&self.namespace, key)
    }

    /// Fetch the stored raw value, using the cache. Errors only on a real backend fault.
    async fn fetch_value(&self, key: &str) -> Result<Option<String>> {
        if let Some(hit) = cache_get(&self.values, key) {
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
        if let Some(hit) = cache_get(&self.flags, key) {
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
        // `FlagRule` is `#[non_exhaustive]`; an unknown rule resolves to
        // the caller's default (matching the "never errors" flag contract).
        #[allow(unreachable_patterns)]
        _ => (default, "default_unknown_rule"),
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
            if let Ok(v) = std::env::var(format!("FORGE_CFG_{key}")) {
                tracing::Span::current().record("config.source", "env");
                return Ok(Some(v));
            }
            // Fall back to the uppercased name (the universal env convention), so an
            // operator exporting FORGE_CFG_MAX_UPLOAD_MB finds a key "max_upload_mb".
            // The verbatim name above takes precedence. Only uppercase (an allocation)
            // when the key has a lowercase letter to fold; an already-uppercase key
            // would just re-probe the same var.
            if key.bytes().any(|b| b.is_ascii_lowercase())
                && let Ok(v) = std::env::var(format!("FORGE_CFG_{}", key.to_ascii_uppercase()))
            {
                tracing::Span::current().record("config.source", "env");
                return Ok(Some(v));
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
            metrics::counter!("forge_ops_total", "primitive" => "config", "op" => "flag", "outcome" => "ok")
                .increment(1);
            result
        }
        .instrument(span)
        .await
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

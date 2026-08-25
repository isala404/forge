use super::{
    ConfigStore, EvalCtx, FlagEvaluation, FlagRule, MAX_ALLOWLIST_ENTRIES, MAX_KEY_BYTES,
    MAX_VALUE_BYTES, capture_environment_overrides, environment_override,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Largest allowed `AllowList` entry, in bytes. Over => `Limit`. Mirrors the Postgres backend.
const MAX_ALLOWLIST_ENTRY_BYTES: usize = 256;

pub(crate) struct MemConfig {
    /// Prefix joined to every stored key as `<namespace>:<key>`. Empty = no prefix.
    /// The `FORGE_CFG_<KEY>` env override uses the caller (logical) key, matching Postgres.
    namespace: String,
    environment: HashMap<String, String>,
    values: Mutex<HashMap<String, String>>,
    flags: Mutex<HashMap<String, FlagRule>>,
}

impl MemConfig {
    pub(crate) fn new(namespace: String) -> Self {
        Self {
            namespace,
            environment: capture_environment_overrides(),
            values: Mutex::new(HashMap::new()),
            flags: Mutex::new(HashMap::new()),
        }
    }

    /// Stored key for a caller key, applying the namespace prefix.
    fn physical(&self, key: &str) -> String {
        crate::util::namespaced(&self.namespace, key)
    }
}

/// Take a map lock, recovering the guard if a previous holder panicked. The critical
/// sections are short and synchronous (no `await` held across the lock), so a poisoned
/// lock never reflects a half-updated invariant worth aborting for.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
/// `DefaultHasher`, whose seed is per-process). Matches the Postgres backend.
fn stable_bucket(flag_key: &str, targeting_key: &str) -> u32 {
    let hex = crate::util::sha256_hex(format!("{flag_key}:{targeting_key}").as_bytes());
    let prefix = hex.get(..8).unwrap_or("0");
    u32::from_str_radix(prefix, 16).unwrap_or(0) % 100
}

/// Evaluate a resolved rule against the context. Mirrors [`super::PgConfig`]'s resolution
/// table: a `Percent` rule with no targeting key falls back to the caller's `default`; an
/// `AllowList` matches only an explicit targeting key.
fn evaluate(key: &str, rule: &FlagRule, default: bool, ctx: &EvalCtx) -> bool {
    match rule {
        FlagRule::On => true,
        FlagRule::Off => false,
        FlagRule::Percent(p) => match &ctx.targeting_key {
            Some(k) => stable_bucket(key, k) < u32::from(*p),
            None => default,
        },
        FlagRule::AllowList(list) => match &ctx.targeting_key {
            Some(k) => list.iter().any(|e| e == k),
            None => false,
        },
        FlagRule::Value { value, .. } => value.as_bool().unwrap_or(default),
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
        FlagRule::AllowList(entries) => {
            let enabled = ctx
                .targeting_key
                .as_ref()
                .is_some_and(|targeting_key| entries.iter().any(|entry| entry == targeting_key));
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
    }
}

#[async_trait]
impl ConfigStore for MemConfig {
    async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        check_key(key)?;
        // env `FORGE_CFG_<KEY>` wins over the store, even when set to empty (12-factor).
        if let Some(value) = environment_override(&self.environment, key) {
            return Ok(Some(value.to_string()));
        }
        Ok(lock(&self.values).get(&self.physical(key)).cloned())
    }

    async fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        check_key(key)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(ForgeError::limit(format!(
                "config value is {} bytes; max is {MAX_VALUE_BYTES}",
                value.len()
            )));
        }
        // Last-write-wins upsert; visible to every reader immediately.
        lock(&self.values).insert(self.physical(key), value.to_string());
        Ok(())
    }

    async fn flag(&self, key: &str, default: bool, ctx: &EvalCtx) -> bool {
        // Never errors, never panics: a missing rule (or anything else) resolves to `default`.
        match lock(&self.flags).get(&self.physical(key)) {
            Some(rule) => evaluate(key, rule, default, ctx),
            None => default,
        }
    }

    async fn flag_details(
        &self,
        key: &str,
        default: &serde_json::Value,
        ctx: &EvalCtx,
    ) -> FlagEvaluation {
        match lock(&self.flags).get(&self.physical(key)) {
            Some(rule) => evaluate_details(key, rule, default, ctx),
            None => FlagEvaluation::new(default, None, "default_missing", None),
        }
    }

    async fn set_flag(&self, key: &str, rule: FlagRule) -> Result<()> {
        check_key(key)?;
        check_rule(&rule)?;
        lock(&self.flags).insert(self.physical(key), rule);
        Ok(())
    }

    async fn delete_raw(&self, key: &str) -> Result<bool> {
        check_key(key)?;
        Ok(lock(&self.values).remove(&self.physical(key)).is_some())
    }

    async fn delete_flag(&self, key: &str) -> Result<bool> {
        check_key(key)?;
        Ok(lock(&self.flags).remove(&self.physical(key)).is_some())
    }
}

#[async_trait]
impl BackendLifecycle for MemConfig {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Config
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process"
    }
    // No expiry or leases to reclaim; the maps only grow on explicit writes. Default no-op.
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_then_get_roundtrips_and_missing_is_none() {
        let cfg = MemConfig::new(String::new());
        assert_eq!(cfg.get_raw("retries").await.unwrap(), None);
        cfg.set_raw("retries", "3").await.unwrap();
        assert_eq!(cfg.get_raw("retries").await.unwrap(), Some("3".to_string()));
        // Last-write-wins.
        cfg.set_raw("retries", "5").await.unwrap();
        assert_eq!(cfg.get_raw("retries").await.unwrap(), Some("5".to_string()));
    }

    #[tokio::test]
    async fn typed_get_parses_json_and_reports_bad_values() {
        use super::super::ConfigExt;
        let cfg = MemConfig::new(String::new());
        cfg.set_raw("limit", "42").await.unwrap();
        assert_eq!(cfg.get::<u32>("limit").await.unwrap(), Some(42));
        assert_eq!(cfg.get::<u32>("absent").await.unwrap(), None);
        cfg.set_raw("limit", "not-json").await.unwrap();
        assert!(matches!(
            cfg.get::<u32>("limit").await,
            Err(ForgeError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn delete_raw_reports_presence_then_absence() {
        let cfg = MemConfig::new(String::new());
        assert!(!cfg.delete_raw("k").await.unwrap(), "absent => false");
        cfg.set_raw("k", "v").await.unwrap();
        assert!(cfg.delete_raw("k").await.unwrap(), "present => true");
        assert_eq!(cfg.get_raw("k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn empty_key_is_invalid_and_oversized_value_is_limit() {
        let cfg = MemConfig::new(String::new());
        assert!(matches!(
            cfg.set_raw("", "v").await,
            Err(ForgeError::Invalid(_))
        ));
        let big = "x".repeat(MAX_VALUE_BYTES + 1);
        assert!(matches!(
            cfg.set_raw("k", &big).await,
            Err(ForgeError::Limit(_))
        ));
    }

    #[tokio::test]
    async fn flag_resolution_covers_the_rule_table() {
        let cfg = MemConfig::new(String::new());
        let with = EvalCtx::user("u");
        let without = EvalCtx::new();

        // Missing rule => the caller's default.
        assert!(cfg.flag("missing", true, &with).await);
        assert!(!cfg.flag("missing", false, &with).await);

        cfg.set_flag("on", FlagRule::On).await.unwrap();
        cfg.set_flag("off", FlagRule::Off).await.unwrap();
        assert!(cfg.flag("on", false, &with).await);
        assert!(!cfg.flag("off", true, &with).await);

        cfg.set_flag("p_all", FlagRule::Percent(100)).await.unwrap();
        cfg.set_flag("p_none", FlagRule::Percent(0)).await.unwrap();
        assert!(cfg.flag("p_all", false, &with).await);
        assert!(!cfg.flag("p_none", true, &with).await);
        // Percent with no targeting key falls back to the caller default.
        cfg.set_flag("p_mid", FlagRule::Percent(50)).await.unwrap();
        assert!(cfg.flag("p_mid", true, &without).await);
        assert!(!cfg.flag("p_mid", false, &without).await);

        cfg.set_flag("allow", FlagRule::AllowList(vec!["u".into()]))
            .await
            .unwrap();
        assert!(cfg.flag("allow", false, &with).await, "listed subject");
        assert!(
            !cfg.flag("allow", true, &EvalCtx::user("other")).await,
            "unlisted subject"
        );
        assert!(
            !cfg.flag("allow", true, &without).await,
            "no targeting key => miss, not default"
        );
    }

    #[tokio::test]
    async fn typed_flags_return_value_variant_and_reason() {
        let cfg = MemConfig::new(String::new());
        cfg.set_flag(
            "theme",
            FlagRule::Value {
                value: serde_json::json!({"color": "blue"}),
                variant: "blue-v2".into(),
            },
        )
        .await
        .unwrap();
        let result = cfg
            .flag_details("theme", &serde_json::json!({}), &EvalCtx::new())
            .await;
        assert_eq!(result.value_json, r#"{"color":"blue"}"#);
        assert_eq!(result.value_type, "object");
        assert_eq!(result.variant.as_deref(), Some("blue-v2"));
        assert_eq!(result.reason, "static");
        assert_eq!(result.error_code, None);
    }

    #[tokio::test]
    async fn delete_flag_reverts_to_default() {
        let cfg = MemConfig::new(String::new());
        cfg.set_flag("f", FlagRule::On).await.unwrap();
        assert!(cfg.flag("f", false, &EvalCtx::new()).await);
        assert!(cfg.delete_flag("f").await.unwrap(), "present => true");
        assert!(!cfg.delete_flag("f").await.unwrap(), "absent => false");
        assert!(
            !cfg.flag("f", false, &EvalCtx::new()).await,
            "deleted rule => caller default"
        );
    }

    #[tokio::test]
    async fn set_flag_validates_rule_bounds() {
        let cfg = MemConfig::new(String::new());
        assert!(matches!(
            cfg.set_flag("p", FlagRule::Percent(101)).await,
            Err(ForgeError::Invalid(_))
        ));
        let big = FlagRule::AllowList(vec!["x".to_string(); MAX_ALLOWLIST_ENTRIES + 1]);
        assert!(matches!(
            cfg.set_flag("a", big).await,
            Err(ForgeError::Limit(_))
        ));
    }

    #[test]
    fn percent_bucket_is_stable_and_namespaced() {
        let a = stable_bucket("flag_a", "user-1");
        assert_eq!(
            a,
            stable_bucket("flag_a", "user-1"),
            "same inputs => same bucket"
        );
        assert!(a < 100);
        assert_ne!(
            stable_bucket("flag_a", "user-1"),
            stable_bucket("flag_b", "user-1"),
            "distinct flag keys bucket independently"
        );
    }

    #[tokio::test]
    async fn namespaces_isolate_values_and_flags() {
        let a = MemConfig::new("app_a".to_string());
        let b = MemConfig::new("app_b".to_string());
        a.set_raw("shared", "from-a").await.unwrap();
        b.set_raw("shared", "from-b").await.unwrap();
        assert_eq!(
            a.get_raw("shared").await.unwrap(),
            Some("from-a".to_string())
        );
        assert_eq!(
            b.get_raw("shared").await.unwrap(),
            Some("from-b".to_string())
        );

        a.set_flag("gate", FlagRule::On).await.unwrap();
        // The physical key carries the namespace, so b never sees a's flag.
        assert!(!b.flag("gate", false, &EvalCtx::new()).await);
        assert!(a.flag("gate", false, &EvalCtx::new()).await);

        assert_eq!(a.physical("k"), "app_a:k");
        assert_eq!(MemConfig::new(String::new()).physical("k"), "k");
    }

    #[test]
    fn backend_metadata_reports_in_process_non_durable() {
        let cfg = MemConfig::new(String::new());
        assert_eq!(cfg.name(), "memory");
        assert_eq!(cfg.primitive(), Primitive::Config);
        assert!(!cfg.durable());
        assert_eq!(cfg.caveats(), "in-process");
    }
}

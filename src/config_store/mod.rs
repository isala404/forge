use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn capture_environment_overrides() -> std::collections::HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            let value = value.into_string().ok()?;
            key.starts_with("FORGE_CFG_").then_some((key, value))
        })
        .collect()
}

fn environment_override<'a>(
    overrides: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> Option<&'a str> {
    overrides
        .get(&format!("FORGE_CFG_{key}"))
        .or_else(|| {
            key.bytes()
                .any(|byte| byte.is_ascii_lowercase())
                .then(|| overrides.get(&format!("FORGE_CFG_{}", key.to_ascii_uppercase())))?
        })
        .map(String::as_str)
}

/// Largest allowed config key in encoded UTF-8 bytes. Over => [`ForgeError::Invalid`].
pub const MAX_KEY_BYTES: usize = 256;

/// Largest allowed config value in bytes (64 KiB): a key/value store, not a document
/// store. Over => [`ForgeError::Limit`].
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// Largest allowed `AllowList` rule, in entries. Over => [`ForgeError::Limit`].
pub const MAX_ALLOWLIST_ENTRIES: usize = 10_000;

/// In-process cache staleness bound. A committed write is visible at every reader
/// within this window (part of the contract).
pub const CACHE_TTL_SECS: u64 = 30;
pub const MAX_BULK_KEYS: usize = 256;
pub const MAX_SNAPSHOT_STALE_SECS: u64 = 24 * 60 * 60;
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// OpenFeature `EvaluationContext`. `targeting_key` is the user/org id used for stable
/// percentage bucketing and allow-list matching.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EvalCtx {
    /// Stable subject id (user/org). Drives `Percent`/`AllowList`. `None` => those rules
    /// fall back per the contract.
    pub targeting_key: Option<String>,
    /// Invocation-local OpenFeature context fields. Forge's built-in rules currently use
    /// only `targeting_key`; adapters preserve the remaining fields for hooks and telemetry.
    #[serde(default)]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

impl EvalCtx {
    /// An empty context (no targeting key).
    pub fn new() -> Self {
        Self::default()
    }

    /// A context targeting `key` (a user/org id).
    pub fn user(key: impl Into<String>) -> Self {
        Self {
            targeting_key: Some(key.into()),
            attributes: BTreeMap::new(),
        }
    }

    /// Return a new invocation context with one additional field. This never mutates a
    /// process-global Forge context.
    pub fn with_field(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.attributes.insert(key.into(), value);
        self
    }
}

/// A boolean targeting rule or typed static JSON value.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlagRule {
    /// Always `true`.
    On,
    /// Always `false`.
    Off,
    /// `true` iff the stable bucket of `(key, targeting_key)` is `< p` (`p` in `0..=100`).
    Percent(u8),
    /// `true` iff `targeting_key` is in the list.
    AllowList(Vec<String>),
    /// A typed static value with an application-defined stable variant.
    Value {
        value: serde_json::Value,
        variant: String,
    },
}

/// OpenFeature-style evaluation details for boolean or typed JSON values.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FlagEvaluation {
    /// Canonical JSON so every language preserves the same scalar or structured value.
    pub value_json: String,
    /// One of boolean, string, integer, float, object, or array.
    pub value_type: String,
    /// Stable application-defined variant when the stored rule supplies one.
    pub variant: Option<String>,
    /// Stable evaluation reason such as static, targeting_match, default_missing, or default_error.
    pub reason: String,
    /// Error category when the default was used because evaluation failed.
    pub error_code: Option<String>,
}

/// One typed bulk-evaluation request. `id` disambiguates repeated flag keys evaluated with
/// different defaults or subjects and is returned unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagEvaluationRequest {
    pub id: String,
    pub key: String,
    pub default: serde_json::Value,
    pub context: EvalCtx,
}

/// One config value returned in the same order as the requested keys.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    pub value: Option<String>,
}

/// One bulk flag result returned in request order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FlagEvaluationEntry {
    pub id: String,
    pub key: String,
    pub evaluation: FlagEvaluation,
}

/// Explicit application declaration for config values copied into a portable snapshot.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotSecretHandling {
    /// The application asserts that every requested config value is non-secret.
    NoSecrets,
    /// The application will encrypt and authenticate the encoded snapshot before it leaves
    /// the trusted server boundary.
    ApplicationProtected,
}

impl SnapshotSecretHandling {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoSecrets => "no_secrets",
            Self::ApplicationProtected => "application_protected",
        }
    }
}

/// A read-only, caller-scoped view for explicitly bounded disconnected operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigSnapshot {
    pub schema_version: u32,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub secret_handling: String,
    pub config: Vec<ConfigEntry>,
    pub flags: Vec<FlagEvaluationEntry>,
}

impl ConfigSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate_shape()?;
        let encoded = serde_json::to_vec(self)
            .map_err(|_| ForgeError::invalid("config snapshot cannot be encoded"))?;
        if encoded.len() > MAX_SNAPSHOT_BYTES {
            return Err(ForgeError::limit("config snapshot exceeds 1 MiB"));
        }
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        if encoded.len() > MAX_SNAPSHOT_BYTES {
            return Err(ForgeError::limit("config snapshot exceeds 1 MiB"));
        }
        let snapshot: Self = serde_json::from_slice(encoded)
            .map_err(|_| ForgeError::invalid("config snapshot must be valid JSON"))?;
        snapshot.validate_shape()?;
        Ok(snapshot)
    }

    pub fn ensure_fresh(&self, now_ms: u64) -> Result<()> {
        if now_ms > self.expires_at_ms {
            return Err(ForgeError::precondition("config snapshot is stale"));
        }
        Ok(())
    }

    pub fn get_raw(&self, key: &str, now_ms: u64) -> Result<Option<String>> {
        self.ensure_fresh(now_ms)?;
        self.config
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.clone())
            .ok_or_else(|| ForgeError::invalid("config key was not included in the snapshot"))
    }

    pub fn flag_details(&self, id: &str, now_ms: u64) -> Result<FlagEvaluation> {
        self.ensure_fresh(now_ms)?;
        self.flags
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.evaluation.clone())
            .ok_or_else(|| ForgeError::invalid("flag request id was not included in the snapshot"))
    }

    fn validate_shape(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(ForgeError::invalid(
                "unsupported config snapshot schema version",
            ));
        }
        if self.expires_at_ms < self.created_at_ms
            || self.expires_at_ms - self.created_at_ms > MAX_SNAPSHOT_STALE_SECS * 1000
        {
            return Err(ForgeError::invalid("config snapshot staleness is invalid"));
        }
        if !matches!(
            self.secret_handling.as_str(),
            "no_secrets" | "application_protected"
        ) {
            return Err(ForgeError::invalid(
                "config snapshot secret handling is invalid",
            ));
        }
        if self.config.len() > MAX_BULK_KEYS || self.flags.len() > MAX_BULK_KEYS {
            return Err(ForgeError::limit("config snapshot has too many entries"));
        }
        if !all_unique(self.config.iter().map(|entry| entry.key.as_str())) {
            return Err(ForgeError::invalid(
                "config snapshot contains duplicate config keys",
            ));
        }
        if !all_unique(self.flags.iter().map(|entry| entry.id.as_str())) {
            return Err(ForgeError::invalid(
                "config snapshot contains duplicate flag request ids",
            ));
        }
        Ok(())
    }
}

fn all_unique<'a>(values: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = HashSet::new();
    values.into_iter().all(|value| seen.insert(value))
}

impl FlagEvaluation {
    pub(crate) fn new(
        value: &serde_json::Value,
        variant: Option<String>,
        reason: impl Into<String>,
        error_code: Option<String>,
    ) -> Self {
        let value_type = match value {
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
            serde_json::Value::Number(_) => "float",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
            serde_json::Value::Null => "null",
        };
        Self {
            value_json: value.to_string(),
            value_type: value_type.to_string(),
            variant,
            reason: reason.into(),
            error_code,
        }
    }
}

/// Typed, runtime configuration and boolean feature flags. Lineage: 12-factor +
/// OpenFeature. Object-safe; the facade hands out `Arc<dyn ConfigStore>`.
///
/// Exact resolution order, caching, and flag evaluation: <https://tryforge.dev/primitives/#config-and-flags>.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// Resolved string value: env `FORGE_CFG_<KEY>` over the stored value over `None`.
    /// Served from the in-process cache (≤30s stale). `None` if unset at every layer.
    async fn get_raw(&self, key: &str) -> Result<Option<String>>;

    /// Resolve up to 256 exact config keys in input order. Durable backends implement this
    /// with one backend round trip for cache misses.
    async fn get_many_raw(&self, keys: &[String]) -> Result<Vec<ConfigEntry>> {
        check_bulk_len(keys.len())?;
        let mut entries = Vec::with_capacity(keys.len());
        for key in keys {
            entries.push(ConfigEntry {
                key: key.clone(),
                value: self.get_raw(key).await?,
            });
        }
        Ok(entries)
    }

    /// Upsert the stored value (last-write-wins). Visible to every reader within the
    /// cache bound; an active `FORGE_CFG_<KEY>` env var still shadows it.
    async fn set_raw(&self, key: &str, value: &str) -> Result<()>;

    /// OpenFeature `getBooleanValue`. Never errors, never panics: any failure
    /// (missing flag, backend down, malformed rule) resolves to `default`, reason
    /// logged via obs.
    async fn flag(&self, key: &str, default: bool, ctx: &EvalCtx) -> bool;

    /// Evaluate a typed flag and return stable variant/reason/error details. The default
    /// is canonical JSON and is returned on missing or invalid rules and backend errors.
    async fn flag_details(
        &self,
        key: &str,
        default: &serde_json::Value,
        ctx: &EvalCtx,
    ) -> FlagEvaluation;

    /// Evaluate up to 256 typed requests in input order. Each failure resolves only that
    /// request to its default, preserving the ordinary OpenFeature contract.
    async fn flag_details_many(
        &self,
        requests: &[FlagEvaluationRequest],
    ) -> Result<Vec<FlagEvaluationEntry>> {
        check_bulk_len(requests.len())?;
        let mut entries = Vec::with_capacity(requests.len());
        for request in requests {
            entries.push(FlagEvaluationEntry {
                id: request.id.clone(),
                key: request.key.clone(),
                evaluation: self
                    .flag_details(&request.key, &request.default, &request.context)
                    .await,
            });
        }
        Ok(entries)
    }

    /// Capture exact requested values and pre-evaluated flags for a bounded disconnected
    /// interval. This never enumerates config and has no mutation methods.
    async fn snapshot(
        &self,
        config_keys: &[String],
        flag_requests: &[FlagEvaluationRequest],
        max_stale: Duration,
        secret_handling: SnapshotSecretHandling,
    ) -> Result<ConfigSnapshot> {
        check_bulk_len(config_keys.len())?;
        check_bulk_len(flag_requests.len())?;
        if !all_unique(config_keys.iter().map(String::as_str)) {
            return Err(ForgeError::invalid("snapshot config keys must be unique"));
        }
        if !all_unique(flag_requests.iter().map(|request| request.id.as_str())) {
            return Err(ForgeError::invalid(
                "snapshot flag request ids must be unique",
            ));
        }
        if max_stale.is_zero() || max_stale.as_secs() > MAX_SNAPSHOT_STALE_SECS {
            return Err(ForgeError::invalid(
                "snapshot max staleness must be in 1..=86400 seconds",
            ));
        }
        let created_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ForgeError::invalid("system time is before the Unix epoch"))?
            .as_millis()
            .try_into()
            .map_err(|_| ForgeError::limit("snapshot timestamp exceeds u64"))?;
        let snapshot = ConfigSnapshot {
            schema_version: 1,
            created_at_ms,
            expires_at_ms: created_at_ms + max_stale.as_millis() as u64,
            secret_handling: secret_handling.as_str().to_string(),
            config: self.get_many_raw(config_keys).await?,
            flags: self.flag_details_many(flag_requests).await?,
        };
        snapshot.validate_shape()?;
        snapshot.encode()?;
        Ok(snapshot)
    }

    /// Validate and encode a portable snapshot. This performs no backend I/O.
    fn encode_snapshot(&self, snapshot: &ConfigSnapshot) -> Result<Vec<u8>> {
        snapshot.encode()
    }

    /// Decode and validate a portable snapshot. This performs no backend I/O.
    fn decode_snapshot(&self, encoded: &[u8]) -> Result<ConfigSnapshot> {
        ConfigSnapshot::decode(encoded)
    }

    /// Upsert a flag's [`FlagRule`] (last-write-wins). Visible to `flag` within the
    /// cache bound.
    async fn set_flag(&self, key: &str, rule: FlagRule) -> Result<()>;

    /// Delete the stored value. `true` if a value was removed, `false` if absent.
    /// An active `FORGE_CFG_<KEY>` env var still shadows reads afterwards.
    async fn delete_raw(&self, key: &str) -> Result<bool>;

    /// Delete a flag's rule. `true` if a rule was removed. `flag` then reverts to
    /// returning the caller's `default` for that key.
    async fn delete_flag(&self, key: &str) -> Result<bool>;
}

pub(super) fn check_bulk_len(len: usize) -> Result<()> {
    if len > MAX_BULK_KEYS {
        return Err(ForgeError::limit("bulk config request exceeds 256 keys"));
    }
    Ok(())
}

/// Typed accessor over [`ConfigStore`]. Blanket-implemented, so it works on
/// `&dyn ConfigStore`. The resolved raw string is parsed as JSON into `T`.
#[async_trait]
pub trait ConfigExt: ConfigStore {
    /// Resolve the raw value and deserialize it from JSON into `T`. `None` if unset; a
    /// present value that fails to parse is [`ForgeError::Invalid`].
    async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get_raw(key).await? {
            Some(raw) => serde_json::from_str(&raw).map(Some).map_err(|e| {
                ForgeError::invalid(format!("could not deserialize config value: {e}"))
            }),
            None => Ok(None),
        }
    }
}

impl<T: ConfigStore + ?Sized> ConfigExt for T {}

mod memory;
mod postgres;
pub(crate) use memory::MemConfig;
pub(crate) use postgres::PgConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_environment_resolution_is_exact_then_uppercase() {
        let overrides = std::collections::HashMap::from([
            ("FORGE_CFG_name".to_string(), "exact".to_string()),
            ("FORGE_CFG_NAME".to_string(), "uppercase".to_string()),
        ]);
        assert_eq!(environment_override(&overrides, "name"), Some("exact"));
        assert_eq!(environment_override(&overrides, "NAME"), Some("uppercase"));
        assert_eq!(environment_override(&overrides, "missing"), None);
    }
}

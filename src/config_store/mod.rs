//! `config` (+ flags) — lineage: 12-factor env precedence + OpenFeature. See
//! `docs/contracts/config.md`.
//!
//! The trait is [`ConfigStore`] (module `config_store`) so it never collides with
//! `forge::ForgeConfig`; the facade accessor is `forge.config()`.
//!
//! The contract (the [`ConfigStore`] trait, [`ConfigExt`], [`EvalCtx`], [`FlagRule`])
//! lives in this module, which also wires the Postgres backend.

use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;

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

/// OpenFeature `EvaluationContext`. `targeting_key` is the user/org id used for stable
/// percentage bucketing and allow-list matching.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct EvalCtx {
    /// Stable subject id (user/org). Drives `Percent`/`AllowList`. `None` => those rules
    /// fall back per the contract.
    pub targeting_key: Option<String>,
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
        }
    }
}

/// A boolean flag rule. v1 is boolean-only (see the contract's Non-goals).
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
}

/// Typed, runtime configuration and boolean feature flags. Lineage: 12-factor +
/// OpenFeature. Object-safe; the facade hands out `Arc<dyn ConfigStore>`.
///
/// Exact resolution order, caching, and flag evaluation: `docs/contracts/config.md`.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// Resolved string value: env `FORGE_CFG_<KEY>` over the stored value over `None`.
    /// Served from the in-process cache (≤30s stale). `None` if unset at every layer.
    async fn get_raw(&self, key: &str) -> Result<Option<String>>;

    /// Upsert the stored value (last-write-wins). Visible to every reader within the
    /// cache bound; an active `FORGE_CFG_<KEY>` env var still shadows it.
    async fn set_raw(&self, key: &str, value: &str) -> Result<()>;

    /// OpenFeature `getBooleanValue`. **Never errors, never panics**: any failure
    /// (missing flag, backend down, malformed rule) resolves to `default`, reason
    /// logged via obs.
    async fn flag(&self, key: &str, default: bool, ctx: &EvalCtx) -> bool;

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

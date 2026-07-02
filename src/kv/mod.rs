use crate::error::Result;
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use std::time::Duration;

/// Largest allowed key in encoded UTF-8 bytes (fits a btree entry without TOAST). Over => [`crate::error::ForgeError::Limit`].
pub const MAX_KEY_BYTES: usize = 512;

/// Largest allowed value in bytes (1 MiB): a string/counter/session store, not a blob store. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// Condition under which a `set` writes. Mirrors Redis `SET` / `SET NX` / `SET XX`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SetMode {
    /// Unconditional write (Redis `SET`).
    #[default]
    Always,
    /// Write only if the key is absent or expired (Redis `SET NX`).
    IfNotExists,
    /// Write only if a live key exists (Redis `SET XX`).
    IfExists,
}

/// Options for [`Kv::set`].
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct SetOpts {
    /// Time-to-live. `None` persists with no expiry. Seconds precision; positive sub-second TTL rounds up to 1s.
    pub ttl: Option<Duration>,
    pub mode: SetMode,
}

impl SetOpts {
    /// Default options: unconditional write, no TTL.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    pub fn with_mode(mut self, mode: SetMode) -> Self {
        self.mode = mode;
        self
    }
}

/// A Redis-shaped key/value store: caching, sessions, counters, ephemeral state.
///
/// Object-safe (facade hands out `Arc<dyn Kv>`). Exact semantics, limits, and error mapping: <https://tryforge.dev/primitives/#key-value>.
#[async_trait]
pub trait Kv: Send + Sync {
    /// `GET`. `Some(value)` if present and unexpired, else `None`. An expired key returns `None`, guaranteed.
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;

    /// `MGET`. One slot per input key, in the same order: `Some(value)` for a live key,
    /// `None` for an absent/expired one. Duplicate keys repeat their value. Empty input
    /// returns an empty vec. One round-trip regardless of key count.
    async fn mget(&self, keys: &[&str]) -> Result<Vec<Option<Bytes>>>;

    /// `SET` / `SET NX` / `SET XX` per `opts.mode`. Returns whether the write happened.
    async fn set(&self, key: &str, value: Bytes, opts: SetOpts) -> Result<bool>;

    /// `DEL`. `true` if a key was removed, `false` if it was absent/expired.
    async fn delete(&self, key: &str) -> Result<bool>;

    /// `EXISTS`. `true` iff a live, unexpired key is present.
    async fn exists(&self, key: &str) -> Result<bool>;

    /// `INCRBY` (atomic). Missing/expired key starts from `0`; non-numeric existing value is
    /// [`crate::error::ForgeError::Invalid`], `i64` overflow is [`crate::error::ForgeError::Limit`]. TTL preserved.
    async fn incr(&self, key: &str, by: i64) -> Result<i64>;

    /// `EXPIRE`. Sets/replaces the TTL on a live key; `false` if absent/expired. Does not create keys.
    async fn expire(&self, key: &str, ttl: Duration) -> Result<bool>;

    /// Atomic single-key compare-and-swap (memcached `cas` / etcd txn shape; a documented deviation
    /// from Redis). Writes `new` iff current state equals `old` (`old = None` means expected absent/expired).
    async fn compare_and_swap(&self, key: &str, old: Option<Bytes>, new: Bytes) -> Result<bool>;

    /// `SCAN MATCH prefix*`. Up to `limit` keys with `prefix`, plus a next-page cursor (`None` when done).
    /// Weakly consistent: callers must tolerate duplicates across pages.
    async fn scan(
        &self,
        prefix: &str,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<String>, Option<Cursor>)>;
}

mod memory;
mod postgres;
pub(crate) use memory::MemKv;
pub(crate) use postgres::PgKv;

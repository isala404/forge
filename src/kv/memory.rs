use super::{Kv, MAX_KEY_BYTES, MAX_VALUE_BYTES, SetMode, SetOpts};
use crate::clock::Clock;
use crate::error::{ForgeError, Result};
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Upper bound on a relative TTL (~100 years), matching the Postgres backend. Over => `Limit`.
const MAX_TTL_SECS: f64 = 100.0 * 365.0 * 24.0 * 60.0 * 60.0;

/// One stored value plus its optional expiry deadline.
struct Entry {
    value: Bytes,
    /// Absolute deadline; `None` means no expiry. An entry is gone once `now >= expires_at`.
    expires_at: Option<Duration>,
}

impl Entry {
    fn is_expired(&self, now: Duration) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }
}

pub(crate) struct MemKv {
    state: Mutex<HashMap<String, Entry>>,
    /// Prefix joined to every key as `<namespace>:<key>`. Empty = no prefix.
    namespace: String,
    clock: Arc<dyn Clock>,
}

impl MemKv {
    #[cfg(test)]
    pub(crate) fn new(namespace: String) -> Self {
        Self::with_clock(namespace, Arc::new(crate::clock::SystemClock::new()))
    }

    pub(crate) fn with_clock(namespace: String, clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            namespace,
            clock,
        }
    }

    /// Take the map lock, recovering the guard if a previous holder panicked. Our
    /// critical sections are short and synchronous (no `await` held across the lock),
    /// so a poisoned lock never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn physical(&self, key: &str) -> String {
        crate::util::namespaced(&self.namespace, key)
    }

    fn logical<'a>(&self, stored: &'a str) -> &'a str {
        if self.namespace.is_empty() {
            stored
        } else {
            stored
                .strip_prefix(&self.namespace)
                .and_then(|s| s.strip_prefix(':'))
                .unwrap_or(stored)
        }
    }

    /// Validate the *physical* key (namespace prefix included) against the byte cap,
    /// identical to the Postgres backend so a key that fits one fits the other.
    fn check_key(namespace: &str, key: &str) -> Result<()> {
        let physical = if namespace.is_empty() {
            key.len()
        } else {
            namespace.len() + 1 + key.len()
        };
        if physical > MAX_KEY_BYTES {
            return Err(ForgeError::limit(format!(
                "key is {physical} bytes including the namespace prefix; max is {MAX_KEY_BYTES}"
            )));
        }
        Ok(())
    }

    fn check_value(value: &[u8]) -> Result<()> {
        if value.len() > MAX_VALUE_BYTES {
            return Err(ForgeError::limit(format!(
                "value is {} bytes; max is {MAX_VALUE_BYTES}",
                value.len()
            )));
        }
        Ok(())
    }

    /// Drop every expired entry. Reads already hide them; this reclaims the memory.
    pub(crate) fn purge_expired(&self) {
        let now = self.clock.elapsed();
        self.lock().retain(|_, e| !e.is_expired(now));
    }
}

/// Convert a TTL to a whole-second [`Duration`] (rounding a positive sub-second TTL up
/// to 1s), rejecting zero (`Invalid`) and over-max (`Limit`). Mirrors the Postgres
/// backend's `ttl_to_secs` so a given TTL expires at the same observable instant.
fn ttl_to_duration(ttl: Duration) -> Result<Duration> {
    if ttl.is_zero() {
        return Err(ForgeError::invalid("ttl must be positive"));
    }
    let secs = ttl.as_secs_f64().ceil().max(1.0);
    if secs > MAX_TTL_SECS {
        return Err(ForgeError::limit("ttl exceeds the backend maximum"));
    }
    Ok(Duration::from_secs(secs as u64))
}

#[async_trait]
impl Kv for MemKv {
    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        Self::check_key(&self.namespace, key)?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        match state.get(&pk) {
            Some(e) if e.is_expired(now) => {
                state.remove(&pk);
                Ok(None)
            }
            Some(e) => Ok(Some(e.value.clone())),
            None => Ok(None),
        }
    }

    async fn mget(&self, keys: &[&str]) -> Result<Vec<Option<Bytes>>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        for k in keys {
            Self::check_key(&self.namespace, k)?;
        }
        let now = self.clock.elapsed();
        let state = self.lock();
        let out = keys
            .iter()
            .map(|k| {
                let pk = self.physical(k);
                match state.get(&pk) {
                    Some(e) if !e.is_expired(now) => Some(e.value.clone()),
                    _ => None,
                }
            })
            .collect();
        Ok(out)
    }

    async fn set(&self, key: &str, value: Bytes, opts: SetOpts) -> Result<bool> {
        Self::check_key(&self.namespace, key)?;
        Self::check_value(&value)?;
        let ttl = opts.ttl.map(ttl_to_duration).transpose()?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        let live = state.get(&pk).is_some_and(|e| !e.is_expired(now));
        let wrote = match opts.mode {
            SetMode::Always => true,
            SetMode::IfNotExists => !live,
            SetMode::IfExists => live,
        };
        if wrote {
            state.insert(
                pk,
                Entry {
                    value,
                    expires_at: ttl.map(|d| now + d),
                },
            );
        }
        Ok(wrote)
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        Self::check_key(&self.namespace, key)?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        // An expired entry counts as absent: remove it but report `false`.
        let removed = match state.remove(&pk) {
            Some(e) => !e.is_expired(now),
            None => false,
        };
        Ok(removed)
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        Self::check_key(&self.namespace, key)?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        if state.get(&pk).is_some_and(|e| e.is_expired(now)) {
            state.remove(&pk);
            return Ok(false);
        }
        Ok(state.contains_key(&pk))
    }

    async fn incr(&self, key: &str, by: i64) -> Result<i64> {
        Self::check_key(&self.namespace, key)?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        // A missing or expired key starts from 0 with no TTL; a live key adds `by` and
        // keeps its TTL, matching the Postgres backend's ON CONFLICT branch.
        let (current, expires_at) = match state.get(&pk).filter(|e| !e.is_expired(now)) {
            Some(e) => {
                let n = std::str::from_utf8(&e.value)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .ok_or_else(|| ForgeError::invalid("value is not an integer"))?;
                (n, e.expires_at)
            }
            None => (0, None),
        };
        let next = current
            .checked_add(by)
            .ok_or_else(|| ForgeError::limit("counter overflow (exceeds i64)"))?;
        state.insert(
            pk,
            Entry {
                value: Bytes::from(next.to_string()),
                expires_at,
            },
        );
        Ok(next)
    }

    async fn expire(&self, key: &str, ttl: Duration) -> Result<bool> {
        Self::check_key(&self.namespace, key)?;
        let dur = ttl_to_duration(ttl)?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        match state.get_mut(&pk) {
            Some(e) if !e.is_expired(now) => {
                e.expires_at = Some(now + dur);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn compare_and_swap(&self, key: &str, old: Option<Bytes>, new: Bytes) -> Result<bool> {
        Self::check_key(&self.namespace, key)?;
        Self::check_value(&new)?;
        let pk = self.physical(key);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        let live = state
            .get(&pk)
            .filter(|e| !e.is_expired(now))
            .map(|e| e.value.clone());
        let matches = match (&old, &live) {
            (None, None) => true,
            (Some(expected), Some(current)) => expected == current,
            _ => false,
        };
        if matches {
            // A successful swap clears any TTL (contract).
            state.insert(
                pk,
                Entry {
                    value: new,
                    expires_at: None,
                },
            );
        }
        Ok(matches)
    }

    async fn scan(
        &self,
        prefix: &str,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<String>, Option<Cursor>)> {
        let physical_prefix = self.physical(prefix);
        let limit = limit.clamp(1, 10_000) as usize;
        // Keyset pagination over the physical key, like the Postgres backend: the
        // cursor token is the last physical key returned.
        let after = cursor.map(|c| c.token().to_string());
        let now = self.clock.elapsed();
        let state = self.lock();
        let mut keys: Vec<String> = state
            .iter()
            .filter(|(k, e)| !e.is_expired(now) && k.starts_with(physical_prefix.as_str()))
            .map(|(k, _)| k.clone())
            .filter(|k| after.as_deref().is_none_or(|a| k.as_str() > a))
            .collect();
        keys.sort();
        keys.truncate(limit);
        let next = if keys.len() < limit {
            None
        } else {
            keys.last().map(|k| Cursor::from_token(k.clone()))
        };
        let out: Vec<String> = keys.iter().map(|k| self.logical(k).to_string()).collect();
        Ok((out, next))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn b(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn set_then_get_roundtrips_and_missing_is_none() {
        let kv = MemKv::new(String::new());
        assert!(kv.set("greeting", b("hi"), SetOpts::new()).await.unwrap());
        assert_eq!(kv.get("greeting").await.unwrap(), Some(b("hi")));
        assert_eq!(kv.get("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_modes_match_redis_semantics() {
        let kv = MemKv::new(String::new());
        let nx = SetOpts::new().with_mode(SetMode::IfNotExists);
        let xx = SetOpts::new().with_mode(SetMode::IfExists);

        assert!(
            !kv.set("k", b("v"), xx.clone()).await.unwrap(),
            "XX absent => no write"
        );
        assert!(
            kv.set("k", b("first"), nx.clone()).await.unwrap(),
            "NX absent => write"
        );
        assert!(
            !kv.set("k", b("second"), nx).await.unwrap(),
            "NX live => blocked"
        );
        assert!(
            kv.set("k", b("third"), xx).await.unwrap(),
            "XX live => write"
        );
        assert_eq!(kv.get("k").await.unwrap(), Some(b("third")));
    }

    #[tokio::test]
    async fn ttl_expires_and_zero_is_invalid() {
        let kv = MemKv::new(String::new());
        assert!(matches!(
            kv.set("z", b("v"), SetOpts::new().with_ttl(Duration::ZERO))
                .await,
            Err(ForgeError::Invalid(_))
        ));

        kv.set("k", b("v"), SetOpts::new().with_ttl(Duration::from_secs(1)))
            .await
            .unwrap();
        assert!(kv.exists("k").await.unwrap());
        // The backend rounds TTL up to whole seconds, so wait past the 1s deadline.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(
            kv.get("k").await.unwrap(),
            None,
            "expired key reads as absent"
        );
        assert!(!kv.exists("k").await.unwrap());
    }

    #[tokio::test]
    async fn incr_counts_from_zero_and_rejects_non_numeric() {
        let kv = MemKv::new(String::new());
        assert_eq!(kv.incr("c", 1).await.unwrap(), 1, "missing key starts at 0");
        assert_eq!(kv.incr("c", 10).await.unwrap(), 11);
        assert_eq!(kv.incr("c", -5).await.unwrap(), 6);
        assert_eq!(
            kv.get("c").await.unwrap(),
            Some(b("6")),
            "counter is a string value"
        );

        kv.set("s", b("nope"), SetOpts::new()).await.unwrap();
        assert!(matches!(kv.incr("s", 1).await, Err(ForgeError::Invalid(_))));
    }

    #[tokio::test]
    async fn compare_and_swap_guards_writes() {
        let kv = MemKv::new(String::new());
        assert!(
            kv.compare_and_swap("k", None, b("v1")).await.unwrap(),
            "expected absent"
        );
        assert!(
            !kv.compare_and_swap("k", None, b("v2")).await.unwrap(),
            "now present => expected-absent fails"
        );
        assert!(
            kv.compare_and_swap("k", Some(b("v1")), b("v2"))
                .await
                .unwrap()
        );
        assert!(
            !kv.compare_and_swap("k", Some(b("v1")), b("v3"))
                .await
                .unwrap(),
            "stale expected value"
        );
        assert_eq!(kv.get("k").await.unwrap(), Some(b("v2")));
    }

    #[tokio::test]
    async fn scan_paginates_by_prefix() {
        let kv = MemKv::new(String::new());
        for i in 0..10 {
            kv.set(&format!("user{i:02}"), b("x"), SetOpts::new())
                .await
                .unwrap();
        }
        kv.set("other", b("y"), SetOpts::new()).await.unwrap();

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let (keys, next) = kv.scan("user", cursor, 3).await.unwrap();
            seen.extend(keys);
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        seen.sort();
        assert_eq!(seen.len(), 10, "exactly the 10 user* keys");
        assert_eq!(seen.first().map(String::as_str), Some("user00"));
        assert!(!seen.iter().any(|k| k == "other"));
    }

    #[tokio::test]
    async fn namespaces_isolate_keys() {
        let a = MemKv::new("app_a".to_string());
        let bb = MemKv::new("app_b".to_string());
        a.set("shared", b("from-a"), SetOpts::new()).await.unwrap();
        bb.set("shared", b("from-b"), SetOpts::new()).await.unwrap();
        assert_eq!(a.get("shared").await.unwrap(), Some(b("from-a")));
        assert_eq!(bb.get("shared").await.unwrap(), Some(b("from-b")));
        // Scan strips the namespace back to the logical key.
        let (keys, _) = a.scan("", None, 100).await.unwrap();
        assert_eq!(keys, vec!["shared".to_string()]);
    }

    #[tokio::test]
    async fn purge_expired_reclaims_only_dead_entries() {
        let kv = MemKv::new(String::new());
        kv.set("live", b("v"), SetOpts::new()).await.unwrap();
        kv.set(
            "dead",
            b("v"),
            SetOpts::new().with_ttl(Duration::from_secs(1)),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        kv.purge_expired();
        // Assert against the map directly: a `get` would lazily purge too, so reaching in
        // is what proves the bulk sweep (not the read path) reclaimed the dead entry.
        let state = kv.lock();
        assert!(state.contains_key("live"));
        assert!(!state.contains_key("dead"));
    }
}

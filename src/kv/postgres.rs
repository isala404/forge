use super::{Kv, MAX_KEY_BYTES, MAX_VALUE_BYTES, SetMode, SetOpts};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::types::Cursor;
use crate::util::key_hash;
use async_trait::async_trait;
use bytes::Bytes;
use sqlx::PgPool;
use std::time::Duration;
use tracing::field::Empty;

/// Upper bound on a relative TTL (~100 years). Over => `Limit`.
const MAX_TTL_SECS: f64 = 100.0 * 365.0 * 24.0 * 60.0 * 60.0;

pub(crate) struct PgKv {
    pool: PgPool,
    /// Prefix joined to every key as `<namespace>:<key>`. Empty = no prefix.
    namespace: String,
}

impl PgKv {
    pub(crate) fn new(pool: PgPool, namespace: String) -> Self {
        Self { pool, namespace }
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
    /// so a namespaced key can't exceed it. Keys may contain `:`; the prefix stays
    /// unambiguous because the namespace is colon-free.
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

    /// Delete expired rows to reclaim space (reads already hide them).
    pub(crate) async fn sweep(&self) -> Result<u64> {
        let r = sqlx::query!(
            "DELETE FROM forge_kv WHERE expires_at IS NOT NULL AND expires_at <= now()"
        )
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
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
}

/// Convert a TTL to whole seconds (rounding a positive sub-second TTL up to 1s),
/// rejecting zero (`Invalid`) and over-max (`Limit`).
fn ttl_to_secs(ttl: Duration) -> Result<f64> {
    if ttl.is_zero() {
        return Err(ForgeError::invalid("ttl must be positive"));
    }
    let secs = ttl.as_secs_f64().ceil().max(1.0);
    if secs > MAX_TTL_SECS {
        return Err(ForgeError::limit("ttl exceeds the backend maximum"));
    }
    Ok(secs)
}

/// Escape `%`, `_`, and `\` so a caller prefix is matched literally by `LIKE`.
fn like_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 2);
    for c in prefix.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[async_trait]
impl Kv for PgKv {
    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let span = tracing::info_span!(
            "forge.kv.get",
            kv.key_hash = %key_hash(key),
            kv.hit = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "get", span, async move {
            Self::check_key(&self.namespace, key)?;
            let pk = self.physical(key);
            let row = sqlx::query_scalar!(
                "SELECT value FROM forge_kv \
                 WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())",
                pk
            )
            .fetch_optional(&self.pool)
            .await?;
            let value = row.map(Bytes::from);
            tracing::Span::current().record("kv.hit", value.is_some());
            Ok(value)
        })
        .await
    }

    async fn mget(&self, keys: &[&str]) -> Result<Vec<Option<Bytes>>> {
        let span = tracing::info_span!(
            "forge.kv.mget",
            kv.keys = keys.len(),
            kv.hits = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "mget", span, async move {
            if keys.is_empty() {
                return Ok(Vec::new());
            }
            for k in keys {
                Self::check_key(&self.namespace, k)?;
            }
            let physical: Vec<String> = keys.iter().map(|k| self.physical(k)).collect();
            // One round-trip; the map collapses duplicate keys, and we re-expand to
            // every input position below to honor order and duplicates.
            let rows = sqlx::query!(
                "SELECT key, value FROM forge_kv \
                 WHERE key = ANY($1) AND (expires_at IS NULL OR expires_at > now())",
                &physical,
            )
            .fetch_all(&self.pool)
            .await?;
            // `Bytes::from(Vec<u8>)` takes the buffer with no copy; re-expanding to input
            // positions is then a cheap refcount bump, including for duplicate keys.
            let mut found: std::collections::HashMap<String, Bytes> =
                std::collections::HashMap::with_capacity(rows.len());
            for r in rows {
                found.insert(r.key, Bytes::from(r.value));
            }
            let out: Vec<Option<Bytes>> =
                physical.iter().map(|pk| found.get(pk).cloned()).collect();
            tracing::Span::current().record("kv.hits", out.iter().filter(|v| v.is_some()).count());
            Ok(out)
        })
        .await
    }

    async fn set(&self, key: &str, value: Bytes, opts: SetOpts) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.kv.set",
            kv.key_hash = %key_hash(key),
            kv.value_bytes = value.len(),
            kv.wrote = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "set", span, async move {
            Self::check_key(&self.namespace, key)?;
            Self::check_value(&value)?;
            let secs = opts.ttl.map(ttl_to_secs).transpose()?;
            let pk = self.physical(key);
            let bytes = value.as_ref();
            let wrote = match opts.mode {
                SetMode::Always => {
                    sqlx::query!(
                        "INSERT INTO forge_kv (key, value, expires_at) \
                         VALUES ($1, $2, CASE WHEN $3::double precision IS NULL THEN NULL \
                                 ELSE now() + make_interval(secs => $3) END) \
                         ON CONFLICT (key) DO UPDATE \
                         SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at",
                        pk,
                        bytes,
                        secs,
                    )
                    .execute(&self.pool)
                    .await?;
                    true
                }
                SetMode::IfNotExists => {
                    // On conflict, reclaim only an expired row; a live row fails the WHERE => blocked.
                    sqlx::query_scalar!(
                        "INSERT INTO forge_kv (key, value, expires_at) \
                         VALUES ($1, $2, CASE WHEN $3::double precision IS NULL THEN NULL \
                                 ELSE now() + make_interval(secs => $3) END) \
                         ON CONFLICT (key) DO UPDATE \
                         SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at \
                         WHERE forge_kv.expires_at IS NOT NULL AND forge_kv.expires_at <= now() \
                         RETURNING key",
                        pk,
                        bytes,
                        secs,
                    )
                    .fetch_optional(&self.pool)
                    .await?
                    .is_some()
                }
                SetMode::IfExists => sqlx::query_scalar!(
                    "UPDATE forge_kv \
                     SET value = $2, expires_at = CASE WHEN $3::double precision IS NULL THEN NULL \
                             ELSE now() + make_interval(secs => $3) END \
                     WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) \
                     RETURNING key",
                    pk,
                    bytes,
                    secs,
                )
                .fetch_optional(&self.pool)
                .await?
                .is_some(),
            };
            tracing::Span::current().record("kv.wrote", wrote);
            Ok(wrote)
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.kv.delete",
            kv.key_hash = %key_hash(key),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "delete", span, async move {
            Self::check_key(&self.namespace, key)?;
            let pk = self.physical(key);
            // An expired row counts as absent: excluded here, reclaimed by the sweep.
            let removed = sqlx::query_scalar!(
                "DELETE FROM forge_kv \
                 WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) \
                 RETURNING key",
                pk
            )
            .fetch_optional(&self.pool)
            .await?
            .is_some();
            Ok(removed)
        })
        .await
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.kv.exists",
            kv.key_hash = %key_hash(key),
            kv.hit = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "exists", span, async move {
            Self::check_key(&self.namespace, key)?;
            let pk = self.physical(key);
            let present = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                       SELECT 1 FROM forge_kv
                       WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())
                   ) AS "present!""#,
                pk
            )
            .fetch_one(&self.pool)
            .await?;
            tracing::Span::current().record("kv.hit", present);
            Ok(present)
        })
        .await
    }

    async fn incr(&self, key: &str, by: i64) -> Result<i64> {
        let span = tracing::info_span!(
            "forge.kv.incr",
            kv.key_hash = %key_hash(key),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "incr", span, async move {
            Self::check_key(&self.namespace, key)?;
            let pk = self.physical(key);
            // Atomic: fresh/expired key starts from 0, live key adds `by` and keeps its TTL.
            // Stored as decimal ASCII so the same column round-trips through `get`.
            let res = sqlx::query_scalar!(
                r#"INSERT INTO forge_kv (key, value, expires_at)
                   VALUES ($1, convert_to(($2::int8)::text, 'UTF8'), NULL)
                   ON CONFLICT (key) DO UPDATE SET
                     value = convert_to((
                       CASE WHEN forge_kv.expires_at IS NOT NULL AND forge_kv.expires_at <= now()
                            THEN $2::int8
                            ELSE convert_from(forge_kv.value, 'UTF8')::bigint + $2::int8
                       END)::text, 'UTF8'),
                     expires_at = CASE WHEN forge_kv.expires_at IS NOT NULL AND forge_kv.expires_at <= now()
                                       THEN NULL ELSE forge_kv.expires_at END
                   RETURNING convert_from(value, 'UTF8')::bigint AS "v!""#,
                pk,
                by,
            )
            .fetch_one(&self.pool)
            .await;
            match res {
                Ok(v) => Ok(v),
                Err(sqlx::Error::Database(db)) => match db.code().as_deref() {
                    // 22P02 (non-integer text) and 22021 (non-UTF-8, from convert_from)
                    // both mean a non-numeric incr target => Invalid per the contract.
                    Some("22P02") | Some("22021") => {
                        Err(ForgeError::invalid("value is not an integer"))
                    }
                    // 22003 numeric_value_out_of_range: i64 overflow.
                    Some("22003") => Err(ForgeError::limit("counter overflow (exceeds i64)")),
                    _ => Err(ForgeError::from_sqlx(sqlx::Error::Database(db))),
                },
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    async fn expire(&self, key: &str, ttl: Duration) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.kv.expire",
            kv.key_hash = %key_hash(key),
            kv.ttl_secs = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "expire", span, async move {
            Self::check_key(&self.namespace, key)?;
            let secs = ttl_to_secs(ttl)?;
            let pk = self.physical(key);
            tracing::Span::current().record("kv.ttl_secs", secs);
            let applied = sqlx::query_scalar!(
                "UPDATE forge_kv SET expires_at = now() + make_interval(secs => $2) \
                 WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) \
                 RETURNING key",
                pk,
                secs,
            )
            .fetch_optional(&self.pool)
            .await?
            .is_some();
            Ok(applied)
        })
        .await
    }

    async fn compare_and_swap(&self, key: &str, old: Option<Bytes>, new: Bytes) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.kv.cas",
            kv.key_hash = %key_hash(key),
            kv.wrote = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "cas", span, async move {
            Self::check_key(&self.namespace, key)?;
            Self::check_value(&new)?;
            let pk = self.physical(key);
            // A successful swap clears any TTL (contract).
            let new_bytes = new.as_ref();
            let wrote = match old {
                None => {
                    // Expected absent: insert, or reclaim an expired row.
                    sqlx::query_scalar!(
                        "INSERT INTO forge_kv (key, value, expires_at) VALUES ($1, $2, NULL) \
                         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, expires_at = NULL \
                         WHERE forge_kv.expires_at IS NOT NULL AND forge_kv.expires_at <= now() \
                         RETURNING key",
                        pk,
                        new_bytes,
                    )
                    .fetch_optional(&self.pool)
                    .await?
                    .is_some()
                }
                Some(expected) => {
                    let expected = expected.as_ref();
                    sqlx::query_scalar!(
                        "UPDATE forge_kv SET value = $2, expires_at = NULL \
                         WHERE key = $1 AND value = $3 \
                           AND (expires_at IS NULL OR expires_at > now()) \
                         RETURNING key",
                        pk,
                        new_bytes,
                        expected,
                    )
                    .fetch_optional(&self.pool)
                    .await?
                    .is_some()
                }
            };
            tracing::Span::current().record("kv.wrote", wrote);
            Ok(wrote)
        })
        .await
    }

    async fn scan(
        &self,
        prefix: &str,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<String>, Option<Cursor>)> {
        let physical_prefix = self.physical(prefix);
        let pattern = format!("{}%", like_escape(&physical_prefix));
        let limit_i = i64::from(limit.clamp(1, 10_000));
        let span = tracing::info_span!(
            "forge.kv.scan",
            kv.scan_returned = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("kv", "scan", span, async move {
            let rows = match cursor {
                None => {
                    sqlx::query_scalar!(
                        r#"SELECT key FROM forge_kv
                           WHERE key LIKE $1 ESCAPE '\'
                             AND (expires_at IS NULL OR expires_at > now())
                           ORDER BY key
                           LIMIT $2"#,
                        pattern,
                        limit_i,
                    )
                    .fetch_all(&self.pool)
                    .await?
                }
                Some(c) => {
                    let after = c.token().to_string();
                    sqlx::query_scalar!(
                        r#"SELECT key FROM forge_kv
                           WHERE key LIKE $1 ESCAPE '\'
                             AND key > $2
                             AND (expires_at IS NULL OR expires_at > now())
                           ORDER BY key
                           LIMIT $3"#,
                        pattern,
                        after,
                        limit_i,
                    )
                    .fetch_all(&self.pool)
                    .await?
                }
            };

            let next = if (rows.len() as i64) < limit_i {
                None
            } else {
                rows.last().map(|k| Cursor::from_token(k.clone()))
            };
            let keys: Vec<String> = rows.iter().map(|k| self.logical(k).to_string()).collect();
            tracing::Span::current().record("kv.scan_returned", keys.len());
            Ok((keys, next))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_escape_neutralizes_wildcards() {
        assert_eq!(like_escape("a%b_c"), "a\\%b\\_c");
        assert_eq!(like_escape("plain"), "plain");
    }

    #[test]
    fn check_key_allows_colon_rejects_oversize() {
        assert!(PgKv::check_key("", "user:42:session").is_ok());
        let big = "x".repeat(MAX_KEY_BYTES + 1);
        assert!(matches!(
            PgKv::check_key("", &big),
            Err(ForgeError::Limit(_))
        ));
        assert!(PgKv::check_key("", "ok").is_ok());
        // The namespace prefix counts against the budget.
        let near = "x".repeat(MAX_KEY_BYTES - 3);
        assert!(matches!(
            PgKv::check_key("app", &near),
            Err(ForgeError::Limit(_))
        ));
    }

    #[test]
    fn ttl_zero_is_invalid_and_rounds_up() {
        assert!(matches!(
            ttl_to_secs(Duration::ZERO),
            Err(ForgeError::Invalid(_))
        ));
        assert_eq!(
            ttl_to_secs(Duration::from_millis(10)).expect("positive"),
            1.0
        );
    }
}

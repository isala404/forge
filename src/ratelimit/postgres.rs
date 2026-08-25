use super::algo::{
    SlidingState, check_bucket, check_cost, check_key, check_limit, is_soft_error,
    resolve_fail_open, sliding_step, synthetic_allow, token_bucket_step,
};
use super::{
    Algo, Decision, FailMode, Limit, MAX_RESERVATION_TTL, RateLimit, Reservation, ReservationState,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::util::key_hash;
use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::time::{Duration, SystemTime};
use tracing::field::Empty;
use uuid::Uuid;

/// Idle rows untouched this long are purged by the sweep; an idle bucket refills to
/// full / a window ages out long before this, so dropping it changes nothing.
const IDLE_PURGE_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Postgres-backed [`RateLimit`].
pub(crate) struct PgRateLimit {
    pool: PgPool,
    namespace: String,
    fail_open: bool,
}

impl PgRateLimit {
    pub(crate) fn new(pool: PgPool, namespace: String, fail_open: bool) -> Self {
        Self {
            pool,
            namespace,
            fail_open,
        }
    }

    /// Namespaced bucket value. The namespace is colon-free, so `<ns>:<bucket>` never
    /// collides across distinct `(ns, bucket)`.
    fn ns_bucket(&self, bucket: &str) -> String {
        crate::util::namespaced(&self.namespace, bucket)
    }

    /// Delete rows untouched for longer than the idle window. Idempotent.
    pub(crate) async fn sweep(&self) -> Result<u64> {
        let prefix = (!self.namespace.is_empty()).then(|| format!("{}:", self.namespace));
        let mut tx = self.pool.begin().await?;
        expire_reservations(&mut tx).await?;
        let r = sqlx::query!(
            "DELETE FROM forge_ratelimit \
             WHERE updated_at <= now() - make_interval(secs => $1) \
             AND ($2::text IS NULL OR left(bucket, length($2)) = $2)",
            IDLE_PURGE_SECS,
            prefix,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(r.rows_affected())
    }

    async fn check_token_bucket(
        &self,
        bucket: &str,
        subject: &str,
        limit: Limit,
        cost: u32,
    ) -> Result<Decision> {
        let mut tx = self.pool.begin().await?;
        expire_reservations(&mut tx).await?;
        let (decision, _) = consume_tx(&mut tx, bucket, subject, limit, cost).await?;
        tx.commit().await?;
        Ok(decision)
    }

    async fn check_sliding(
        &self,
        bucket: &str,
        subject: &str,
        limit: Limit,
        cost: u32,
    ) -> Result<Decision> {
        let mut tx = self.pool.begin().await?;
        expire_reservations(&mut tx).await?;
        let (decision, _) = consume_tx(&mut tx, bucket, subject, limit, cost).await?;
        tx.commit().await?;
        Ok(decision)
    }
}

#[async_trait]
impl RateLimit for PgRateLimit {
    async fn check_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        fail: FailMode,
    ) -> Result<Decision> {
        self.check_cost_with(bucket, key, limit, 1, fail).await
    }

    async fn check_cost_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        cost: u32,
        fail: FailMode,
    ) -> Result<Decision> {
        let fail_open = resolve_fail_open(fail, self.fail_open);
        let span = tracing::info_span!(
            "forge.ratelimit.check",
            ratelimit.bucket = %bucket,
            ratelimit.key_hash = %key_hash(key),
            ratelimit.algo = algo_label(limit.algo),
            ratelimit.limit = limit.max,
            ratelimit.allowed = Empty,
            ratelimit.remaining = Empty,
            ratelimit.reset_after_secs = Empty,
            ratelimit.retry_after_secs = Empty,
            ratelimit.fail_open = Empty,
            ratelimit.outcome = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("ratelimit", "check", span, async move {
            check_bucket(bucket)?;
            check_key(key)?;
            check_limit(&limit)?;
            check_cost(&limit, cost)?;
            let ns_bucket = self.ns_bucket(bucket);
            let result = match limit.algo {
                Algo::TokenBucket => self.check_token_bucket(&ns_bucket, key, limit, cost).await,
                Algo::SlidingWindow => self.check_sliding(&ns_bucket, key, limit, cost).await,
            };
            let decision = match result {
                Ok(d) => d,
                Err(e) if fail_open && is_soft_error(&e) => {
                    tracing::warn!(error = %e, "ratelimit backend error; failing open (allowing)");
                    tracing::Span::current().record("ratelimit.fail_open", true);
                    synthetic_allow(limit)
                }
                Err(e) => return Err(e),
            };
            let s = tracing::Span::current();
            s.record("ratelimit.allowed", decision.allowed);
            s.record("ratelimit.remaining", decision.remaining);
            s.record("ratelimit.reset_after_secs", decision.reset_after.as_secs());
            if let Some(ra) = decision.retry_after {
                s.record("ratelimit.retry_after_secs", ra.as_secs());
            }
            s.record(
                "ratelimit.outcome",
                if decision.allowed {
                    "allowed"
                } else {
                    "denied"
                },
            );
            Ok(decision)
        })
        .await
    }

    #[allow(clippy::disallowed_methods)]
    async fn reserve(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        units: u32,
        ttl: Duration,
    ) -> Result<Option<Reservation>> {
        check_bucket(bucket)?;
        check_key(key)?;
        check_limit(&limit)?;
        check_cost(&limit, units)?;
        if ttl.is_zero() || ttl > MAX_RESERVATION_TTL {
            return Err(ForgeError::invalid("reservation ttl must be in (0, 3600s]"));
        }
        let bucket = self.ns_bucket(bucket);
        let mut tx = self.pool.begin().await?;
        expire_reservations(&mut tx).await?;
        let (decision, sliding_window_start) =
            consume_tx(&mut tx, &bucket, key, limit, units).await?;
        if !decision.allowed {
            tx.commit().await?;
            return Ok(None);
        }
        let id = Uuid::new_v4();
        let row = sqlx::query(
            "INSERT INTO forge_ratelimit_reservations \
             (id, bucket, subject, algorithm, capacity, period_secs, reserved_units, sliding_window_start, expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now()+make_interval(secs=>$9)) \
             RETURNING expires_at",
        ).bind(id).bind(&bucket).bind(key).bind(algo_label(limit.algo))
            .bind(i32::try_from(limit.max).unwrap_or(i32::MAX)).bind(limit.per.as_secs_f64())
            .bind(i32::try_from(units).unwrap_or(i32::MAX)).bind(sliding_window_start)
            .bind(ttl.as_secs_f64()).fetch_one(&mut *tx).await?;
        let expires_at = to_system_time(row.try_get("expires_at")?);
        tx.commit().await?;
        Ok(Some(Reservation {
            id,
            reserved_units: units,
            expires_at,
            state: ReservationState::Pending,
            committed_units: None,
        }))
    }

    async fn commit(&self, reservation_id: Uuid, actual_units: u32) -> Result<Reservation> {
        settle_reservation(&self.pool, reservation_id, Some(actual_units)).await
    }

    async fn release(&self, reservation_id: Uuid) -> Result<Reservation> {
        settle_reservation(&self.pool, reservation_id, None).await
    }
}

#[allow(clippy::disallowed_methods)]
async fn consume_tx(
    tx: &mut Transaction<'_, Postgres>,
    bucket: &str,
    subject: &str,
    limit: Limit,
    cost: u32,
) -> Result<(Decision, Option<f64>)> {
    sqlx::query(
        "INSERT INTO forge_ratelimit (bucket, subject) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(bucket)
    .bind(subject)
    .execute(&mut **tx)
    .await?;
    let row = sqlx::query(
        "SELECT tokens, window_start, cur_count, prev_count, \
                EXTRACT(EPOCH FROM (now() - updated_at))::float8 AS elapsed, \
                EXTRACT(EPOCH FROM now())::float8 AS now_epoch \
         FROM forge_ratelimit WHERE bucket = $1 AND subject = $2 FOR UPDATE",
    )
    .bind(bucket)
    .bind(subject)
    .fetch_one(&mut **tx)
    .await?;
    match limit.algo {
        Algo::TokenBucket => {
            let (tokens, decision) =
                token_bucket_step(row.try_get("tokens")?, row.try_get("elapsed")?, limit, cost);
            sqlx::query("UPDATE forge_ratelimit SET tokens=$3, updated_at=now() WHERE bucket=$1 AND subject=$2")
                .bind(bucket).bind(subject).bind(tokens).execute(&mut **tx).await?;
            Ok((decision, None))
        }
        Algo::SlidingWindow => {
            let stored = row
                .try_get::<Option<f64>, _>("window_start")?
                .map(|window_start| SlidingState {
                    window_start,
                    cur: row
                        .try_get::<Option<i32>, _>("cur_count")
                        .unwrap_or(None)
                        .map(i64::from)
                        .unwrap_or(0),
                    prev: row
                        .try_get::<Option<i32>, _>("prev_count")
                        .unwrap_or(None)
                        .map(i64::from)
                        .unwrap_or(0),
                });
            let (state, decision) = sliding_step(stored, row.try_get("now_epoch")?, limit, cost);
            sqlx::query("UPDATE forge_ratelimit SET window_start=$3,cur_count=$4,prev_count=$5,updated_at=now() WHERE bucket=$1 AND subject=$2")
                .bind(bucket).bind(subject).bind(state.window_start).bind(state.cur).bind(state.prev).execute(&mut **tx).await?;
            Ok((decision, Some(state.window_start)))
        }
    }
}

#[allow(clippy::disallowed_methods)]
async fn refund_tx(
    tx: &mut Transaction<'_, Postgres>,
    row: &sqlx::postgres::PgRow,
    units: u32,
) -> Result<()> {
    if units == 0 {
        return Ok(());
    }
    let bucket: String = row.try_get("bucket")?;
    let subject: String = row.try_get("subject")?;
    let capacity = u32::try_from(row.try_get::<i32, _>("capacity")?).unwrap_or(u32::MAX);
    let period = Duration::from_secs_f64(row.try_get::<f64, _>("period_secs")?);
    let algorithm: String = row.try_get("algorithm")?;
    let state = sqlx::query(
        "SELECT tokens, window_start, cur_count, prev_count, \
                EXTRACT(EPOCH FROM (now()-updated_at))::float8 AS elapsed, \
                EXTRACT(EPOCH FROM now())::float8 AS now_epoch \
         FROM forge_ratelimit WHERE bucket=$1 AND subject=$2 FOR UPDATE",
    )
    .bind(&bucket)
    .bind(&subject)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(state) = state else {
        return Ok(());
    };
    if algorithm == "token_bucket" {
        let max = f64::from(capacity);
        let refill = state.try_get::<f64, _>("elapsed")?.max(0.0) * max / period.as_secs_f64();
        let tokens =
            (state.try_get::<Option<f64>, _>("tokens")?.unwrap_or(max) + refill + f64::from(units))
                .min(max);
        sqlx::query(
            "UPDATE forge_ratelimit SET tokens=$3,updated_at=now() WHERE bucket=$1 AND subject=$2",
        )
        .bind(bucket)
        .bind(subject)
        .bind(tokens)
        .execute(&mut **tx)
        .await?;
    } else {
        let limit = Limit::per_duration(capacity, period).with_algo(Algo::SlidingWindow);
        let stored = state
            .try_get::<Option<f64>, _>("window_start")?
            .map(|window_start| SlidingState {
                window_start,
                cur: state
                    .try_get::<Option<i32>, _>("cur_count")
                    .unwrap_or(None)
                    .map(i64::from)
                    .unwrap_or(0),
                prev: state
                    .try_get::<Option<i32>, _>("prev_count")
                    .unwrap_or(None)
                    .map(i64::from)
                    .unwrap_or(0),
            });
        let (mut normalized, _) = sliding_step(stored, state.try_get("now_epoch")?, limit, 0);
        let reserved_start: Option<f64> = row.try_get("sliding_window_start")?;
        let current = (normalized.window_start / period.as_secs_f64()).floor() as i64;
        let reserved = reserved_start.map(|value| (value / period.as_secs_f64()).floor() as i64);
        match reserved {
            Some(index) if index == current => {
                normalized.cur = normalized.cur.saturating_sub(i64::from(units))
            }
            Some(index) if index == current - 1 => {
                normalized.prev = normalized.prev.saturating_sub(i64::from(units))
            }
            _ => {}
        }
        sqlx::query("UPDATE forge_ratelimit SET window_start=$3,cur_count=$4,prev_count=$5,updated_at=now() WHERE bucket=$1 AND subject=$2")
            .bind(bucket).bind(subject).bind(normalized.window_start).bind(normalized.cur).bind(normalized.prev).execute(&mut **tx).await?;
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)]
async fn expire_reservations(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    let rows = sqlx::query(
        "SELECT id,bucket,subject,algorithm,capacity,period_secs,reserved_units,committed_units, \
                sliding_window_start,state,expires_at \
         FROM forge_ratelimit_reservations WHERE state='pending' AND expires_at <= now() \
         ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1000",
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let units = u32::try_from(row.try_get::<i32, _>("reserved_units")?).unwrap_or(0);
        refund_tx(tx, &row, units).await?;
        sqlx::query("UPDATE forge_ratelimit_reservations SET state='expired' WHERE id=$1")
            .bind(row.try_get::<Uuid, _>("id")?)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)]
async fn settle_reservation(pool: &PgPool, id: Uuid, actual: Option<u32>) -> Result<Reservation> {
    let mut tx = pool.begin().await?;
    expire_reservations(&mut tx).await?;
    let row = sqlx::query(
        "SELECT id,bucket,subject,algorithm,capacity,period_secs,reserved_units,committed_units, \
                sliding_window_start,state,expires_at \
         FROM forge_ratelimit_reservations WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ForgeError::NotFound)?;
    let state: String = row.try_get("state")?;
    let reserved = u32::try_from(row.try_get::<i32, _>("reserved_units")?).unwrap_or(0);
    let prior_committed = row
        .try_get::<Option<i32>, _>("committed_units")?
        .and_then(|value| u32::try_from(value).ok());
    match (state.as_str(), actual) {
        ("pending", Some(value)) => {
            if value > reserved {
                return Err(ForgeError::limit("committed units exceed reservation"));
            }
            refund_tx(&mut tx, &row, reserved - value).await?;
            sqlx::query("UPDATE forge_ratelimit_reservations SET state='committed',committed_units=$2 WHERE id=$1")
                .bind(id).bind(i32::try_from(value).unwrap_or(i32::MAX)).execute(&mut *tx).await?;
        }
        ("pending", None) => {
            refund_tx(&mut tx, &row, reserved).await?;
            sqlx::query("UPDATE forge_ratelimit_reservations SET state='released' WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        ("committed", Some(value)) if prior_committed == Some(value) => {}
        ("released", None) => {}
        ("committed", Some(_)) => {
            return Err(ForgeError::precondition(
                "reservation was committed with a different unit count",
            ));
        }
        _ => return Err(ForgeError::precondition("reservation is no longer pending")),
    }
    let final_state = match (state.as_str(), actual) {
        ("pending", Some(_)) => ReservationState::Committed,
        ("pending", None) => ReservationState::Released,
        ("committed", _) => ReservationState::Committed,
        ("released", _) => ReservationState::Released,
        _ => ReservationState::Expired,
    };
    let committed_units = if final_state == ReservationState::Committed {
        actual.or(prior_committed)
    } else {
        None
    };
    let expires_at = to_system_time(row.try_get("expires_at")?);
    tx.commit().await?;
    Ok(Reservation {
        id,
        reserved_units: reserved,
        expires_at,
        state: final_state,
        committed_units,
    })
}

fn to_system_time(value: chrono::DateTime<chrono::Utc>) -> SystemTime {
    SystemTime::UNIX_EPOCH
        + Duration::new(
            value.timestamp().max(0) as u64,
            value.timestamp_subsec_nanos(),
        )
}

fn algo_label(algo: Algo) -> &'static str {
    match algo {
        Algo::TokenBucket => "token_bucket",
        Algo::SlidingWindow => "sliding_window",
    }
}

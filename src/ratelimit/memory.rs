use super::algo::{
    SlidingState, check_bucket, check_cost, check_key, check_limit, is_soft_error,
    resolve_fail_open, sliding_step, synthetic_allow, token_bucket_step,
};
use super::{
    Algo, Decision, FailMode, Limit, MAX_RESERVATION_TTL, RateLimit, Reservation, ReservationState,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::clock::Clock;
use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Entries untouched this long are dropped by `maintain`. An idle bucket has refilled
/// to full or its window has aged out by then, so dropping it is observably identical to
/// keeping it: a re-check starts from a fresh full bucket either way. Mirrors the
/// Postgres sweep window.
const IDLE_PURGE_SECS: u64 = 24 * 60 * 60;

/// One subject's mutable limiter state. The entry holds both algorithms' state (like the
/// Postgres row that carries every column), so switching algorithm on a key reads the
/// other algorithm's fresh state. Whichever `check` runs touches only its own fields.
struct Bucket {
    /// Token-bucket level. `None` until the first token-bucket check, where it reads as a
    /// fresh full bucket (mirrors the row's `NULL` tokens default).
    tokens: Option<f64>,
    /// Sliding-window state. `None` until the first sliding-window check.
    sliding: Option<SlidingState>,
    /// Last time this entry was touched. Drives the token-bucket refill and the
    /// idle-purge sweep.
    updated_at: Duration,
}

struct ReservationRow {
    id: Uuid,
    bucket: String,
    subject: String,
    limit: Limit,
    reserved: u32,
    expires_at: Duration,
    expires_at_wall: SystemTime,
    state: ReservationState,
    committed: Option<u32>,
    sliding_window_start: Option<f64>,
}

#[derive(Default)]
struct State {
    buckets: HashMap<(String, String), Bucket>,
    reservations: HashMap<Uuid, ReservationRow>,
}

impl Bucket {
    fn fresh(now: Duration) -> Self {
        Self {
            tokens: None,
            sliding: None,
            updated_at: now,
        }
    }
}

pub(crate) struct MemRateLimit {
    state: Mutex<State>,
    /// Prefix joined to every bucket as `<namespace>:<bucket>`. Empty = no prefix.
    namespace: String,
    /// Instance default for what happens on a *soft* backend error. In-process bucket math
    /// is infallible, so this never fires today; it exists for parity with
    /// [`super::PgRateLimit`] and stays honored if a fallible path is ever added.
    fail_open: bool,
    clock: Arc<dyn Clock>,
}

impl MemRateLimit {
    #[cfg(test)]
    pub(crate) fn new(namespace: String, fail_open: bool) -> Self {
        Self::with_clock(
            namespace,
            fail_open,
            Arc::new(crate::clock::SystemClock::new()),
        )
    }

    pub(crate) fn with_clock(namespace: String, fail_open: bool, clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Mutex::new(State::default()),
            namespace,
            fail_open,
            clock,
        }
    }

    /// Take the map lock, recovering the guard if a previous holder panicked. Critical
    /// sections are short and synchronous (no `await` across the lock), so a poisoned lock
    /// never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Namespaced bucket value. The namespace is colon-free, so `<ns>:<bucket>` never
    /// collides across distinct `(ns, bucket)`.
    fn ns_bucket(&self, bucket: &str) -> String {
        crate::util::namespaced(&self.namespace, bucket)
    }

    /// Drop entries untouched for longer than the idle window. Idempotent; a dropped
    /// entry is observably a fresh one, so a later check resurrects it harmlessly.
    pub(crate) fn purge_idle(&self) {
        let now = self.clock.elapsed();
        let cutoff = Duration::from_secs(IDLE_PURGE_SECS);
        let mut state = self.lock();
        self.expire_locked(&mut state, now);
        state
            .buckets
            .retain(|_, b| now.saturating_sub(b.updated_at) < cutoff);
    }

    fn consume_locked(
        state: &mut State,
        bucket: &str,
        subject: &str,
        limit: Limit,
        cost: u32,
        now: Duration,
    ) -> Decision {
        let entry = state
            .buckets
            .entry((bucket.to_string(), subject.to_string()))
            .or_insert_with(|| Bucket::fresh(now));
        let decision = match limit.algo {
            Algo::TokenBucket => {
                let elapsed = now.saturating_sub(entry.updated_at).as_secs_f64();
                let (tokens, decision) = token_bucket_step(entry.tokens, elapsed, limit, cost);
                entry.tokens = Some(tokens);
                decision
            }
            Algo::SlidingWindow => {
                let (sliding, decision) =
                    sliding_step(entry.sliding.clone(), now.as_secs_f64(), limit, cost);
                entry.sliding = Some(sliding);
                decision
            }
        };
        entry.updated_at = now;
        decision
    }

    fn refund_locked(state: &mut State, row: &ReservationRow, units: u32, now: Duration) {
        if units == 0 {
            return;
        }
        let Some(bucket) = state
            .buckets
            .get_mut(&(row.bucket.clone(), row.subject.clone()))
        else {
            return;
        };
        match row.limit.algo {
            Algo::TokenBucket => {
                let elapsed = now.saturating_sub(bucket.updated_at).as_secs_f64();
                let max = f64::from(row.limit.max);
                let refill = elapsed * max / row.limit.per.as_secs_f64();
                bucket.tokens =
                    Some((bucket.tokens.unwrap_or(max) + refill + f64::from(units)).min(max));
            }
            Algo::SlidingWindow => {
                let (normalized, _) =
                    sliding_step(bucket.sliding.clone(), now.as_secs_f64(), row.limit, 0);
                let per = row.limit.per.as_secs_f64();
                let current_index = (normalized.window_start / per).floor() as i64;
                let reserved_index = row
                    .sliding_window_start
                    .map(|start| (start / per).floor() as i64);
                let mut normalized = normalized;
                match reserved_index {
                    Some(index) if index == current_index => {
                        normalized.cur = normalized.cur.saturating_sub(i64::from(units))
                    }
                    Some(index) if index == current_index - 1 => {
                        normalized.prev = normalized.prev.saturating_sub(i64::from(units))
                    }
                    _ => {}
                }
                bucket.sliding = Some(normalized);
            }
        }
        bucket.updated_at = now;
    }

    fn expire_locked(&self, state: &mut State, now: Duration) {
        let ids = state
            .reservations
            .iter()
            .filter(|(_, row)| row.state == ReservationState::Pending && row.expires_at <= now)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in ids {
            if let Some(mut row) = state.reservations.remove(&id) {
                Self::refund_locked(state, &row, row.reserved, now);
                row.state = ReservationState::Expired;
                state.reservations.insert(id, row);
            }
        }
    }

    fn reservation(&self, row: &ReservationRow) -> Reservation {
        Reservation {
            id: row.id,
            reserved_units: row.reserved,
            expires_at: row.expires_at_wall,
            state: row.state,
            committed_units: row.committed,
        }
    }
}

#[async_trait]
impl RateLimit for MemRateLimit {
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
        // Caller bugs (`Invalid`/`Limit`) always surface, regardless of fail mode.
        check_bucket(bucket)?;
        check_key(key)?;
        check_limit(&limit)?;
        check_cost(&limit, cost)?;
        let ns_bucket = self.ns_bucket(bucket);
        let now = self.clock.elapsed();
        let mut state = self.lock();
        self.expire_locked(&mut state, now);
        let result: Result<Decision> = Ok(Self::consume_locked(
            &mut state, &ns_bucket, key, limit, cost, now,
        ));
        match result {
            Ok(d) => Ok(d),
            // The in-process math is infallible, so this branch never fires today; it
            // mirrors the Postgres backend so a future fallible path obeys the same
            // `ratelimit_fail_open` semantics.
            Err(e) if fail_open && is_soft_error(&e) => {
                tracing::warn!(error = %e, "ratelimit backend error; failing open (allowing)");
                Ok(synthetic_allow(limit))
            }
            Err(e) => Err(e),
        }
    }

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
            return Err(crate::ForgeError::invalid(
                "reservation ttl must be in (0, 3600s]",
            ));
        }
        let now = self.clock.elapsed();
        let ns_bucket = self.ns_bucket(bucket);
        let mut state = self.lock();
        self.expire_locked(&mut state, now);
        let decision = Self::consume_locked(&mut state, &ns_bucket, key, limit, units, now);
        if !decision.allowed {
            return Ok(None);
        }
        let id = Uuid::new_v4();
        let sliding_window_start = state
            .buckets
            .get(&(ns_bucket.clone(), key.to_string()))
            .and_then(|entry| entry.sliding.as_ref().map(|value| value.window_start));
        let row = ReservationRow {
            id,
            bucket: ns_bucket,
            subject: key.to_string(),
            limit,
            reserved: units,
            expires_at: now + ttl,
            expires_at_wall: self.clock.now() + ttl,
            state: ReservationState::Pending,
            committed: None,
            sliding_window_start,
        };
        let out = self.reservation(&row);
        state.reservations.insert(id, row);
        Ok(Some(out))
    }

    async fn commit(&self, reservation_id: Uuid, actual_units: u32) -> Result<Reservation> {
        let now = self.clock.elapsed();
        let mut state = self.lock();
        self.expire_locked(&mut state, now);
        let Some(mut row) = state.reservations.remove(&reservation_id) else {
            return Err(crate::ForgeError::NotFound);
        };
        match row.state {
            ReservationState::Pending => {
                if actual_units > row.reserved {
                    state.reservations.insert(reservation_id, row);
                    return Err(crate::ForgeError::limit(
                        "committed units exceed reservation",
                    ));
                }
                Self::refund_locked(&mut state, &row, row.reserved - actual_units, now);
                row.state = ReservationState::Committed;
                row.committed = Some(actual_units);
            }
            ReservationState::Committed if row.committed == Some(actual_units) => {}
            ReservationState::Committed => {
                state.reservations.insert(reservation_id, row);
                return Err(crate::ForgeError::precondition(
                    "reservation was committed with a different unit count",
                ));
            }
            ReservationState::Released | ReservationState::Expired => {
                state.reservations.insert(reservation_id, row);
                return Err(crate::ForgeError::precondition(
                    "reservation is no longer pending",
                ));
            }
        }
        let out = self.reservation(&row);
        state.reservations.insert(reservation_id, row);
        Ok(out)
    }

    async fn release(&self, reservation_id: Uuid) -> Result<Reservation> {
        let now = self.clock.elapsed();
        let mut state = self.lock();
        self.expire_locked(&mut state, now);
        let Some(mut row) = state.reservations.remove(&reservation_id) else {
            return Err(crate::ForgeError::NotFound);
        };
        match row.state {
            ReservationState::Pending => {
                Self::refund_locked(&mut state, &row, row.reserved, now);
                row.state = ReservationState::Released;
            }
            ReservationState::Released => {}
            ReservationState::Committed | ReservationState::Expired => {
                state.reservations.insert(reservation_id, row);
                return Err(crate::ForgeError::precondition(
                    "reservation is no longer pending",
                ));
            }
        }
        let out = self.reservation(&row);
        state.reservations.insert(reservation_id, row);
        Ok(out)
    }
}

#[async_trait]
impl BackendLifecycle for MemRateLimit {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::RateLimit
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "per-process buckets, not shared across processes"
    }
    async fn maintain(&self) -> Result<()> {
        self.purge_idle();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::super::{MAX_BUCKET_BYTES, MAX_KEY_BYTES};
    use super::*;
    use crate::error::ForgeError;

    fn tb(max: u32, per_secs: u64) -> Limit {
        Limit::per_duration(max, Duration::from_secs(per_secs))
    }

    fn sw(max: u32, per_secs: u64) -> Limit {
        tb(max, per_secs).with_algo(Algo::SlidingWindow)
    }

    #[tokio::test]
    async fn token_bucket_consumes_budget_then_denies() {
        let rl = MemRateLimit::new(String::new(), true);
        // A large window keeps refill over the test's microseconds far below one token,
        // so the first three checks drain the budget deterministically.
        let limit = tb(3, 3600);
        for expected_remaining in [2, 1, 0] {
            let d = rl.check("api", "user1", limit).await.unwrap();
            assert!(d.allowed);
            assert_eq!(d.remaining, expected_remaining);
            assert_eq!(d.limit, 3);
            assert!(d.retry_after.is_none());
        }
        let denied = rl.check("api", "user1", limit).await.unwrap();
        assert!(!denied.allowed, "fourth call exhausts the bucket");
        assert_eq!(denied.remaining, 0);
        assert!(denied.retry_after.is_some(), "a denial carries retry_after");
    }

    #[tokio::test]
    async fn sliding_window_caps_then_denies_within_window() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = sw(2, 100);
        let d1 = rl.check("login", "ip", limit).await.unwrap();
        assert!(d1.allowed && d1.remaining == 1);
        let d2 = rl.check("login", "ip", limit).await.unwrap();
        assert!(d2.allowed && d2.remaining == 0);
        let d3 = rl.check("login", "ip", limit).await.unwrap();
        assert!(!d3.allowed, "third call in the window is denied");
        assert!(d3.retry_after.is_some());
    }

    #[tokio::test]
    async fn distinct_subjects_have_independent_budgets() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 3600);
        assert!(rl.check("api", "alice", limit).await.unwrap().allowed);
        assert!(
            !rl.check("api", "alice", limit).await.unwrap().allowed,
            "alice is now exhausted"
        );
        assert!(
            rl.check("api", "bob", limit).await.unwrap().allowed,
            "bob has his own bucket"
        );
    }

    #[tokio::test]
    async fn distinct_buckets_have_independent_budgets() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 3600);
        assert!(rl.check("send", "u", limit).await.unwrap().allowed);
        assert!(!rl.check("send", "u", limit).await.unwrap().allowed);
        assert!(
            rl.check("read", "u", limit).await.unwrap().allowed,
            "a different bucket for the same subject is independent"
        );
    }

    #[tokio::test]
    async fn namespaces_isolate_buckets() {
        let a = MemRateLimit::new("tenant_a".to_string(), true);
        let b = MemRateLimit::new("tenant_b".to_string(), true);
        let limit = tb(1, 3600);
        assert!(a.check("api", "shared", limit).await.unwrap().allowed);
        assert!(
            !a.check("api", "shared", limit).await.unwrap().allowed,
            "tenant_a is exhausted"
        );
        assert!(
            b.check("api", "shared", limit).await.unwrap().allowed,
            "tenant_b's namespaced bucket is untouched"
        );
    }

    #[tokio::test]
    async fn invalid_inputs_surface_as_errors_regardless_of_fail_open() {
        // fail_open = true must NOT swallow caller bugs.
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 60);
        assert!(matches!(
            rl.check("", "u", limit).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            rl.check("api", "", limit).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            rl.check("api", "u", tb(0, 60)).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            rl.check("api", "u", Limit::per_duration(1, Duration::ZERO))
                .await,
            Err(ForgeError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn oversized_bucket_and_key_are_limit_errors() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 60);
        let big_bucket = "b".repeat(MAX_BUCKET_BYTES + 1);
        let big_key = "k".repeat(MAX_KEY_BYTES + 1);
        assert!(matches!(
            rl.check(&big_bucket, "u", limit).await,
            Err(ForgeError::Limit(_))
        ));
        assert!(matches!(
            rl.check("api", &big_key, limit).await,
            Err(ForgeError::Limit(_))
        ));
    }

    #[tokio::test]
    async fn purge_idle_keeps_recent_entries() {
        let rl = MemRateLimit::new(String::new(), true);
        rl.check("api", "u", tb(1, 60)).await.unwrap();
        // Nothing is idle yet, so a sweep must not drop the live bucket: the next check
        // sees the consumed budget rather than a fresh one.
        rl.purge_idle();
        assert!(
            !rl.check("api", "u", tb(1, 60)).await.unwrap().allowed,
            "the recent bucket survived the sweep and is still exhausted"
        );
    }

    #[tokio::test]
    async fn weighted_checks_and_reservations_never_double_refund() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(10, 3600);
        let weighted = rl.check_cost("tokens", "tenant", limit, 4).await.unwrap();
        assert!(weighted.allowed);
        assert_eq!(weighted.remaining, 6);
        assert!(matches!(
            rl.check_cost("tokens", "tenant", limit, 11).await,
            Err(ForgeError::Limit(_))
        ));

        let reservation = rl
            .reserve("tokens", "tenant", limit, 5, Duration::from_secs(30))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reservation.state, ReservationState::Pending);
        let committed = rl.commit(reservation.id, 2).await.unwrap();
        assert_eq!(committed.committed_units, Some(2));
        assert_eq!(rl.commit(reservation.id, 2).await.unwrap(), committed);
        assert!(matches!(
            rl.release(reservation.id).await,
            Err(ForgeError::Precondition(_))
        ));
        assert_eq!(
            rl.check_cost("tokens", "tenant", limit, 4)
                .await
                .unwrap()
                .remaining,
            0
        );
    }

    #[tokio::test]
    async fn released_reservation_restores_units_once() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(3, 3600);
        let reservation = rl
            .reserve("compute", "tenant", limit, 3, Duration::from_secs(30))
            .await
            .unwrap()
            .unwrap();
        let released = rl.release(reservation.id).await.unwrap();
        assert_eq!(released.state, ReservationState::Released);
        assert_eq!(rl.release(reservation.id).await.unwrap(), released);
        assert_eq!(
            rl.check_cost("compute", "tenant", limit, 3)
                .await
                .unwrap()
                .remaining,
            0
        );
    }
}

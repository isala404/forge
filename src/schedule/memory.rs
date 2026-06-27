//! In-process `schedule` backend. Contract: docs/contracts/schedule.md.
//!
//! A `Mutex<HashMap>` of registered schedules keyed by name. Each instance is scoped to
//! one app, so the app is not part of the key (separate instances are separate maps). The
//! target-queue name is stored with the same `<app>:<queue>` prefix the Postgres backend
//! uses and stripped back to the logical name on [`Schedule::list`].
//!
//! Timing and the observable contract match [`super::PgSchedule`]: `cron`/`at` register a
//! schedule, [`Schedule::process_due`] computes due ticks with the shared
//! [`Cron`](super::cron::Cron) evaluator (same missed-tick grace window), advances a
//! recurring schedule to its next tick, and drops a one-shot once it has fired.
//!
//! Delivery is real: the backend holds an [`Arc<dyn Queue>`](super::super::Queue) and
//! `process_due` enqueues a job through it for every due tick, carrying the schedule's
//! stored payload, job id, and `max_attempts`.

use super::cron::Cron;
use super::{
    MAX_AT_HORIZON_DAYS, MAX_NAME_BYTES, Schedule, ScheduleInfo, ScheduleKind, ScheduleOpts,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::{ForgeError, Result};
use crate::queue::{EnqueueOpts, JobId, MAX_PAYLOAD_BYTES, Queue};
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;
use uuid::Uuid;

/// A tick more than this far late (e.g. every replica was down) fires once on recovery
/// only if within the window, else is skipped and logged. Matches the Postgres backend's
/// `MISSED_TICK_GRACE_SECS` (k8s `startingDeadlineSeconds`).
const MISSED_TICK_GRACE_SECS: f64 = 60.0 * 60.0;
/// In-process claim timeout for a due schedule. If a `process_due` future is cancelled
/// after claiming but before settling, a later pass may retry after this window.
const CLAIM_TIMEOUT_SECS: i64 = 30;

/// One registered schedule. Times are held as [`DateTime<Utc>`] for cron math; the
/// `SystemTime` boundary conversion happens only in `at` (in) and `list` (out).
struct ScheduleEntry {
    kind: ScheduleKind,
    /// Stored (namespaced) target-queue name, matching `PgSchedule`'s prefixing.
    target_queue: String,
    /// The body each tick enqueues.
    payload: Bytes,
    /// One-shot job id returned by `at`; cron ticks use deterministic ids.
    job_id: Option<JobId>,
    /// Delivery options carried onto the enqueued job (currently `max_attempts`).
    opts: ScheduleOpts,
    next_run: DateTime<Utc>,
    last_run: Option<DateTime<Utc>>,
    claim: Option<(Uuid, DateTime<Utc>)>,
}

pub(crate) struct MemSchedule {
    state: Mutex<HashMap<String, ScheduleEntry>>,
    /// App namespace mixed into the stored target-queue name so a scheduled enqueue
    /// names this app's queue. Empty = the unnamespaced app.
    app: String,
    /// Resolved queue backend; a due tick enqueues through it.
    queue: Arc<dyn Queue>,
}

impl MemSchedule {
    pub(crate) fn new(app: String, queue: Arc<dyn Queue>) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            app,
            queue,
        }
    }

    /// Take the map lock, recovering the guard if a previous holder panicked. The
    /// critical sections are short and synchronous (no `await` held across the lock), so a
    /// poisoned lock never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ScheduleEntry>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The stored (namespaced) target-queue name, matching `PgSchedule`'s prefixing.
    fn physical_queue(&self, queue: &str) -> String {
        crate::util::namespaced(&self.app, queue)
    }

    /// Strip the namespace prefix from a stored target-queue name.
    fn logical_queue<'a>(&self, stored: &'a str) -> &'a str {
        if self.app.is_empty() {
            stored
        } else {
            stored
                .strip_prefix(&self.app)
                .and_then(|s| s.strip_prefix(':'))
                .unwrap_or(stored)
        }
    }

    /// Stable id for one cron tick, matching the Postgres scheduler's scheme. Makes retry
    /// after a successful enqueue but unsettled claim idempotent for built-in queue
    /// backends and custom queues that honor `EnqueueOpts::job_id`.
    fn tick_job_id(&self, name: &str, next_run: DateTime<Utc>) -> JobId {
        let mut h = Sha256::new();
        h.update(b"forge:schedule:tick:v1");
        h.update([0]);
        h.update(self.app.as_bytes());
        h.update([0]);
        h.update(name.as_bytes());
        h.update([0]);
        let ts = next_run
            .timestamp_nanos_opt()
            .map_or_else(|| next_run.to_rfc3339(), |n| n.to_string());
        h.update(ts.as_bytes());

        let digest = h.finalize();
        let mut bytes = [0u8; 16];
        for (dst, src) in bytes.iter_mut().zip(digest.iter()) {
            *dst = *src;
        }
        if let Some(b) = bytes.get_mut(6) {
            *b = (*b & 0x0f) | 0x80;
        }
        if let Some(b) = bytes.get_mut(8) {
            *b = (*b & 0x3f) | 0x80;
        }
        JobId(Uuid::from_bytes(bytes))
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ForgeError::invalid("schedule name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(ForgeError::limit(format!(
            "schedule name is {} bytes; max is {MAX_NAME_BYTES}",
            name.len()
        )));
    }
    Ok(())
}

/// Validate the target queue name (same rules as `queue`, checked at registration so a
/// bad name fails now rather than silently at tick time).
fn check_queue(queue: &str) -> Result<()> {
    if queue.is_empty() {
        return Err(ForgeError::invalid("target queue must not be empty"));
    }
    if !queue
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(ForgeError::invalid(
            "target queue may only contain [A-Za-z0-9_.-]",
        ));
    }
    if queue.ends_with(".dlq") {
        return Err(ForgeError::invalid(
            "target queue must not end in '.dlq' (reserved)",
        ));
    }
    Ok(())
}

fn check_payload(payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ForgeError::limit(format!(
            "payload is {} bytes; max is {MAX_PAYLOAD_BYTES}",
            payload.len()
        )));
    }
    Ok(())
}

#[async_trait]
impl Schedule for MemSchedule {
    async fn cron(
        &self,
        name: &str,
        expr: &str,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<()> {
        check_name(name)?;
        check_queue(queue)?;
        check_payload(&payload)?;
        let next = Cron::parse(expr)?
            .next_after(Utc::now())
            .ok_or_else(|| ForgeError::invalid("cron expression never fires"))?;
        // Insert replaces any existing schedule of the same name, resetting last_run; the
        // in-process analogue of the Postgres `ON CONFLICT (name, app) DO UPDATE`.
        self.lock().insert(
            name.to_string(),
            ScheduleEntry {
                kind: ScheduleKind::Cron(expr.to_string()),
                target_queue: self.physical_queue(queue),
                payload,
                job_id: None,
                opts,
                next_run: next,
                last_run: None,
                claim: None,
            },
        );
        Ok(())
    }

    async fn at(
        &self,
        when: SystemTime,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<JobId> {
        check_queue(queue)?;
        check_payload(&payload)?;
        let when_dt: DateTime<Utc> = when.into();
        // A past/now `when` is allowed (it fires on the next tick within grace); only an
        // absurdly-far-future `when` is rejected, matching the contract's ~100-year
        // ceiling so every backend agrees on the horizon.
        if when_dt > Utc::now() + chrono::Duration::days(MAX_AT_HORIZON_DAYS) {
            return Err(ForgeError::limit("at `when` exceeds the ~100-year ceiling"));
        }
        let job_id = JobId::new();
        // The `at:<job_id>` name is the link `cancel_at` resolves back to.
        let name = format!("at:{job_id}");
        self.lock().insert(
            name,
            ScheduleEntry {
                kind: ScheduleKind::At,
                target_queue: self.physical_queue(queue),
                payload,
                job_id: Some(job_id),
                opts,
                next_run: when_dt,
                last_run: None,
                claim: None,
            },
        );
        Ok(job_id)
    }

    async fn cancel(&self, name: &str) -> Result<bool> {
        Ok(self.lock().remove(name).is_some())
    }

    async fn list(
        &self,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<ScheduleInfo>, Option<Cursor>)> {
        let limit = limit.clamp(1, 10_000) as usize;
        // Keyset pagination over the name, like the Postgres backend: the cursor token is
        // the last name returned, and `name` is the unique order column.
        let after = cursor.map(|c| c.token().to_string());
        let state = self.lock();
        let mut names: Vec<&String> = state
            .keys()
            .filter(|n| after.as_deref().is_none_or(|a| n.as_str() > a))
            .collect();
        names.sort();
        names.truncate(limit);
        let next = if names.len() < limit {
            None
        } else {
            names.last().map(|n| Cursor::from_token((*n).clone()))
        };
        let items = names
            .iter()
            .filter_map(|n| {
                state.get(*n).map(|e| {
                    ScheduleInfo::new(
                        (*n).clone(),
                        e.kind.clone(),
                        self.logical_queue(&e.target_queue).to_string(),
                        e.next_run.into(),
                        e.last_run.map(Into::into),
                    )
                })
            })
            .collect();
        Ok((items, next))
    }

    async fn process_due(&self) -> Result<u64> {
        let now = Utc::now();
        struct DuePlan {
            name: String,
            claim: Uuid,
            next_run: DateTime<Utc>,
            enqueue: Option<(String, Bytes, EnqueueOpts)>,
            next: Option<DateTime<Utc>>,
        }

        let claim_matches = |entry: &ScheduleEntry, next_run: DateTime<Utc>, claim: Uuid| {
            entry.next_run == next_run && entry.claim.as_ref().is_some_and(|(id, _)| *id == claim)
        };

        // Phase 1 (locked, synchronous): claim due schedules and collect the work to do.
        // The schedule state is not advanced yet; if enqueue fails, the claim is cleared
        // and a later pass retries instead of losing the tick.
        let plans: Vec<DuePlan> = {
            let mut state = self.lock();
            let stale_before = now - chrono::Duration::seconds(CLAIM_TIMEOUT_SECS);
            // Due schedules in next_run order, mirroring the Postgres `ORDER BY next_run`.
            // There is no batch cap: with no row locking to contend for, one pass can fire
            // every due schedule.
            let mut due: Vec<(DateTime<Utc>, String)> = state
                .iter()
                .filter(|(_, e)| {
                    e.next_run <= now
                        && e.claim
                            .as_ref()
                            .is_none_or(|(_, claimed_at)| *claimed_at <= stale_before)
                })
                .map(|(name, e)| (e.next_run, name.clone()))
                .collect();
            due.sort();

            let mut plans = Vec::new();
            for (_, name) in due {
                let Some(entry) = state.get_mut(&name) else {
                    continue;
                };
                let claim = Uuid::new_v4();
                entry.claim = Some((claim, now));
                let next_run = entry.next_run;
                let cron = match &entry.kind {
                    ScheduleKind::Cron(expr) => Cron::parse(expr).ok(),
                    ScheduleKind::At => None,
                };

                // For a cron the grace is measured from the most-recent missed tick, so a fast
                // cron that fell behind during an outage still fires its latest tick instead of
                // being skipped wholesale. A one-shot (no cron) keeps its next_run.
                let base_lateness = (now - next_run).num_seconds() as f64;
                let lateness = cron
                    .as_ref()
                    .and_then(|c| c.prev_or_at(now))
                    .map_or(base_lateness, |prev| (now - prev).num_seconds() as f64);
                let enqueue = if lateness <= MISSED_TICK_GRACE_SECS {
                    // Unset `max_attempts` inherits the queue's own enqueue default, matching
                    // the Postgres tick's `unwrap_or` path.
                    let mut eo = EnqueueOpts::new();
                    if let Some(m) = entry.opts.max_attempts {
                        eo.max_attempts = m;
                    }
                    let job_id = entry
                        .job_id
                        .unwrap_or_else(|| self.tick_job_id(&name, next_run));
                    eo = eo.with_job_id(job_id);
                    Some((
                        self.logical_queue(&entry.target_queue).to_string(),
                        entry.payload.clone(),
                        eo,
                    ))
                } else {
                    tracing::warn!(
                        schedule.name = %name,
                        lateness_secs = lateness,
                        "skipping missed schedule tick (past the grace window)"
                    );
                    None
                };

                // After a successful enqueue/skip, advance a cron to its next tick or drop a
                // one-shot (or a cron that will never fire again), matching Postgres.
                let next = cron.and_then(|c| c.next_after(now));
                plans.push(DuePlan {
                    name,
                    claim,
                    next_run,
                    enqueue,
                    next,
                });
            }
            plans
        };

        // Phase 2 (unlocked, async): enqueue each delivery through the resolved queue backend,
        // so a memory-backed schedule actually runs work. Phase 3 settles only after
        // enqueue succeeds, so transient queue errors leave the schedule retryable.
        let mut enqueued = 0u64;
        for plan in plans {
            if let Some((queue, payload, opts)) = plan.enqueue {
                if let Err(e) = self.queue.enqueue(&queue, payload, opts).await {
                    let mut state = self.lock();
                    if let Some(entry) = state.get_mut(&plan.name)
                        && claim_matches(entry, plan.next_run, plan.claim)
                    {
                        entry.claim = None;
                    }
                    return Err(e);
                }
                enqueued += 1;
            }

            let mut state = self.lock();
            let should_settle = state
                .get(&plan.name)
                .is_some_and(|entry| claim_matches(entry, plan.next_run, plan.claim));
            if !should_settle {
                continue;
            }
            match plan.next {
                Some(next) => {
                    if let Some(entry) = state.get_mut(&plan.name) {
                        entry.last_run = Some(now);
                        entry.next_run = next;
                        entry.claim = None;
                    }
                }
                None => {
                    state.remove(&plan.name);
                }
            }
        }
        Ok(enqueued)
    }
}

#[async_trait]
impl BackendLifecycle for MemSchedule {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Schedule
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process, not durable"
    }
    // No `maintain` override: firing/cleanup is `process_due`, driven by the scheduler
    // loop, and a skipped one-shot is removed there. Nothing else accumulates, so the
    // no-op default applies (same as the Postgres schedule backend).
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::queue::{DequeueOpts, Job, NackOpts, QueueDepth};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn b(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    /// A throwaway in-memory queue for the scheduler to deliver into.
    fn mem_queue() -> std::sync::Arc<crate::queue::MemQueue> {
        std::sync::Arc::new(crate::queue::MemQueue::new(
            Duration::from_secs(300),
            Duration::from_secs(86_400),
            String::new(),
        ))
    }

    /// A `MemSchedule` wired to a fresh throwaway queue, for tests that don't inspect delivery.
    fn sched(app: &str) -> MemSchedule {
        MemSchedule::new(app.to_string(), mem_queue())
    }

    struct FlakyQueue {
        inner: Arc<crate::queue::MemQueue>,
        enqueues: AtomicUsize,
    }

    impl FlakyQueue {
        fn new() -> Self {
            Self {
                inner: mem_queue(),
                enqueues: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Queue for FlakyQueue {
        async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId> {
            if self.enqueues.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ForgeError::unavailable("simulated queue outage"));
            }
            self.inner.enqueue(queue, payload, opts).await
        }

        async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>> {
            self.inner.dequeue(queue, opts).await
        }

        async fn ack(&self, job: &Job) -> Result<()> {
            self.inner.ack(job).await
        }

        async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()> {
            self.inner.nack(job, opts).await
        }

        async fn heartbeat(&self, job: &Job) -> Result<()> {
            self.inner.heartbeat(job).await
        }

        async fn depth(&self, queue: &str) -> Result<QueueDepth> {
            self.inner.depth(queue).await
        }
    }

    #[tokio::test]
    async fn cron_registers_and_lists_with_logical_queue() {
        let s = sched("");
        s.cron("nightly", "0 0 * * *", "jobs", b("x"), ScheduleOpts::new())
            .await
            .unwrap();
        let (items, next) = s.list(None, 100).await.unwrap();
        assert!(next.is_none());
        assert_eq!(items.len(), 1);
        let info = items.first().unwrap();
        assert_eq!(info.name, "nightly");
        assert_eq!(info.kind, ScheduleKind::Cron("0 0 * * *".to_string()));
        assert_eq!(info.queue, "jobs");
        assert!(info.last_run.is_none());
        assert!(
            info.next_run > SystemTime::now(),
            "next tick is in the future"
        );
    }

    #[tokio::test]
    async fn at_returns_a_job_id_and_lists_as_one_shot() {
        let s = sched("");
        let when = SystemTime::now() + Duration::from_secs(3600);
        let id = s
            .at(when, "jobs", b("x"), ScheduleOpts::new())
            .await
            .unwrap();
        let (items, _) = s.list(None, 100).await.unwrap();
        assert_eq!(items.len(), 1);
        let info = items.first().unwrap();
        assert_eq!(info.kind, ScheduleKind::At);
        assert_eq!(info.queue, "jobs");
        assert_eq!(info.name, format!("at:{id}"), "name encodes the job id");
    }

    #[tokio::test]
    async fn cron_reregistration_replaces_in_place() {
        let s = sched("");
        s.cron("daily", "0 0 * * *", "a", b("x"), ScheduleOpts::new())
            .await
            .unwrap();
        s.cron("daily", "0 9 * * *", "b", b("y"), ScheduleOpts::new())
            .await
            .unwrap();
        let (items, _) = s.list(None, 100).await.unwrap();
        assert_eq!(items.len(), 1, "re-register replaces, not duplicates");
        let info = items.first().unwrap();
        assert_eq!(info.kind, ScheduleKind::Cron("0 9 * * *".to_string()));
        assert_eq!(info.queue, "b");
    }

    #[tokio::test]
    async fn process_due_fires_a_past_one_shot_and_removes_it() {
        let s = sched("");
        s.at(
            SystemTime::now() - Duration::from_secs(5),
            "jobs",
            b("x"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();
        assert_eq!(s.process_due().await.unwrap(), 1, "due one-shot fires");
        let (items, _) = s.list(None, 100).await.unwrap();
        assert!(items.is_empty(), "a fired one-shot is removed");
    }

    #[tokio::test]
    async fn process_due_skips_a_one_shot_past_grace_but_still_cleans_it() {
        let s = sched("");
        s.at(
            SystemTime::now() - Duration::from_secs(2 * 3600),
            "jobs",
            b("x"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            s.process_due().await.unwrap(),
            0,
            "past the 1h grace window"
        );
        let (items, _) = s.list(None, 100).await.unwrap();
        assert!(items.is_empty(), "a skipped one-shot is still cleaned up");
    }

    #[tokio::test]
    async fn process_due_fires_a_behind_cron_via_its_most_recent_tick() {
        let s = sched("");
        // A fast cron two hours behind: its latest missed tick is only seconds late, so
        // the grace check passes and it fires (not skipped wholesale).
        {
            let mut st = s.lock();
            st.insert(
                "behind".to_string(),
                ScheduleEntry {
                    kind: ScheduleKind::Cron("* * * * *".to_string()),
                    target_queue: "jobs".to_string(),
                    payload: b("x"),
                    job_id: None,
                    opts: ScheduleOpts::new(),
                    next_run: Utc::now() - chrono::Duration::hours(2),
                    last_run: None,
                    claim: None,
                },
            );
        }
        assert_eq!(s.process_due().await.unwrap(), 1);

        let st = s.lock();
        let entry = st.get("behind");
        assert!(entry.is_some(), "a recurring schedule survives a tick");
        let next_run = entry.map(|e| e.next_run);
        assert!(
            next_run.is_some_and(|n| n > Utc::now() - chrono::Duration::minutes(5)),
            "next tick advanced from 2h-ago to roughly now"
        );
        assert!(
            st.get("behind").and_then(|e| e.last_run).is_some(),
            "last_run recorded"
        );
    }

    #[tokio::test]
    async fn list_paginates_by_name() {
        let s = sched("");
        for i in 0..5 {
            s.cron(
                &format!("s{i:02}"),
                "0 0 * * *",
                "jobs",
                b("x"),
                ScheduleOpts::new(),
            )
            .await
            .unwrap();
        }

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let (items, next) = s.list(cursor, 2).await.unwrap();
            seen.extend(items.into_iter().map(|i| i.name));
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 5, "exactly the five schedules, no dupes");
        assert_eq!(seen.first().map(String::as_str), Some("s00"));
    }

    #[tokio::test]
    async fn namespaces_prefix_the_queue_and_isolate_instances() {
        let a = sched("app_a");
        let bb = sched("app_b");
        a.cron("job", "0 0 * * *", "q", b("x"), ScheduleOpts::new())
            .await
            .unwrap();
        bb.cron("job", "0 0 * * *", "q", b("y"), ScheduleOpts::new())
            .await
            .unwrap();

        // list reports the logical queue name in both apps.
        let (ia, _) = a.list(None, 100).await.unwrap();
        let (ib, _) = bb.list(None, 100).await.unwrap();
        assert_eq!(ia.len(), 1);
        assert_eq!(ib.len(), 1);
        assert_eq!(ia.first().map(|i| i.queue.as_str()), Some("q"));
        assert_eq!(ib.first().map(|i| i.queue.as_str()), Some("q"));

        // ...but the stored queue carries the app prefix, like the Postgres backend.
        let st = a.lock();
        assert_eq!(
            st.get("job").map(|e| e.target_queue.as_str()),
            Some("app_a:q")
        );
    }

    #[tokio::test]
    async fn cancel_removes_and_reports_presence() {
        let s = sched("");
        s.cron("x", "0 0 * * *", "jobs", b("x"), ScheduleOpts::new())
            .await
            .unwrap();
        assert!(s.cancel("x").await.unwrap(), "removed an existing schedule");
        assert!(!s.cancel("x").await.unwrap(), "already gone");
        assert!(!s.cancel("never").await.unwrap(), "never existed");
    }

    #[tokio::test]
    async fn cancel_at_recalls_a_pending_one_shot() {
        let s = sched("");
        let id = s
            .at(
                SystemTime::now() + Duration::from_secs(3600),
                "jobs",
                b("x"),
                ScheduleOpts::new(),
            )
            .await
            .unwrap();
        assert!(s.cancel_at(id).await.unwrap(), "pending one-shot recalled");
        assert!(!s.cancel_at(id).await.unwrap(), "already cancelled");
    }

    #[tokio::test]
    async fn invalid_registrations_are_rejected() {
        let s = sched("");
        assert!(matches!(
            s.cron("", "0 0 * * *", "q", b("x"), ScheduleOpts::new())
                .await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            s.cron("n", "0 0 * * *", "bad queue!", b("x"), ScheduleOpts::new())
                .await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            s.cron("n", "0 0 * * *", "jobs.dlq", b("x"), ScheduleOpts::new())
                .await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            s.cron("n", "not a cron", "q", b("x"), ScheduleOpts::new())
                .await,
            Err(ForgeError::Invalid(_))
        ));
        // Feb 30 never exists, so the expression parses but never fires.
        assert!(matches!(
            s.cron("n", "0 0 30 2 *", "q", b("x"), ScheduleOpts::new())
                .await,
            Err(ForgeError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn at_beyond_the_horizon_is_a_limit_error() {
        let s = sched("");
        let far =
            SystemTime::now() + Duration::from_secs((MAX_AT_HORIZON_DAYS as u64 + 5) * 86_400);
        assert!(matches!(
            s.at(far, "jobs", b("x"), ScheduleOpts::new()).await,
            Err(ForgeError::Limit(_))
        ));
    }

    #[tokio::test]
    async fn process_due_delivers_a_job_to_the_queue() {
        use crate::queue::DequeueOpts;
        let q = mem_queue();
        let s = MemSchedule::new(String::new(), q.clone());
        // A one-shot 2s in the past fires on the next tick (within the grace window).
        let when = SystemTime::now() - Duration::from_secs(2);
        let id = s
            .at(
                when,
                "oneshot",
                b("hello"),
                ScheduleOpts::new().with_max_attempts(1),
            )
            .await
            .unwrap();
        // The tick enqueues exactly one job...
        assert_eq!(s.process_due().await.unwrap(), 1);
        // ...carrying the scheduled payload + max_attempts, deliverable from the queue.
        let mut dq = DequeueOpts::new();
        dq.wait = Duration::ZERO;
        let job = q
            .dequeue("oneshot", dq)
            .await
            .unwrap()
            .expect("the scheduled job was enqueued");
        assert_eq!(job.id, id);
        assert_eq!(job.payload.as_ref(), b"hello");
        assert_eq!(job.max_attempts, 1);
        // The one-shot is consumed: a second tick delivers nothing.
        assert_eq!(s.process_due().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn process_due_keeps_schedule_pending_when_queue_enqueue_fails() {
        let q = Arc::new(FlakyQueue::new());
        let s = MemSchedule::new(String::new(), q.clone());
        let id = s
            .at(
                SystemTime::now() - Duration::from_secs(2),
                "oneshot",
                b("hello"),
                ScheduleOpts::new(),
            )
            .await
            .unwrap();

        assert!(matches!(
            s.process_due().await,
            Err(ForgeError::Unavailable(_))
        ));
        let (items, _) = s.list(None, 100).await.unwrap();
        assert_eq!(
            items.len(),
            1,
            "failed enqueue leaves the schedule retryable"
        );

        assert_eq!(s.process_due().await.unwrap(), 1);
        let job = q
            .dequeue("oneshot", DequeueOpts::new().with_wait(Duration::ZERO))
            .await
            .unwrap()
            .expect("retry enqueued the due job");
        assert_eq!(job.id, id);
        let (items, _) = s.list(None, 100).await.unwrap();
        assert!(items.is_empty(), "successful retry consumes the one-shot");
    }
}

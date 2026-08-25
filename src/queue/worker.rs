use super::{DequeueOpts, Job, JobId, NackOpts, Queue};
use crate::obs::Observability;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{Instrument, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerFailureKind {
    Dequeue,
    Handler,
    Heartbeat,
    LeaseLost,
    Settle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerFailure {
    pub kind: WorkerFailureKind,
    pub job_id: Option<JobId>,
    pub retryable: bool,
}

type ErrorCallback = Arc<dyn Fn(WorkerFailure) + Send + Sync>;

pub(crate) struct WorkerTracker {
    active: AtomicUsize,
    changed: tokio::sync::Notify,
}

impl WorkerTracker {
    pub(crate) fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            changed: tokio::sync::Notify::new(),
        }
    }

    fn enter(self: &Arc<Self>) -> WorkerGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        WorkerGuard(self.clone())
    }

    pub(crate) async fn drained(&self) {
        while self.active.load(Ordering::Acquire) != 0 {
            self.changed.notified().await;
        }
    }

    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

struct WorkerGuard(Arc<WorkerTracker>);

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

/// Builder for a managed worker. Obtain one from `Forge::worker(queue_name)`.
pub struct WorkerBuilder {
    queue: Arc<dyn Queue>,
    name: String,
    concurrency: usize,
    visibility_timeout: Duration,
    poll_wait: Duration,
    grace: Duration,
    heartbeat_cadence: Duration,
    retry_backoff: Duration,
    identity: String,
    on_error: Option<ErrorCallback>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    tracker: Arc<WorkerTracker>,
    obs: Arc<Observability>,
}

impl WorkerBuilder {
    pub(crate) fn new(
        queue: Arc<dyn Queue>,
        name: impl Into<String>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        tracker: Arc<WorkerTracker>,
        obs: Arc<Observability>,
    ) -> Self {
        Self {
            queue,
            name: name.into(),
            concurrency: 1,
            visibility_timeout: Duration::from_secs(30),
            poll_wait: Duration::from_secs(20),
            grace: Duration::from_secs(30),
            heartbeat_cadence: Duration::from_secs(10),
            retry_backoff: Duration::from_secs(1),
            identity: "worker".to_string(),
            on_error: None,
            shutdown,
            tracker,
            obs,
        }
    }

    /// Shutdown wait for in-flight handlers before abort; aborted leases expire
    /// and redeliver (at-least-once). Default 30s.
    pub fn grace(mut self, d: Duration) -> Self {
        self.grace = d;
        self
    }

    /// Maximum jobs processed at once. Default 1.
    pub fn concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Lease/visibility timeout for each dequeue. The worker auto-heartbeats at
    /// roughly a third of this while a handler runs. Default 30s.
    pub fn visibility_timeout(mut self, d: Duration) -> Self {
        self.visibility_timeout = d;
        if self.heartbeat_cadence >= d {
            self.heartbeat_cadence = (d / 3).max(Duration::from_millis(1));
        }
        self
    }

    /// Heartbeat cadence. Must be shorter than the visibility timeout.
    pub fn heartbeat_cadence(mut self, d: Duration) -> Self {
        self.heartbeat_cadence = d;
        self
    }

    /// Base delay after a dequeue/backend failure. Each wait receives bounded
    /// 80–120% jitter and is capped at 30 seconds.
    pub fn retry_backoff(mut self, d: Duration) -> Self {
        self.retry_backoff = d.min(Duration::from_secs(30));
        self
    }

    /// Low-cardinality identity included in worker diagnostics, never metric labels.
    pub fn identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = identity.into();
        self
    }

    /// Receive classified worker failures without payloads or raw exception strings.
    pub fn on_error(mut self, callback: impl Fn(WorkerFailure) + Send + Sync + 'static) -> Self {
        self.on_error = Some(Arc::new(callback));
        self
    }

    /// Long-poll wait per dequeue. Default 20s (the SQS maximum).
    pub fn poll_wait(mut self, d: Duration) -> Self {
        self.poll_wait = d;
        self
    }

    /// Run until the owning Forge begins shutdown, then drain in-flight handlers.
    /// Applications own process signals and call [`crate::Forge::close`].
    pub async fn run<H, F, E>(self, handler: H)
    where
        H: Fn(Job) -> F + Send + Sync + 'static,
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: Send + 'static,
    {
        self.run_until(std::future::pending(), handler).await;
    }

    /// Run until `shutdown` resolves, then stop dequeuing and wait (bounded by a
    /// grace period) for in-flight handlers to finish.
    pub async fn run_until<H, F, E, S>(self, shutdown: S, handler: H)
    where
        H: Fn(Job) -> F + Send + Sync + 'static,
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: Send + 'static,
        S: Future<Output = ()> + Send,
    {
        let _worker_guard = self.tracker.enter();
        if self.heartbeat_cadence.is_zero() || self.heartbeat_cadence >= self.visibility_timeout {
            report(&self.on_error, WorkerFailureKind::Heartbeat, None, false);
            return;
        }
        let handler = Arc::new(handler);
        let permits = Arc::new(Semaphore::new(self.concurrency));
        let mut in_flight = JoinSet::new();
        let (cancel_handlers, _) = tokio::sync::watch::channel(false);
        let hb_interval = self.heartbeat_cadence;
        let mut retry_attempt = 0u32;

        let mut forge_shutdown = self.shutdown.clone();
        let combined_shutdown = async move {
            tokio::select! {
                _ = shutdown => {},
                _ = forge_shutdown.wait_for(|closing| *closing) => {},
            }
        };
        let mut shutdown = std::pin::pin!(combined_shutdown);
        loop {
            // Acquire before dequeue: avoids pulling a job we can't run yet.
            let permit = {
                let acquire = Arc::clone(&permits).acquire_owned();
                tokio::select! {
                    p = acquire => p.expect("semaphore is never closed"),
                    _ = &mut shutdown => break,
                }
            };

            let job = tokio::select! {
                r = self.queue.dequeue(
                    &self.name,
                    DequeueOpts::new()
                        .with_wait(self.poll_wait)
                        .with_visibility_timeout(self.visibility_timeout),
                ) => r,
                _ = &mut shutdown => break,
            };

            let job = match job {
                Ok(Some(job)) => job,
                Ok(None) => {
                    drop(permit);
                    continue;
                }
                Err(e) => {
                    let retryable = e.is_retryable();
                    warn!(retryable, queue = %self.name, worker.identity = %self.identity, "dequeue failed");
                    report(&self.on_error, WorkerFailureKind::Dequeue, None, retryable);
                    drop(permit);
                    if !retryable {
                        break;
                    }
                    retry_attempt = retry_attempt.saturating_add(1);
                    tokio::time::sleep(jittered(self.retry_backoff, retry_attempt)).await;
                    continue;
                }
            };
            retry_attempt = 0;

            // If cancellation became ready at the same boundary as dequeue, release
            // the freshly leased job and never start user code.
            let start = tokio::select! {
                biased;
                _ = &mut shutdown => false,
                _ = std::future::ready(()) => true,
            };
            if !start {
                let _ = self
                    .queue
                    .nack(&job, NackOpts::retry_in(Duration::ZERO))
                    .await;
                drop(permit);
                break;
            }

            let queue = Arc::clone(&self.queue);
            let handler = Arc::clone(&handler);
            let on_error = self.on_error.clone();
            let identity = self.identity.clone();
            let cancel = cancel_handlers.subscribe();
            let obs = self.obs.clone();
            let process_span = tracing::info_span!(
                "forge.messaging.process",
                messaging.system = "forge",
                messaging.operation.name = "process",
                messaging.destination.name = %self.name,
                messaging.message.id = %job.id,
                messaging.message.delivery_count = job.attempt,
            );
            #[cfg(feature = "otel")]
            if let Some(context) = &job.trace_context {
                context.apply_to_span(&process_span, job.attempt > 1);
            }
            in_flight.spawn(
                async move {
                    let context = SupervisorContext {
                        queue,
                        hb_interval,
                        on_error,
                        identity,
                        obs,
                    };
                    supervise(context, handler, job, cancel).await;
                    drop(permit);
                }
                .instrument(process_span),
            );

            while let Some(res) = in_flight.try_join_next() {
                if let Err(e) = res {
                    warn!(error = %e, "worker supervisor task failed");
                }
            }
        }

        warn!(queue = %self.name, worker.identity = %self.identity, in_flight = in_flight.len(), grace = ?self.grace, "worker shutting down; draining");
        let drain = async { while in_flight.join_next().await.is_some() {} };
        if tokio::time::timeout(self.grace, drain).await.is_err() {
            warn!(
                queue = %self.name,
                "drain grace expired; aborting remaining handlers (their leases will expire and redeliver)"
            );
            let _ = cancel_handlers.send(true);
            // Supervisors cancel user handlers and release their leases before exiting.
            while in_flight.join_next().await.is_some() {}
        }
    }
}

struct SupervisorContext {
    queue: Arc<dyn Queue>,
    hb_interval: Duration,
    on_error: Option<ErrorCallback>,
    identity: String,
    obs: Arc<Observability>,
}

/// Run one job's handler with auto-heartbeat, then settle it (ack/nack).
async fn supervise<H, F, E>(
    context: SupervisorContext,
    handler: Arc<H>,
    job: Job,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) where
    H: Fn(Job) -> F + Send + Sync + 'static,
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: Send + 'static,
{
    let SupervisorContext {
        queue,
        hb_interval,
        on_error,
        identity,
        obs,
    } = context;
    let job_for_handler = job.clone();
    let mut task = tokio::task::spawn(async move { handler(job_for_handler).await });
    let mut ticker = tokio::time::interval(hb_interval);
    ticker.tick().await; // first tick fires immediately; skip it.
    let mut application_cancel_requested = false;

    let outcome = loop {
        tokio::select! {
            joined = &mut task => break Some(joined),
            changed = cancel.changed() => {
                let should_cancel = changed.is_err() || *cancel.borrow();
                if should_cancel {
                    task.abort();
                    let _ = task.await;
                    obs.counter("forge_queue_handler_outcomes_total", &[("outcome", "cancelled")], 1);
                    if let Err(e) = queue.nack(&job, NackOpts::retry_in(Duration::ZERO).with_failure_summary("worker shutdown interrupted the handler")).await {
                        report(&on_error, WorkerFailureKind::Settle, Some(job.id), e.is_retryable());
                    }
                    return;
                }
            }
            _ = ticker.tick() => {
                if application_cancel_requested {
                    continue;
                }
                match queue.cancellation_requested(&job).await {
                    Ok(true) => {
                        application_cancel_requested = true;
                        job.cancellation.signal();
                        obs.counter("forge_queue_handler_outcomes_total", &[("outcome", "cancel_requested")], 1);
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        report(&on_error, WorkerFailureKind::Heartbeat, Some(job.id), e.is_retryable());
                        warn!(retryable = e.is_retryable(), job.id = %job.id, worker.identity = %identity, "cancellation check failed; will retry");
                    }
                }
                match queue.heartbeat(&job).await {
                    Ok(()) => {}
                    Err(error) if error.code() == "PRECONDITION" => {
                        // Lease lost: another worker owns this job; abort and let it settle.
                        task.abort();
                        obs.counter("forge_queue_lease_loss_total", &[("operation", "worker_heartbeat")], 1);
                        report(&on_error, WorkerFailureKind::LeaseLost, Some(job.id), false);
                        warn!(job.id = %job.id, worker.identity = %identity, "lease lost mid-handler; abandoning");
                        break None;
                    }
                    Err(e) => {
                        report(&on_error, WorkerFailureKind::Heartbeat, Some(job.id), e.is_retryable());
                        warn!(retryable = e.is_retryable(), job.id = %job.id, worker.identity = %identity, "heartbeat failed; will retry");
                    },
                }
            }
        }
    };

    if application_cancel_requested {
        if let Err(e) = queue.finish_cancellation(&job).await {
            report(
                &on_error,
                WorkerFailureKind::Settle,
                Some(job.id),
                e.is_retryable(),
            );
        }
        return;
    }

    match outcome {
        Some(Ok(Ok(()))) => {
            obs.counter(
                "forge_queue_handler_outcomes_total",
                &[("outcome", "success")],
                1,
            );
            if let Err(e) = queue.ack(&job).await {
                let kind = if e.code() == "PRECONDITION" {
                    WorkerFailureKind::LeaseLost
                } else {
                    WorkerFailureKind::Settle
                };
                report(&on_error, kind, Some(job.id), e.is_retryable());
                warn!(retryable = e.is_retryable(), job.id = %job.id, worker.identity = %identity, "ack failed");
            }
        }
        Some(Ok(Err(_app_err))) => {
            obs.counter(
                "forge_queue_handler_outcomes_total",
                &[("outcome", "error")],
                1,
            );
            report(&on_error, WorkerFailureKind::Handler, Some(job.id), false);
            warn!(job.id = %job.id, worker.identity = %identity, "handler returned error; nacking");
            if let Err(e) = queue
                .nack(
                    &job,
                    NackOpts::default().with_failure_summary("handler returned an error"),
                )
                .await
            {
                report(
                    &on_error,
                    WorkerFailureKind::Settle,
                    Some(job.id),
                    e.is_retryable(),
                );
                warn!(retryable = e.is_retryable(), job.id = %job.id, worker.identity = %identity, "nack failed");
            }
        }
        Some(Err(join_err)) => {
            // JoinError covers both panic and cancellation; nack either way.
            obs.counter(
                "forge_queue_handler_outcomes_total",
                &[("outcome", "panic")],
                1,
            );
            report(&on_error, WorkerFailureKind::Handler, Some(job.id), false);
            warn!(cancelled = join_err.is_cancelled(), job.id = %job.id, worker.identity = %identity, "handler task stopped; nacking");
            if let Err(e) = queue
                .nack(
                    &job,
                    NackOpts::default().with_failure_summary("handler task stopped"),
                )
                .await
            {
                report(
                    &on_error,
                    WorkerFailureKind::Settle,
                    Some(job.id),
                    e.is_retryable(),
                );
                warn!(retryable = e.is_retryable(), job.id = %job.id, worker.identity = %identity, "nack failed");
            }
        }
        None => { /* lease lost; the new owner settles it */ }
    }
}

fn report(
    callback: &Option<ErrorCallback>,
    kind: WorkerFailureKind,
    job_id: Option<JobId>,
    retryable: bool,
) {
    if let Some(callback) = callback {
        callback(WorkerFailure {
            kind,
            job_id,
            retryable,
        });
    }
}

fn jittered(base: Duration, attempt: u32) -> Duration {
    if base.is_zero() {
        return base;
    }
    let exponent = 1u32.checked_shl(attempt.min(5)).unwrap_or(u32::MAX);
    let capped = base.saturating_mul(exponent).min(Duration::from_secs(30));
    let spread = 80 + (u64::from(attempt).wrapping_mul(37) % 41);
    capped.mul_f64(spread as f64 / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_jitter_is_bounded() {
        let value = jittered(Duration::from_secs(1), 2);
        assert!((Duration::from_millis(3200)..=Duration::from_millis(4800)).contains(&value));
        assert!(jittered(Duration::from_secs(30), 30) <= Duration::from_secs(36));
    }
}

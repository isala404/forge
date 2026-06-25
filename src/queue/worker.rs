//! Managed queue consumer: the `worker()` half of the queue contract.
//!
//! Dequeues up to `concurrency` jobs, runs a handler per job, auto-heartbeats
//! while it runs, acks on success / nacks on error or panic, and drains
//! in-flight work on shutdown. Backend-agnostic: drives `Arc<dyn Queue>` only.

use super::{DequeueOpts, Job, NackOpts, Queue};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{Instrument, warn};

/// Builder for a managed worker. Obtain one from `Forge::worker(queue_name)`.
pub struct WorkerBuilder {
    queue: Arc<dyn Queue>,
    name: String,
    concurrency: usize,
    visibility_timeout: Duration,
    poll_wait: Duration,
    grace: Duration,
}

impl WorkerBuilder {
    pub(crate) fn new(queue: Arc<dyn Queue>, name: impl Into<String>) -> Self {
        Self {
            queue,
            name: name.into(),
            concurrency: 1,
            visibility_timeout: Duration::from_secs(30),
            poll_wait: Duration::from_secs(20),
            grace: Duration::from_secs(30),
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
        self
    }

    /// Long-poll wait per dequeue. Default 20s (the SQS maximum).
    pub fn poll_wait(mut self, d: Duration) -> Self {
        self.poll_wait = d;
        self
    }

    /// Run until SIGINT/SIGTERM, then drain in-flight handlers.
    pub async fn run<H, F, E>(self, handler: H)
    where
        H: Fn(Job) -> F + Send + Sync + 'static,
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        self.run_until(shutdown_signal(), handler).await;
    }

    /// Run until `shutdown` resolves, then stop dequeuing and wait (bounded by a
    /// grace period) for in-flight handlers to finish.
    pub async fn run_until<H, F, E, S>(self, shutdown: S, handler: H)
    where
        H: Fn(Job) -> F + Send + Sync + 'static,
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
        S: Future<Output = ()> + Send,
    {
        let handler = Arc::new(handler);
        let permits = Arc::new(Semaphore::new(self.concurrency));
        let mut in_flight = JoinSet::new();
        let hb_interval = (self.visibility_timeout / 3).max(Duration::from_secs(1));

        let mut shutdown = std::pin::pin!(shutdown);
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
                    warn!(error = %e, queue = %self.name, "dequeue failed; backing off");
                    drop(permit);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let queue = Arc::clone(&self.queue);
            let handler = Arc::clone(&handler);
            in_flight.spawn(
                async move {
                    supervise(queue, handler, job, hb_interval).await;
                    drop(permit);
                }
                .in_current_span(),
            );

            while let Some(res) = in_flight.try_join_next() {
                if let Err(e) = res {
                    warn!(error = %e, "worker supervisor task failed");
                }
            }
        }

        warn!(queue = %self.name, in_flight = in_flight.len(), grace = ?self.grace, "worker shutting down; draining");
        let drain = async { while in_flight.join_next().await.is_some() {} };
        if tokio::time::timeout(self.grace, drain).await.is_err() {
            warn!(
                queue = %self.name,
                "drain grace expired; aborting remaining handlers (their leases will expire and redeliver)"
            );
            in_flight.abort_all();
            // Reap the aborted tasks so their JoinHandles don't leak.
            while in_flight.join_next().await.is_some() {}
        }
    }
}

/// Resolve on SIGINT (ctrl-c) or, on unix, SIGTERM. Shared by the managed worker and
/// the scheduler loop so both stop the same way.
pub(crate) async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    let _ = ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

/// Run one job's handler with auto-heartbeat, then settle it (ack/nack).
async fn supervise<H, F, E>(queue: Arc<dyn Queue>, handler: Arc<H>, job: Job, hb_interval: Duration)
where
    H: Fn(Job) -> F + Send + Sync + 'static,
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let job_for_handler = job.clone();
    let mut task = tokio::task::spawn(async move { handler(job_for_handler).await });
    let mut ticker = tokio::time::interval(hb_interval);
    ticker.tick().await; // first tick fires immediately; skip it.

    let outcome = loop {
        tokio::select! {
            joined = &mut task => break Some(joined),
            _ = ticker.tick() => {
                match queue.heartbeat(&job).await {
                    Ok(()) => {}
                    Err(crate::ForgeError::Precondition(_)) => {
                        // Lease lost: another worker owns this job; abort and let it settle.
                        task.abort();
                        warn!(job.id = %job.id, "lease lost mid-handler; abandoning");
                        break None;
                    }
                    Err(e) => warn!(error = %e, job.id = %job.id, "heartbeat failed; will retry"),
                }
            }
        }
    };

    match outcome {
        Some(Ok(Ok(()))) => {
            if let Err(e) = queue.ack(&job).await {
                warn!(error = %e, job.id = %job.id, "ack failed");
            }
        }
        Some(Ok(Err(app_err))) => {
            warn!(error = %app_err, job.id = %job.id, "handler returned error; nacking");
            if let Err(e) = queue.nack(&job, NackOpts::default()).await {
                warn!(error = %e, job.id = %job.id, "nack failed");
            }
        }
        Some(Err(join_err)) => {
            // JoinError covers both panic and cancellation; nack either way.
            warn!(error = %join_err, job.id = %job.id, "handler panicked; nacking");
            if let Err(e) = queue.nack(&job, NackOpts::default()).await {
                warn!(error = %e, job.id = %job.id, "nack failed");
            }
        }
        None => { /* lease lost; the new owner settles it */ }
    }
}

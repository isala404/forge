use crate::error::{ForgeError, Result};
use crate::queue::{EnqueueOpts, JobId};
use crate::{Forge, Runtime};
use bytes::Bytes;
use sqlx::Row;
use std::future::Future;
use std::time::Duration;
use tracing::Instrument as _;
use uuid::Uuid;

pub const OUTBOX_TABLE: &str = "app_forge_outbox_v1";
pub const OUTBOX_SCHEMA_SQL: &str = include_str!("../sql/forge_outbox_v1.sql");

#[derive(Debug, Clone)]
pub struct OutboxRelayOpts {
    pub batch_size: u32,
    pub claim_for: Duration,
    pub failure_backoff: Duration,
    pub idle_delay: Duration,
    /// Only these W3C baggage keys are copied from application-owned outbox rows.
    pub baggage_allowlist: Vec<String>,
}

impl Default for OutboxRelayOpts {
    fn default() -> Self {
        Self {
            batch_size: 50,
            claim_for: Duration::from_secs(30),
            failure_backoff: Duration::from_secs(1),
            idle_delay: Duration::from_millis(500),
            baggage_allowlist: Vec::new(),
        }
    }
}

impl OutboxRelayOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_batch_size(mut self, batch_size: u32) -> Self {
        self.batch_size = batch_size;
        self
    }

    pub fn with_claim_for(mut self, claim_for: Duration) -> Self {
        self.claim_for = claim_for;
        self
    }

    pub fn with_failure_backoff(mut self, failure_backoff: Duration) -> Self {
        self.failure_backoff = failure_backoff;
        self
    }

    pub fn with_idle_delay(mut self, idle_delay: Duration) -> Self {
        self.idle_delay = idle_delay;
        self
    }

    pub fn with_baggage_allowlist(mut self, baggage_allowlist: Vec<String>) -> Self {
        self.baggage_allowlist = baggage_allowlist;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutboxRelayReport {
    pub claimed: u32,
    pub dispatched: u32,
    pub failed: u32,
    pub pending: u64,
    pub oldest_pending_age_ms: Option<u64>,
}

struct ClaimedRow {
    event_id: Uuid,
    destination: String,
    payload: Vec<u8>,
    delay_seconds: f64,
    max_attempts: i32,
    dedup_id: Option<String>,
    traceparent: Option<String>,
    tracestate: Option<String>,
    baggage: Option<String>,
}

impl Forge {
    /// Claim and dispatch one bounded batch from the application-owned v1 outbox.
    /// Replica claims use `SKIP LOCKED`; enqueue uses `event_id` as the deterministic
    /// job id, so replay after a crash between enqueue and mark is idempotent.
    #[allow(clippy::disallowed_methods)]
    pub async fn run_outbox_once(&self, opts: OutboxRelayOpts) -> Result<OutboxRelayReport> {
        self.ensure_open()?;
        validate_opts(&opts)?;
        let Runtime::Postgres(runtime) = &self.inner.runtime else {
            return Err(ForgeError::not_configured(
                "transactional outbox requires the PostgreSQL runtime",
            ));
        };
        let pool = &runtime.pool;
        let claim_token = Uuid::new_v4();
        let claim_span = tracing::info_span!(
            "forge.messaging.relay",
            messaging.system = "forge",
            messaging.operation.name = "receive",
        );
        let rows = async {
            sqlx::query(
                "WITH candidates AS (\
                 SELECT event_id FROM app_forge_outbox_v1 \
                 WHERE namespace = $1 AND available_at <= now() \
                   AND (dispatch_state = 'pending' \
                        OR (dispatch_state = 'claimed' AND claimed_until <= now())) \
                 ORDER BY available_at, created_at, event_id \
                 FOR UPDATE SKIP LOCKED LIMIT $2\
             ) \
             UPDATE app_forge_outbox_v1 o \
             SET dispatch_state = 'claimed', claim_token = $3, \
                 claimed_until = now() + make_interval(secs => $4), \
                 dispatch_attempts = dispatch_attempts + 1 \
             FROM candidates c WHERE o.event_id = c.event_id \
             RETURNING o.event_id, o.destination, o.payload, o.delay_seconds, \
                       o.max_attempts, o.dedup_id, o.traceparent, o.tracestate, o.baggage",
            )
            .bind(&self.inner.namespace)
            .bind(i64::from(opts.batch_size))
            .bind(claim_token)
            .bind(opts.claim_for.as_secs_f64())
            .fetch_all(pool)
            .await
        }
        .instrument(claim_span)
        .await?;

        let claimed = rows
            .into_iter()
            .map(|row| -> Result<ClaimedRow> {
                Ok(ClaimedRow {
                    event_id: row.try_get("event_id")?,
                    destination: row.try_get("destination")?,
                    payload: row.try_get("payload")?,
                    delay_seconds: row.try_get("delay_seconds")?,
                    max_attempts: row.try_get("max_attempts")?,
                    dedup_id: row.try_get("dedup_id")?,
                    traceparent: row.try_get("traceparent")?,
                    tracestate: row.try_get("tracestate")?,
                    baggage: row.try_get("baggage")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut report = OutboxRelayReport {
            claimed: u32::try_from(claimed.len()).unwrap_or(u32::MAX),
            ..OutboxRelayReport::default()
        };
        for row in claimed {
            let mut enqueue = EnqueueOpts::new()
                .with_job_id(JobId(row.event_id))
                .with_delay(Duration::from_secs_f64(row.delay_seconds))
                .with_max_attempts(u32::try_from(row.max_attempts).unwrap_or(5));
            if let Some(dedup_id) = row.dedup_id {
                enqueue = enqueue.with_dedup_id(dedup_id);
            }
            if let Some(traceparent) = row.traceparent {
                enqueue = enqueue.with_trace_context(crate::TraceContext::from_headers(
                    traceparent,
                    row.tracestate,
                    row.baggage,
                    &opts.baggage_allowlist,
                )?);
            }
            match self
                .queue()
                .enqueue(&row.destination, Bytes::from(row.payload), enqueue)
                .await
            {
                Ok(_) => {
                    let marked = sqlx::query(
                        "UPDATE app_forge_outbox_v1 SET dispatch_state = 'dispatched', \
                             dispatched_at = now(), claimed_until = NULL, claim_token = NULL, \
                             failure_summary = NULL \
                         WHERE event_id = $1 AND dispatch_state = 'claimed' AND claim_token = $2",
                    )
                    .bind(row.event_id)
                    .bind(claim_token)
                    .execute(pool)
                    .await?;
                    if marked.rows_affected() == 1 {
                        report.dispatched = report.dispatched.saturating_add(1);
                    }
                }
                Err(error) => {
                    let summary = safe_error_summary(&error);
                    sqlx::query(
                        "UPDATE app_forge_outbox_v1 SET dispatch_state = 'pending', \
                             available_at = now() + make_interval(secs => $3), \
                             claimed_until = NULL, claim_token = NULL, failure_summary = $4 \
                         WHERE event_id = $1 AND dispatch_state = 'claimed' AND claim_token = $2",
                    )
                    .bind(row.event_id)
                    .bind(claim_token)
                    .bind(opts.failure_backoff.as_secs_f64())
                    .bind(summary)
                    .execute(pool)
                    .await?;
                    report.failed = report.failed.saturating_add(1);
                }
            }
        }

        let backlog = sqlx::query(
            "SELECT count(*) AS pending, \
                    (EXTRACT(EPOCH FROM (now() - min(created_at))) * 1000)::double precision AS oldest_age_ms \
             FROM app_forge_outbox_v1 WHERE namespace = $1 AND dispatch_state <> 'dispatched'",
        )
        .bind(&self.inner.namespace)
        .fetch_one(pool)
        .await?;
        report.pending = u64::try_from(backlog.try_get::<i64, _>("pending")?).unwrap_or(0);
        report.oldest_pending_age_ms = backlog
            .try_get::<Option<f64>, _>("oldest_age_ms")?
            .map(|value| value.max(0.0) as u64);

        self.inner.obs.counter(
            "forge_outbox_dispatch_total",
            &[("outcome", "dispatched")],
            u64::from(report.dispatched),
        );
        self.inner.obs.counter(
            "forge_outbox_dispatch_total",
            &[("outcome", "failed")],
            u64::from(report.failed),
        );
        self.inner
            .obs
            .gauge("forge_outbox_pending", &[], report.pending as f64);
        if let Some(age) = report.oldest_pending_age_ms {
            self.inner.obs.gauge(
                "forge_outbox_oldest_pending_age_seconds",
                &[],
                age as f64 / 1000.0,
            );
        }
        Ok(report)
    }

    /// Run the outbox relay until Forge closes. The library installs no signals.
    pub async fn run_outbox_relay(&self, opts: OutboxRelayOpts) {
        let mut shutdown = self.inner.shutdown.subscribe();
        self.run_outbox_relay_until(
            async move {
                let _ = shutdown.wait_for(|closing| *closing).await;
            },
            opts,
        )
        .await;
    }

    /// Run the outbox relay until a language-native cancellation future resolves.
    pub async fn run_outbox_relay_until<S>(&self, shutdown: S, opts: OutboxRelayOpts)
    where
        S: Future<Output = ()> + Send,
    {
        if let Err(error) = validate_opts(&opts) {
            tracing::warn!(
                error.variant = safe_error_summary(&error),
                "outbox relay configuration is invalid; stopping"
            );
            return;
        }
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            match self.run_outbox_once(opts.clone()).await {
                Ok(report) if report.claimed > 0 => continue,
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error.variant = safe_error_summary(&error),
                    "outbox relay pass failed; retrying"
                ),
            }
            let delay = jitter(opts.idle_delay, Uuid::new_v4());
            tokio::select! {
                _ = &mut shutdown => break,
                _ = tokio::time::sleep(delay) => {},
            }
        }
    }
}

fn validate_opts(opts: &OutboxRelayOpts) -> Result<()> {
    if opts.batch_size == 0 || opts.batch_size > 100 {
        return Err(ForgeError::invalid("outbox batch_size must be in 1..=100"));
    }
    if opts.claim_for < Duration::from_secs(1) || opts.claim_for > Duration::from_secs(300) {
        return Err(ForgeError::invalid(
            "outbox claim_for must be between 1 and 300 seconds",
        ));
    }
    if opts.failure_backoff > Duration::from_secs(300) || opts.idle_delay > Duration::from_secs(30)
    {
        return Err(ForgeError::invalid(
            "outbox retry delays exceed their bounds",
        ));
    }
    Ok(())
}

fn safe_error_summary(error: &ForgeError) -> &'static str {
    match error {
        ForgeError::Context { source, .. } => safe_error_summary(source),
        ForgeError::Unavailable(_) => "queue unavailable",
        ForgeError::Precondition(_) => "queue precondition failed",
        ForgeError::Limit(_) => "queue limit exceeded",
        ForgeError::Invalid(_) => "outbox row is invalid",
        ForgeError::NotConfigured(_) => "queue is not configured",
        ForgeError::NotFound => "queue entity not found",
        ForgeError::Config(_) | ForgeError::Backend { .. } => "queue backend failed",
    }
}

fn jitter(base: Duration, seed: Uuid) -> Duration {
    if base.is_zero() {
        return base;
    }
    let bytes = seed.as_u128() as u64;
    let factor = 0.8 + (bytes % 401) as f64 / 1000.0;
    Duration::from_secs_f64((base.as_secs_f64() * factor).min(30.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_bounded() {
        assert!(validate_opts(&OutboxRelayOpts::new()).is_ok());
        assert!(validate_opts(&OutboxRelayOpts::new().with_batch_size(0)).is_err());
        assert!(validate_opts(&OutboxRelayOpts::new().with_batch_size(101)).is_err());
    }

    #[test]
    fn schema_is_application_owned_and_versioned() {
        assert!(OUTBOX_SCHEMA_SQL.contains("app_forge_outbox_v1"));
        assert!(!OUTBOX_SCHEMA_SQL.contains("forge_jobs"));
    }
}

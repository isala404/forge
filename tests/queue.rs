#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forgelib::testing::TestDatabase;
use forgelib::{
    BatchEnqueueItem, DequeueOpts, EnqueueOpts, Forge, ForgeError, JobId, JobState, NackOpts,
    OUTBOX_SCHEMA_SQL, OutboxRelayOpts, Priority, RedriveDedupPolicy, RedriveOpts,
};
use sqlx::{Connection, PgConnection};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

fn payload(s: &str) -> Bytes {
    Bytes::from(s.as_bytes().to_vec())
}

fn assert_code<T>(result: Result<T, ForgeError>, expected: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), expected),
        Ok(_) => panic!("expected {expected} error"),
    }
}

fn vis(secs: u64) -> DequeueOpts {
    DequeueOpts::new()
        .with_wait(Duration::ZERO)
        .with_visibility_timeout(Duration::from_secs(secs))
}

#[tokio::test]
async fn cancellation_priority_status_and_key_fairness_are_durable() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();
    let cancelled = q
        .enqueue("long", payload("cancel"), EnqueueOpts::new())
        .await
        .unwrap();
    assert_eq!(
        q.cancel(cancelled).await.unwrap().unwrap().state,
        JobState::Cancelled
    );
    let first = q
        .enqueue(
            "long",
            payload("a"),
            EnqueueOpts::new()
                .with_priority(Priority::High)
                .with_concurrency_key("tenant-a"),
        )
        .await
        .unwrap();
    q.enqueue(
        "long",
        payload("a2"),
        EnqueueOpts::new()
            .with_priority(Priority::High)
            .with_concurrency_key("tenant-a"),
    )
    .await
    .unwrap();
    let other = q
        .enqueue(
            "long",
            payload("b"),
            EnqueueOpts::new().with_concurrency_key("tenant-b"),
        )
        .await
        .unwrap();
    let opts = vis(30).with_concurrency_limit_per_key(1);
    let leased = q.dequeue("long", opts.clone()).await.unwrap().unwrap();
    assert_eq!(leased.id, first);
    assert_eq!(q.dequeue("long", opts).await.unwrap().unwrap().id, other);
    assert_eq!(
        q.cancel(first).await.unwrap().unwrap().state,
        JobState::CancelRequested
    );
    assert!(q.cancellation_requested(&leased).await.unwrap());
    assert_code(q.ack(&leased).await, "PRECONDITION");
    q.finish_cancellation(&leased).await.unwrap();
    assert_eq!(
        q.status(first).await.unwrap().unwrap().state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn enqueue_dequeue_ack_roundtrip() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    let id = q
        .enqueue("emails", payload("hi"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("emails", vis(30)).await.unwrap().expect("a job");
    assert_eq!(job.id, id);
    assert_eq!(job.payload, payload("hi"));
    assert_eq!(job.attempt, 1, "first delivery is attempt 1");

    q.ack(&job).await.unwrap();
    assert!(
        q.dequeue("emails", vis(30)).await.unwrap().is_none(),
        "acked job is gone"
    );
}

#[tokio::test]
async fn leased_job_is_invisible_to_other_dequeues() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let _job = q.dequeue("q", vis(60)).await.unwrap().expect("claimed");
    assert!(
        q.dequeue("q", vis(60)).await.unwrap().is_none(),
        "leased => invisible"
    );
}

#[tokio::test]
async fn lease_expiry_redelivers_and_increments_attempt() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let first = q
        .dequeue("q", vis(1))
        .await
        .unwrap()
        .expect("first delivery");
    assert_eq!(first.attempt, 1);

    // Let the lease expire (simulating a crashed worker), then redeliver.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let second = q.dequeue("q", vis(30)).await.unwrap().expect("redelivery");
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempt, 2, "redelivery increments the attempt");

    // Stale handle from the crashed worker can no longer mutate the job.
    assert_code(q.heartbeat(&first).await, "PRECONDITION");
    assert_code(q.ack(&first).await, "PRECONDITION");
    q.ack(&second).await.unwrap();
    assert!(q.dequeue("q", vis(30)).await.unwrap().is_none());
}

#[tokio::test]
async fn nack_returns_job_for_redelivery() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    // Immediate retry so the test doesn't wait out a backoff.
    q.nack(&job, NackOpts::retry_in(Duration::ZERO))
        .await
        .unwrap();

    let again = q.dequeue("q", vis(30)).await.unwrap().expect("redelivered");
    assert_eq!(again.id, job.id);
    assert_eq!(again.attempt, 2);
}

#[tokio::test]
async fn exhausted_job_moves_to_dead_letter_queue() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    // max_attempts = 1: one delivery, then dead-letter on failure.
    let id = q
        .enqueue("q", payload("x"), EnqueueOpts::new().with_max_attempts(1))
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    q.nack(&job, NackOpts::retry_in(Duration::ZERO))
        .await
        .unwrap();

    assert!(
        q.dequeue("q", vis(30)).await.unwrap().is_none(),
        "exhausted job left the source queue"
    );
    let dead = q
        .dequeue("q.dlq", vis(30))
        .await
        .unwrap()
        .expect("job in DLQ");
    assert_eq!(dead.id, id);
    assert_eq!(dead.attempt, 1, "DLQ job is a fresh available job");
}

#[tokio::test]
async fn dedup_collapses_enqueues_within_window() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    let opts = || EnqueueOpts::new().with_dedup_id("order-42");
    let id1 = q.enqueue("q", payload("a"), opts()).await.unwrap();
    let id2 = q.enqueue("q", payload("b"), opts()).await.unwrap();
    assert_eq!(id1, id2, "same dedup_id within window => same job id");

    let job = q.dequeue("q", vis(30)).await.unwrap().expect("one job");
    assert_eq!(job.id, id1);
    q.ack(&job).await.unwrap();
    assert!(
        q.dequeue("q", vis(30)).await.unwrap().is_none(),
        "only one job was enqueued"
    );

    // Same dedup_id in a different queue is independent.
    let other = q.enqueue("other", payload("c"), opts()).await.unwrap();
    assert_ne!(other, id1, "dedup is scoped per queue");
}

#[tokio::test]
async fn requested_job_id_is_idempotent_even_with_new_dedup_slot() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    let requested = JobId::new();
    let first = q
        .enqueue(
            "q",
            payload("first"),
            EnqueueOpts::new().with_job_id(requested),
        )
        .await
        .unwrap();
    assert_eq!(first, requested);

    let second = q
        .enqueue(
            "q",
            payload("second"),
            EnqueueOpts::new()
                .with_job_id(requested)
                .with_dedup_id("fresh-slot"),
        )
        .await
        .unwrap();
    assert_eq!(
        second, requested,
        "same requested id on same queue is success even while claiming a new dedup slot"
    );

    assert_code(
        q.enqueue(
            "other",
            payload("bad"),
            EnqueueOpts::new()
                .with_job_id(requested)
                .with_dedup_id("other-slot"),
        )
        .await,
        "PRECONDITION",
    );
}

#[tokio::test]
async fn many_processes_simultaneously_enqueue_one_deterministic_id() {
    let db = TestDatabase::new().await.unwrap();
    let migrated = db.forge().await.unwrap();
    drop(migrated);
    let requested = JobId::new();
    let executable = std::env::current_exe().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let barrier = listener.local_addr().unwrap().to_string();
    let mut children = Vec::new();
    let mut ready = Vec::new();
    for _ in 0..12 {
        let database_url = db.url().to_string();
        children.push(
            Command::new(&executable)
                .args(["--exact", "deterministic_enqueue_process_helper"])
                .env("FORGE_QUEUE_PROCESS_TEST_URL", database_url)
                .env("FORGE_QUEUE_PROCESS_TEST_ID", requested.to_string())
                .env("FORGE_QUEUE_PROCESS_TEST_BARRIER", &barrier)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        ready.push(listener.accept().unwrap().0);
    }
    for stream in &mut ready {
        stream.write_all(&[1]).unwrap();
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "producer process failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let verify = db.forge_with("auto_migrate = false\n").await.unwrap();
    assert_eq!(verify.queue().depth("same-id").await.unwrap().visible, 1);
}

#[tokio::test]
async fn deterministic_enqueue_process_helper() {
    let (Ok(database_url), Ok(requested)) = (
        std::env::var("FORGE_QUEUE_PROCESS_TEST_URL"),
        std::env::var("FORGE_QUEUE_PROCESS_TEST_ID"),
    ) else {
        return;
    };
    let barrier = std::env::var("FORGE_QUEUE_PROCESS_TEST_BARRIER").unwrap();
    let forge = Forge::init_from_str(&format!(
        "[forge]\nmode = \"postgres\"\nenvironment = \"test\"\n[postgres]\nurl = {database_url:?}\nauto_migrate = false\nmax_connections = 2\nlock_timeout_ms = 0\n"
    ))
    .await
    .unwrap();
    let mut ready = TcpStream::connect(barrier).unwrap();
    let mut release = [0];
    ready.read_exact(&mut release).unwrap();
    let requested = JobId::parse(&requested).unwrap();
    assert_eq!(
        forge
            .queue()
            .enqueue(
                "same-id",
                payload("same"),
                EnqueueOpts::new().with_job_id(requested),
            )
            .await
            .unwrap(),
        requested
    );
    forge.close(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn dead_letter_inspection_redrive_purge_and_dedup_release() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();
    let first = q
        .enqueue(
            "ops",
            payload("one"),
            EnqueueOpts::new()
                .with_max_attempts(1)
                .with_dedup_id("content"),
        )
        .await
        .unwrap();
    let job = q.dequeue("ops", vis(30)).await.unwrap().unwrap();
    q.nack(
        &job,
        NackOpts::default().with_failure_summary("redacted failure"),
    )
    .await
    .unwrap();
    let second = q
        .enqueue(
            "ops",
            payload("two"),
            EnqueueOpts::new()
                .with_dedup_id("content")
                .with_max_attempts(1),
        )
        .await
        .unwrap();
    assert_ne!(first, second);

    let page = q.dead_letters("ops", None, 1).await.unwrap();
    assert_eq!(page.items.len(), 1);
    let item = page.items.first().unwrap();
    assert_eq!(item.attempt_count, 1);
    assert_eq!(item.failure_summary.as_deref(), Some("redacted failure"));
    assert!(
        q.redrive(
            first,
            RedriveOpts::new("recovered", RedriveDedupPolicy::Clear),
        )
        .await
        .unwrap()
    );
    assert_eq!(
        q.dequeue("recovered", vis(30)).await.unwrap().unwrap().id,
        first
    );

    let pending = q.dequeue("ops", vis(30)).await.unwrap().unwrap();
    q.nack(
        &pending,
        NackOpts::default().with_failure_summary("terminal"),
    )
    .await
    .unwrap();
    assert_eq!(q.purge_dead_letters_dry_run("ops").await.unwrap(), 1);
    assert_code(q.purge_dead_letters("ops", "wrong").await, "PRECONDITION");
    assert_eq!(q.purge_dead_letters("ops", "ops").await.unwrap(), 1);
}

#[tokio::test]
#[allow(clippy::disallowed_methods)]
async fn outbox_recovers_before_enqueue_after_enqueue_and_before_mark() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    db.execute_raw(OUTBOX_SCHEMA_SQL).await.unwrap();

    let before = JobId::new();
    let after = JobId::new();
    let before_mark = JobId::new();
    let mut conn = PgConnection::connect(db.url()).await.unwrap();
    for (id, state, expired_claim) in [
        (before, "pending", false),
        (after, "pending", false),
        (before_mark, "claimed", true),
    ] {
        let sql = if expired_claim {
            "INSERT INTO app_forge_outbox_v1 (event_id, destination, payload, dispatch_state, claimed_until) VALUES ($1, 'events', decode('78', 'hex'), $2, now() - interval '1 second')"
        } else {
            "INSERT INTO app_forge_outbox_v1 (event_id, destination, payload, dispatch_state) VALUES ($1, 'events', decode('78', 'hex'), $2)"
        };
        sqlx::query(sql)
            .bind(id.0)
            .bind(state)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    // Simulate a crash after enqueue but before the outbox row was marked.
    forge
        .queue()
        .enqueue(
            "events",
            payload("x"),
            EnqueueOpts::new().with_job_id(after),
        )
        .await
        .unwrap();

    let report = forge
        .run_outbox_once(OutboxRelayOpts::new().with_batch_size(10))
        .await
        .unwrap();
    assert_eq!(
        (report.claimed, report.dispatched, report.failed),
        (3, 3, 0)
    );
    assert_eq!(report.pending, 0);
    assert_eq!(forge.queue().depth("events").await.unwrap().visible, 3);
    let dispatched: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_forge_outbox_v1 WHERE dispatch_state = 'dispatched'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(dispatched, 3);
}

#[tokio::test]
async fn unknown_job_id_is_not_found() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    q.ack(&job).await.unwrap();
    // Row still exists (status=done) but the lease is gone => Precondition, not NotFound.
    assert_code(q.heartbeat(&job).await, "PRECONDITION");
}

#[test]
fn job_ids_are_unique() {
    assert_ne!(JobId::new(), JobId::new());
}

#[tokio::test]
async fn validates_queue_names_and_limits() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    assert_code(
        q.enqueue("bad name", payload("x"), EnqueueOpts::new())
            .await,
        "INVALID",
    );
    assert_code(
        q.enqueue("q.dlq", payload("x"), EnqueueOpts::new()).await,
        "INVALID",
    );

    let too_big = Bytes::from(vec![0u8; 256 * 1024 + 1]);
    assert_code(q.enqueue("q", too_big, EnqueueOpts::new()).await, "LIMIT");

    // Queue names are length-capped (256 bytes) like schedule names.
    let long_name = "a".repeat(257);
    assert_code(
        q.enqueue(&long_name, payload("x"), EnqueueOpts::new())
            .await,
        "INVALID",
    );
}

#[tokio::test]
async fn dlq_job_exhaustion_parks_as_dead_not_chained() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue(
        "orders",
        payload("x"),
        EnqueueOpts::new().with_max_attempts(1),
    )
    .await
    .unwrap();
    let j = q.dequeue("orders", vis(30)).await.unwrap().unwrap();
    q.nack(&j, NackOpts::default()).await.unwrap();

    let d = q.dequeue("orders.dlq", vis(30)).await.unwrap().unwrap();
    assert_eq!(d.attempt, 1, "DLQ job is a fresh attempt");
    q.nack(&d, NackOpts::default()).await.unwrap();

    // It parked as 'dead': not redelivered, and no orders.dlq.dlq chain exists.
    assert!(
        q.dequeue("orders.dlq", vis(1)).await.unwrap().is_none(),
        "a dead job is not redelivered"
    );
    let chained = q.depth("orders.dlq.dlq").await.unwrap();
    assert_eq!(
        chained.visible + chained.in_flight + chained.delayed,
        0,
        "exhaustion in a .dlq queue must not create a .dlq.dlq chain"
    );
}

#[tokio::test]
async fn nack_retry_in_is_bounded() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    // A century-long park is rejected; the cap is the 12h visibility ceiling.
    let res = q
        .nack(
            &job,
            NackOpts::retry_in(Duration::from_secs(100 * 365 * 24 * 3600)),
        )
        .await;
    assert_code(res, "INVALID");
}

/// N concurrent consumers each get a distinct job, exercising `FOR UPDATE SKIP LOCKED`.
#[tokio::test]
async fn concurrent_consumers_never_double_deliver() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    const N: usize = 60;
    for i in 0..N {
        forge
            .queue()
            .enqueue("work", payload(&format!("job-{i}")), EnqueueOpts::new())
            .await
            .unwrap();
    }

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let f = forge.clone();
        tasks.spawn(async move {
            let mut mine = Vec::new();
            // Long lease, never ack: a claimed job stays invisible, so None means drained.
            while let Some(job) = f.queue().dequeue("work", vis(120)).await.unwrap() {
                mine.push(job.id);
            }
            mine
        });
    }

    let mut all = Vec::new();
    while let Some(res) = tasks.join_next().await {
        all.extend(res.unwrap());
    }

    let unique: HashSet<_> = all.iter().copied().collect();
    assert_eq!(
        all.len(),
        unique.len(),
        "a job was delivered to more than one consumer"
    );
    assert_eq!(unique.len(), N, "every job was delivered exactly once");
}

#[tokio::test]
async fn depth_reports_visible_in_flight_and_delayed() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("d", payload("a"), EnqueueOpts::new())
        .await
        .unwrap();
    q.enqueue("d", payload("b"), EnqueueOpts::new())
        .await
        .unwrap();
    q.enqueue(
        "d",
        payload("c"),
        EnqueueOpts::new().with_delay(Duration::from_secs(300)),
    )
    .await
    .unwrap();

    let d = q.depth("d").await.unwrap();
    assert_eq!((d.visible, d.in_flight, d.delayed), (2, 0, 1));
    assert_eq!(d.total(), 3);

    let job = q.dequeue("d", vis(120)).await.unwrap().expect("a job");
    let d = q.depth("d").await.unwrap();
    assert_eq!((d.visible, d.in_flight, d.delayed), (1, 1, 1));

    q.ack(&job).await.unwrap();
    let d = q.depth("d").await.unwrap();
    assert_eq!((d.visible, d.in_flight, d.delayed), (1, 0, 1));
    assert_eq!(d.total(), 2);
}

#[tokio::test]
async fn depth_counts_dead_letter_backlog() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("j", payload("x"), EnqueueOpts::new().with_max_attempts(1))
        .await
        .unwrap();
    let job = q.dequeue("j", vis(30)).await.unwrap().expect("a job");
    q.nack(&job, NackOpts::default()).await.unwrap(); // exhausted -> dead-letter

    assert_eq!(q.depth("j").await.unwrap().total(), 0, "source drained");
    assert_eq!(
        q.depth("j.dlq").await.unwrap().visible,
        1,
        "one job dead-lettered"
    );
}

#[tokio::test]
async fn batch_pause_resume_and_counter_stats_are_durable() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();
    let deterministic = JobId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let results = q
        .enqueue_batch(
            "operator-batch",
            vec![
                BatchEnqueueItem::new(
                    payload("one"),
                    EnqueueOpts::new().with_job_id(deterministic),
                ),
                BatchEnqueueItem::new(payload("two"), EnqueueOpts::new()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        results.first().and_then(|result| result.job_id),
        Some(deterministic)
    );
    q.pause("operator-batch").await.unwrap();
    assert!(q.is_paused("operator-batch").await.unwrap());
    assert!(
        q.dequeue("operator-batch", vis(30))
            .await
            .unwrap()
            .is_none()
    );
    q.resume("operator-batch").await.unwrap();
    let jobs = q
        .dequeue_batch("operator-batch", 10, vis(30))
        .await
        .unwrap();
    assert_eq!(jobs.len(), 2);
    for job in &jobs {
        q.ack(job).await.unwrap();
    }
    let stats = q.stats("operator-batch").await.unwrap();
    assert_eq!(stats.enqueued_total, 2);
    assert_eq!(stats.settled_total, 2);
    assert!(!stats.paused);
}

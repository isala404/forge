package forge

import (
	"context"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

func (f *Forge) pgEnqueue(ctx context.Context, queue string, payload []byte, options EnqueueOptions) (string, error) {
	tx, err := f.postgres(PrimitiveQueue).Begin(ctx)
	if err != nil {
		return "", postgresError("queue.enqueue", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	physicalQueue := f.pgScoped(queue)
	if options.DedupID != "" {
		if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock(hashtextextended($1, hashtextextended($2, 0)))", physicalQueue, options.DedupID); err != nil {
			return "", postgresError("queue.enqueue", err)
		}
		var existing string
		err := tx.QueryRow(ctx, "SELECT job_id::text FROM forge_job_dedup WHERE queue = $1 AND dedup_id = $2 AND expires_at > now()", physicalQueue, options.DedupID).Scan(&existing)
		if err == nil {
			if options.ID != "" && options.ID != existing {
				return "", forgeError(CodePrecondition, "queue.enqueue", "deduplication ID is reserved by a different job ID")
			}
			return existing, nil
		}
		if err != pgx.ErrNoRows {
			return "", postgresError("queue.enqueue", err)
		}
		if _, err := tx.Exec(ctx, "DELETE FROM forge_job_dedup WHERE queue = $1 AND dedup_id = $2", physicalQueue, options.DedupID); err != nil {
			return "", postgresError("queue.enqueue", err)
		}
	}
	id := options.ID
	if id == "" {
		id, err = randomID(f.random, "")
		if err != nil {
			return "", err
		}
	}
	var effective string
	traceparent, tracestate, baggage := tracePointers(options.TraceContext)
	err = tx.QueryRow(ctx, "INSERT INTO forge_jobs (id, queue, payload, status, attempts, max_attempts, available_at, traceparent, tracestate, baggage, priority, concurrency_key) VALUES ($1::uuid, $2, $3, 'available', 0, $4, now() + $5 * interval '1 second', $6, $7, $8, $9, NULLIF($10,'')) ON CONFLICT (id) DO NOTHING RETURNING id::text", id, physicalQueue, payload, int32(options.MaxAttempts), options.Delay.Seconds(), traceparent, tracestate, baggage, priorityRank(options.Priority), options.ConcurrencyKey).Scan(&effective)
	created := err == nil
	if err == pgx.ErrNoRows {
		var existingQueue string
		if queryErr := tx.QueryRow(ctx, "SELECT queue FROM forge_jobs WHERE id = $1::uuid", id).Scan(&existingQueue); queryErr != nil {
			return "", postgresError("queue.enqueue", queryErr)
		}
		if existingQueue != physicalQueue {
			return "", forgeError(CodePrecondition, "queue.enqueue", "job ID already belongs to another queue or namespace")
		}
		effective = id
	} else if err != nil {
		return "", postgresError("queue.enqueue", err)
	}
	if options.DedupID != "" {
		if _, err := tx.Exec(ctx, "INSERT INTO forge_job_dedup (queue, dedup_id, job_id, expires_at) VALUES ($1, $2, $3::uuid, now() + interval '5 minutes')", physicalQueue, options.DedupID, effective); err != nil {
			return "", postgresError("queue.enqueue", err)
		}
	}
	if created {
		if _, err := tx.Exec(ctx, "INSERT INTO forge_queue_counters(queue,enqueued_total) VALUES($1,1) ON CONFLICT(queue) DO UPDATE SET enqueued_total=forge_queue_counters.enqueued_total+1", physicalQueue); err != nil {
			return "", postgresError("queue.enqueue", err)
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return "", postgresError("queue.enqueue", err)
	}
	return effective, nil
}

func (f *Forge) pgDequeue(ctx context.Context, queue string, options DequeueOptions) (*Job, error) {
	deadline := time.Now().Add(options.Wait)
	physicalQueue := f.pgScoped(queue)
	for {
		if err := f.pgReclaimQueue(ctx, physicalQueue); err != nil {
			return nil, err
		}
		var job Job
		var payload []byte
		var attempt, maxAttempts int32
		var leasedUntil time.Time
		err := f.postgres(PrimitiveQueue).QueryRow(ctx, `WITH candidate AS (
SELECT id FROM forge_jobs j WHERE queue = $1 AND status = 'available' AND available_at <= now()
AND NOT EXISTS (SELECT 1 FROM forge_queue_controls c WHERE c.queue=$1 AND c.paused)
AND ($3::bigint = 0 OR concurrency_key IS NULL OR (SELECT count(*) FROM forge_jobs l WHERE l.queue=j.queue AND l.status='leased' AND l.leased_until>now() AND l.cancel_requested_at IS NULL AND l.concurrency_key=j.concurrency_key) < $3)
ORDER BY priority DESC, available_at, enqueued_at, id FOR UPDATE SKIP LOCKED LIMIT 1
), claimed AS (
UPDATE forge_jobs j SET status = 'leased', lease_token = gen_random_uuid(), leased_until = now() + $2 * interval '1 second', lease_secs = $2 FROM candidate WHERE j.id = candidate.id
RETURNING j.id::text, j.lease_token::text, j.payload, j.attempts + 1 AS attempt, j.max_attempts, j.leased_until, j.traceparent, j.tracestate, j.baggage
) SELECT id, lease_token, payload, attempt, max_attempts, leased_until, traceparent, tracestate, baggage FROM claimed`, physicalQueue, options.Visibility.Seconds(), options.ConcurrencyLimitPerKey).Scan(&job.ID, &job.Receipt, &payload, &attempt, &maxAttempts, &leasedUntil, &job.Traceparent, &job.Tracestate, &job.Baggage)
		if err == nil {
			job.Queue = queue
			job.Payload = append([]byte(nil), payload...)
			job.Attempt = uint32(attempt)
			job.MaxAttempts = uint32(maxAttempts)
			job.LeasedUntilMs = float64(leasedUntil.UnixMilli())
			return &job, nil
		}
		if err != pgx.ErrNoRows {
			return nil, postgresError("queue.dequeue", err)
		}
		if options.Wait == 0 || !time.Now().Before(deadline) {
			return nil, nil
		}
		wait := 25 * time.Millisecond
		if remaining := time.Until(deadline); remaining < wait {
			wait = remaining
		}
		if !waitContext(ctx, wait) {
			return nil, errorWithCause(CodeUnavailable, "queue.dequeue", "postgres", "dequeue was cancelled", ctx.Err())
		}
	}
}

func (f *Forge) pgReclaimQueue(ctx context.Context, queue string) error {
	if _, err := f.postgres(PrimitiveQueue).Exec(ctx, "UPDATE forge_jobs SET status='cancelled',completed_at=now(),lease_token=NULL,leased_until=NULL,lease_secs=NULL WHERE queue=$1 AND status='leased' AND cancel_requested_at IS NOT NULL AND leased_until<=now()", queue); err != nil {
		return postgresError("queue.reclaim", err)
	}
	if _, err := f.postgres(PrimitiveQueue).Exec(ctx, "UPDATE forge_jobs SET status = 'available', attempts = attempts + 1, available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL WHERE queue = $1 AND status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now() AND attempts + 1 < max_attempts", queue); err != nil {
		return postgresError("queue.reclaim", err)
	}
	if _, err := f.postgres(PrimitiveQueue).Exec(ctx, `WITH moved AS (
UPDATE forge_jobs SET queue = CASE WHEN right(queue, 4) = '.dlq' THEN queue ELSE queue || '.dlq' END,
status = CASE WHEN right(queue, 4) = '.dlq' THEN 'dead' ELSE 'available' END,
dead_attempts = dead_attempts + attempts + 1, attempts = CASE WHEN right(queue, 4) = '.dlq' THEN attempts + 1 ELSE 0 END,
failure_summary = 'visibility timeout expired', dead_lettered_at = COALESCE(dead_lettered_at, now()),
completed_at = CASE WHEN right(queue, 4) = '.dlq' THEN now() ELSE completed_at END,
available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL
WHERE queue = $1 AND status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now() AND attempts + 1 >= max_attempts RETURNING id)
DELETE FROM forge_job_dedup d USING moved WHERE d.job_id = moved.id`, queue); err != nil {
		return postgresError("queue.reclaim", err)
	}
	return nil
}

func (f *Forge) pgAck(ctx context.Context, receipt string) error {
	if !looksLikeUUID(receipt) {
		return forgeError(CodePrecondition, "queue.ack", "receipt is unknown or its lease was lost")
	}
	var queue string
	err := f.postgres(PrimitiveQueue).QueryRow(ctx, "UPDATE forge_jobs SET status = 'done', completed_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL WHERE lease_token = $1::uuid AND status = 'leased' AND cancel_requested_at IS NULL AND left(queue, length($2)) = $2 RETURNING queue", receipt, f.pgNamespacePrefix()).Scan(&queue)
	if err == pgx.ErrNoRows {
		return forgeError(CodePrecondition, "queue.ack", "receipt is unknown or its lease was lost")
	}
	if err != nil {
		return postgresError("queue.ack", err)
	}
	if _, err := f.postgres(PrimitiveQueue).Exec(ctx, "INSERT INTO forge_queue_counters(queue,settled_total) VALUES($1,1) ON CONFLICT(queue) DO UPDATE SET settled_total=forge_queue_counters.settled_total+1", queue); err != nil {
		return postgresError("queue.ack", err)
	}
	return nil
}

func (f *Forge) pgNack(ctx context.Context, receipt string, options NackOptions) error {
	if !looksLikeUUID(receipt) {
		return forgeError(CodePrecondition, "queue.nack", "receipt is unknown or its lease was lost")
	}
	tx, err := f.postgres(PrimitiveQueue).Begin(ctx)
	if err != nil {
		return postgresError("queue.nack", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	var id, queue string
	var attempts, maxAttempts int32
	err = tx.QueryRow(ctx, "SELECT id::text, queue, attempts, max_attempts FROM forge_jobs WHERE lease_token = $1::uuid AND status = 'leased' AND cancel_requested_at IS NULL AND left(queue, length($2)) = $2 FOR UPDATE", receipt, f.pgNamespacePrefix()).Scan(&id, &queue, &attempts, &maxAttempts)
	if err == pgx.ErrNoRows {
		return forgeError(CodePrecondition, "queue.nack", "receipt is unknown or its lease was lost")
	}
	if err != nil {
		return postgresError("queue.nack", err)
	}
	nextAttempts := attempts + 1
	failureSummary := safeFailureSummary(options.FailureSummary, "handler failed")
	if nextAttempts >= maxAttempts {
		if len(queue) >= 4 && queue[len(queue)-4:] == ".dlq" {
			_, err = tx.Exec(ctx, "UPDATE forge_jobs SET status = 'dead', attempts = $2, dead_attempts = dead_attempts + $2, failure_summary = $3, dead_lettered_at = COALESCE(dead_lettered_at, now()), completed_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL WHERE id = $1::uuid", id, nextAttempts, failureSummary)
		} else {
			_, err = tx.Exec(ctx, "UPDATE forge_jobs SET queue = queue || '.dlq', status = 'available', attempts = 0, dead_attempts = $2, failure_summary = $3, dead_lettered_at = now(), available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL WHERE id = $1::uuid", id, nextAttempts, failureSummary)
		}
		if err == nil {
			_, err = tx.Exec(ctx, "DELETE FROM forge_job_dedup WHERE job_id = $1::uuid", id)
		}
		if err == nil {
			_, err = tx.Exec(ctx, "INSERT INTO forge_queue_counters(queue,settled_total,dead_total) VALUES($1,1,1) ON CONFLICT(queue) DO UPDATE SET settled_total=forge_queue_counters.settled_total+1,dead_total=forge_queue_counters.dead_total+1", queue)
		}
	} else {
		_, err = tx.Exec(ctx, "UPDATE forge_jobs SET status = 'available', attempts = $2, failure_summary = $4, available_at = now() + $3 * interval '1 second', lease_token = NULL, leased_until = NULL, lease_secs = NULL WHERE id = $1::uuid", id, nextAttempts, options.RetryIn.Seconds(), failureSummary)
	}
	if err != nil {
		return postgresError("queue.nack", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return postgresError("queue.nack", err)
	}
	return nil
}

func (f *Forge) pgHeartbeat(ctx context.Context, receipt string, visibility time.Duration) error {
	if !looksLikeUUID(receipt) {
		return forgeError(CodePrecondition, "queue.heartbeat", "receipt is unknown or its lease was lost")
	}
	result, err := f.postgres(PrimitiveQueue).Exec(ctx, "UPDATE forge_jobs SET leased_until = now() + $2 * interval '1 second', lease_secs = $2 WHERE lease_token = $1::uuid AND status = 'leased' AND cancel_requested_at IS NULL AND leased_until > now() AND left(queue, length($3)) = $3", receipt, visibility.Seconds(), f.namespace+":")
	if err != nil {
		return postgresError("queue.heartbeat", err)
	}
	if result.RowsAffected() == 0 {
		return forgeError(CodePrecondition, "queue.heartbeat", "receipt is unknown or its lease was lost")
	}
	return nil
}

func (f *Forge) pgCancelJob(ctx context.Context, jobID string) (*JobStatus, error) {
	tx, err := f.postgres(PrimitiveQueue).Begin(ctx)
	if err != nil {
		return nil, postgresError("queue.cancel", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	row := tx.QueryRow(ctx, `UPDATE forge_jobs SET status=CASE WHEN status='available' THEN 'cancelled' ELSE status END,cancel_requested_at=CASE WHEN status='leased' THEN COALESCE(cancel_requested_at,now()) ELSE cancel_requested_at END,completed_at=CASE WHEN status='available' THEN now() ELSE completed_at END,lease_token=CASE WHEN status='available' THEN NULL ELSE lease_token END,leased_until=CASE WHEN status='available' THEN NULL ELSE leased_until END,lease_secs=CASE WHEN status='available' THEN NULL ELSE lease_secs END WHERE id=$1::uuid AND left(queue,length($2))=$2 RETURNING id::text,queue,status,attempts,max_attempts,priority,concurrency_key,enqueued_at,available_at,completed_at,cancel_requested_at`, jobID, f.pgNamespacePrefix())
	status, err := f.scanPGJobStatus(row)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("queue.cancel", err)
	}
	if _, err = tx.Exec(ctx, "DELETE FROM forge_job_dedup WHERE job_id=$1::uuid", jobID); err != nil {
		return nil, postgresError("queue.cancel", err)
	}
	if err = tx.Commit(ctx); err != nil {
		return nil, postgresError("queue.cancel", err)
	}
	return &status, nil
}

func (f *Forge) pgCancellationRequested(ctx context.Context, receipt string) (bool, error) {
	if !looksLikeUUID(receipt) {
		return false, forgeError(CodePrecondition, "queue.cancellation_requested", "receipt is unknown or its lease was lost")
	}
	var requested bool
	err := f.postgres(PrimitiveQueue).QueryRow(ctx, "SELECT cancel_requested_at IS NOT NULL OR status='cancelled' FROM forge_jobs WHERE lease_token=$1::uuid AND left(queue,length($2))=$2", receipt, f.pgNamespacePrefix()).Scan(&requested)
	if err == pgx.ErrNoRows {
		return false, forgeError(CodePrecondition, "queue.cancellation_requested", "receipt is unknown or its lease was lost")
	}
	if err != nil {
		return false, postgresError("queue.cancellation_requested", err)
	}
	return requested, nil
}

func (f *Forge) pgFinishCancellation(ctx context.Context, receipt string) error {
	if !looksLikeUUID(receipt) {
		return forgeError(CodePrecondition, "queue.finish_cancellation", "cancellation fence was lost")
	}
	result, err := f.postgres(PrimitiveQueue).Exec(ctx, "UPDATE forge_jobs SET status='cancelled',completed_at=now(),lease_token=NULL,leased_until=NULL,lease_secs=NULL WHERE lease_token=$1::uuid AND status='leased' AND cancel_requested_at IS NOT NULL AND left(queue,length($2))=$2", receipt, f.pgNamespacePrefix())
	if err != nil {
		return postgresError("queue.finish_cancellation", err)
	}
	if result.RowsAffected() == 0 {
		return forgeError(CodePrecondition, "queue.finish_cancellation", "cancellation fence was lost")
	}
	return nil
}

func (f *Forge) pgJobStatus(ctx context.Context, jobID string) (*JobStatus, error) {
	row := f.postgres(PrimitiveQueue).QueryRow(ctx, "SELECT id::text,queue,status,attempts,max_attempts,priority,concurrency_key,enqueued_at,available_at,completed_at,cancel_requested_at FROM forge_jobs WHERE id=$1::uuid AND left(queue,length($2))=$2", jobID, f.pgNamespacePrefix())
	status, err := f.scanPGJobStatus(row)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("queue.status", err)
	}
	return &status, nil
}

func (f *Forge) pgListJobStatus(ctx context.Context, filter JobStatusFilter) (JobStatusPage, error) {
	states := make([]string, len(filter.States))
	for i, state := range filter.States {
		states[i] = string(state)
	}
	var queue *string
	if filter.Queue != "" {
		value := f.pgScoped(filter.Queue)
		queue = &value
	}
	var cursor *string
	if filter.Cursor != "" {
		if !looksLikeUUID(filter.Cursor) {
			return JobStatusPage{}, forgeError(CodeInvalid, "queue.list_status", "cursor must be a job ID")
		}
		cursor = &filter.Cursor
	}
	rows, err := f.postgres(PrimitiveQueue).Query(ctx, `SELECT id::text,queue,status,attempts,max_attempts,priority,concurrency_key,enqueued_at,available_at,completed_at,cancel_requested_at FROM forge_jobs j WHERE ($1::text IS NULL OR queue=$1) AND left(queue,length($2))=$2 AND ($3::uuid IS NULL OR (enqueued_at,id)>(SELECT enqueued_at,id FROM forge_jobs WHERE id=$3::uuid)) AND (cardinality($4::text[])=0 OR CASE WHEN status='available' AND attempts>0 THEN 'retrying' WHEN status='available' AND available_at>now() THEN 'delayed' WHEN status='available' THEN 'queued' WHEN status='leased' AND cancel_requested_at IS NOT NULL THEN 'cancel_requested' WHEN status='leased' THEN 'leased' WHEN status='done' THEN 'succeeded' ELSE status END=ANY($4)) ORDER BY enqueued_at,id LIMIT $5`, queue, f.pgNamespacePrefix(), cursor, states, int64(filter.Limit)+1)
	if err != nil {
		return JobStatusPage{}, postgresError("queue.list_status", err)
	}
	defer rows.Close()
	items := []JobStatus{}
	for rows.Next() {
		status, scanErr := f.scanPGJobStatus(rows)
		if scanErr != nil {
			return JobStatusPage{}, postgresError("queue.list_status", scanErr)
		}
		items = append(items, status)
	}
	if err = rows.Err(); err != nil {
		return JobStatusPage{}, postgresError("queue.list_status", err)
	}
	page := JobStatusPage{Items: items}
	if len(items) > int(filter.Limit) {
		page.Items = items[:filter.Limit]
		value := page.Items[len(page.Items)-1].ID
		page.Cursor = &value
	}
	return page, nil
}

type pgStatusScanner interface{ Scan(...any) error }

func (f *Forge) scanPGJobStatus(row pgStatusScanner) (JobStatus, error) {
	var status JobStatus
	var stored string
	var priority int16
	var key *string
	var completed, cancelRequested *time.Time
	err := row.Scan(&status.ID, &status.Queue, &stored, &status.AttemptCount, &status.MaxAttempts, &priority, &key, &status.EnqueuedAt, &status.AvailableAt, &completed, &cancelRequested)
	if err != nil {
		return JobStatus{}, err
	}
	status.Queue = strings.TrimPrefix(status.Queue, f.pgNamespacePrefix())
	status.Priority = PriorityNormal
	if priority == 0 {
		status.Priority = PriorityLow
	} else if priority == 2 {
		status.Priority = PriorityHigh
	}
	status.ConcurrencyKey = key
	status.CompletedAt = completed
	switch {
	case stored == "available" && status.AttemptCount > 0:
		status.State = JobRetrying
	case stored == "available" && time.Now().Before(status.AvailableAt):
		status.State = JobDelayed
	case stored == "available":
		status.State = JobQueued
	case stored == "leased" && cancelRequested != nil:
		status.State = JobCancelRequested
	case stored == "leased":
		status.State = JobLeased
	case stored == "done":
		status.State = JobSucceeded
	case stored == "dead":
		status.State = JobDead
	case stored == "cancelled":
		status.State = JobCancelled
	}
	return status, nil
}

func looksLikeUUID(value string) bool {
	if len(value) != 36 {
		return false
	}
	for index, character := range value {
		if index == 8 || index == 13 || index == 18 || index == 23 {
			if character != '-' {
				return false
			}
			continue
		}
		if !(character >= '0' && character <= '9' || character >= 'a' && character <= 'f' || character >= 'A' && character <= 'F') {
			return false
		}
	}
	return true
}

func (f *Forge) pgDepth(ctx context.Context, queue string) (QueueDepth, error) {
	var depth QueueDepth
	var oldest *float64
	err := f.postgres(PrimitiveQueue).QueryRow(ctx, `SELECT
count(*) FILTER (WHERE status = 'available' AND available_at <= now() OR status = 'leased' AND leased_until <= now()),
count(*) FILTER (WHERE status = 'leased' AND leased_until > now()),
count(*) FILTER (WHERE status = 'available' AND available_at > now()),
EXTRACT(EPOCH FROM (now() - min(enqueued_at) FILTER (WHERE status = 'available' AND available_at <= now() OR status = 'leased' AND leased_until <= now()))) * 1000
FROM forge_jobs WHERE queue = $1`, f.pgScoped(queue)).Scan(&depth.Visible, &depth.InFlight, &depth.Delayed, &oldest)
	if err != nil {
		return QueueDepth{}, postgresError("queue.depth", err)
	}
	depth.OldestVisibleAgeMs = oldest
	return depth, nil
}

func (f *Forge) pgPauseQueue(ctx context.Context, queue string, paused bool) error {
	_, err := f.postgres(PrimitiveQueue).Exec(ctx, "INSERT INTO forge_queue_controls(queue,paused) VALUES($1,$2) ON CONFLICT(queue) DO UPDATE SET paused=$2,updated_at=now()", f.pgScoped(queue), paused)
	if err != nil {
		return postgresError("queue.pause", err)
	}
	return nil
}

func (f *Forge) pgQueuePaused(ctx context.Context, queue string) (bool, error) {
	var paused bool
	err := f.postgres(PrimitiveQueue).QueryRow(ctx, "SELECT COALESCE((SELECT paused FROM forge_queue_controls WHERE queue=$1),false)", f.pgScoped(queue)).Scan(&paused)
	if err != nil {
		return false, postgresError("queue.is_paused", err)
	}
	return paused, nil
}

func (f *Forge) pgQueueStats(ctx context.Context, queue string) (QueueStats, error) {
	var stats QueueStats
	var elapsed *float64
	err := f.postgres(PrimitiveQueue).QueryRow(ctx, `SELECT COALESCE(c.enqueued_total,0),COALESCE(c.settled_total,0),COALESCE(c.dead_total,0),COALESCE(c.cancelled_total,0),
EXTRACT(EPOCH FROM(now()-c.started_at))::double precision,COALESCE(control.paused,false),
(SELECT (EXTRACT(EPOCH FROM(now()-j.enqueued_at))*1000)::double precision FROM forge_jobs j WHERE j.queue=$1 AND j.status='available' AND j.available_at<=now() ORDER BY j.enqueued_at LIMIT 1)
FROM (SELECT $1::text queue) requested LEFT JOIN forge_queue_counters c ON c.queue=requested.queue LEFT JOIN forge_queue_controls control ON control.queue=requested.queue`, f.pgScoped(queue)).Scan(&stats.EnqueuedTotal, &stats.SettledTotal, &stats.DeadTotal, &stats.CancelledTotal, &elapsed, &stats.Paused, &stats.OldestVisibleAgeMs)
	if err != nil {
		return QueueStats{}, postgresError("queue.stats", err)
	}
	minutes := 1.0 / 60.0
	if elapsed != nil && *elapsed > 1 {
		minutes = *elapsed / 60
	}
	stats.EnqueueRatePerMinute = float64(stats.EnqueuedTotal) / minutes
	stats.SettleRatePerMinute = float64(stats.SettledTotal) / minutes
	return stats, nil
}

func (f *Forge) pgDeadLetters(ctx context.Context, queue string, cursor *string, limit uint32) (DeadLetterPage, error) {
	rows, err := f.postgres(PrimitiveQueue).Query(ctx, `SELECT id::text, dead_attempts, enqueued_at, COALESCE(dead_lettered_at, enqueued_at), failure_summary
FROM forge_jobs WHERE queue = $1 AND status IN ('available', 'dead') AND ($2::uuid IS NULL OR id > $2::uuid) ORDER BY id LIMIT $3`, f.pgScoped(queue+".dlq"), cursor, int64(limit)+1)
	if err != nil {
		return DeadLetterPage{}, postgresError("queue.dead_letters", err)
	}
	defer rows.Close()
	page := DeadLetterPage{Items: make([]DeadLetterInfo, 0, limit)}
	for rows.Next() {
		var item DeadLetterInfo
		var enqueued, dead time.Time
		if err := rows.Scan(&item.JobID, &item.AttemptCount, &enqueued, &dead, &item.FailureSummary); err != nil {
			return DeadLetterPage{}, postgresError("queue.dead_letters", err)
		}
		if len(page.Items) == int(limit) {
			value := page.Items[len(page.Items)-1].JobID
			page.Cursor = &value
			break
		}
		item.Queue = queue
		item.EnqueuedAtMs = float64(enqueued.UnixMilli())
		item.DeadLetteredAtMs = float64(dead.UnixMilli())
		page.Items = append(page.Items, item)
	}
	if err := rows.Err(); err != nil {
		return DeadLetterPage{}, postgresError("queue.dead_letters", err)
	}
	return page, nil
}

func (f *Forge) pgRedrive(ctx context.Context, jobID string, options RedriveOptions) (bool, error) {
	tx, err := f.postgres(PrimitiveQueue).Begin(ctx)
	if err != nil {
		return false, postgresError("queue.redrive", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	var retained bool
	err = tx.QueryRow(ctx, "SELECT payload_retained FROM forge_jobs WHERE id=$1::uuid AND queue LIKE '%.dlq' AND status IN ('available','dead') AND left(queue,length($2))=$2 FOR UPDATE", jobID, f.pgNamespacePrefix()).Scan(&retained)
	if err == pgx.ErrNoRows {
		return false, nil
	}
	if err != nil {
		return false, postgresError("queue.redrive", err)
	}
	if !retained {
		return false, forgeError(CodePrecondition, "queue.redrive", "dead-letter payload retention elapsed; the job cannot be redriven")
	}
	result, err := tx.Exec(ctx, `UPDATE forge_jobs SET queue = $2, status = 'available', attempts = 0, available_at = now(), completed_at = NULL, dead_attempts = 0, dead_lettered_at = NULL, failure_summary = NULL, lease_token = NULL, leased_until = NULL, lease_secs = NULL, payload_retained = true WHERE id = $1::uuid AND queue LIKE '%.dlq' AND status IN ('available', 'dead') AND left(queue, length($3)) = $3`, jobID, f.pgScoped(options.Destination), f.pgNamespacePrefix())
	if err != nil {
		return false, postgresError("queue.redrive", err)
	}
	if result.RowsAffected() == 0 {
		return false, nil
	}
	if options.DedupPolicy == "clear" {
		if _, err := tx.Exec(ctx, "DELETE FROM forge_job_dedup WHERE job_id = $1::uuid", jobID); err != nil {
			return false, postgresError("queue.redrive", err)
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return false, postgresError("queue.redrive", err)
	}
	return true, nil
}

func (f *Forge) pgPurgeDeadLettersDryRun(ctx context.Context, queue string) (uint64, error) {
	var count int64
	err := f.postgres(PrimitiveQueue).QueryRow(ctx, "SELECT count(*) FROM forge_jobs WHERE queue = $1 AND status IN ('available', 'dead')", f.pgScoped(queue+".dlq")).Scan(&count)
	if err != nil {
		return 0, postgresError("queue.purge_dead_letters_dry_run", err)
	}
	return uint64(count), nil
}

func (f *Forge) pgPurgeDeadLetters(ctx context.Context, queue string) (uint64, error) {
	result, err := f.postgres(PrimitiveQueue).Exec(ctx, "DELETE FROM forge_jobs WHERE queue = $1 AND status IN ('available', 'dead')", f.pgScoped(queue+".dlq"))
	if err != nil {
		return 0, postgresError("queue.purge_dead_letters", err)
	}
	return uint64(result.RowsAffected()), nil
}

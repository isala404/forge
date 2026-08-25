package forge

import (
	"context"
	"time"

	"github.com/jackc/pgx/v5"
)

const OutboxTable = "app_forge_outbox_v1"

type OutboxRelayOptions struct {
	BatchSize        uint32
	ClaimFor         time.Duration
	FailureBackoff   time.Duration
	IdleDelay        time.Duration
	BaggageAllowlist []string
}

type claimedOutboxRow struct {
	id          string
	destination string
	payload     []byte
	delay       float64
	maxAttempts int32
	dedupID     *string
	traceparent *string
	tracestate  *string
	baggage     *string
}

func defaultOutboxOptions(options OutboxRelayOptions) (OutboxRelayOptions, error) {
	if options.BatchSize == 0 {
		options.BatchSize = 50
	}
	if options.ClaimFor == 0 {
		options.ClaimFor = 30 * time.Second
	}
	if options.FailureBackoff == 0 {
		options.FailureBackoff = time.Second
	}
	if options.IdleDelay == 0 {
		options.IdleDelay = 500 * time.Millisecond
	}
	if options.BatchSize > 100 || options.ClaimFor < time.Second || options.ClaimFor > 5*time.Minute || options.FailureBackoff > 5*time.Minute || options.IdleDelay > 30*time.Second {
		return options, forgeError(CodeInvalid, "queue.outbox_once", "outbox relay options exceed their bounds")
	}
	return options, nil
}

func (f *Forge) RunOutboxOnce(ctx context.Context, options OutboxRelayOptions) (OutboxRelayReport, error) {
	if err := f.ready(ctx, "queue.outbox_once"); err != nil {
		return OutboxRelayReport{}, err
	}
	if f.mode != ModePostgres || f.pg == nil {
		return OutboxRelayReport{}, forgeError(CodeNotConfigured, "queue.outbox_once", "transactional outbox requires PostgreSQL")
	}
	options, err := defaultOutboxOptions(options)
	if err != nil {
		return OutboxRelayReport{}, err
	}
	claim, err := randomID(f.random, "")
	if err != nil {
		return OutboxRelayReport{}, err
	}
	rows, err := f.pg.Query(ctx, `WITH candidates AS (
SELECT event_id FROM app_forge_outbox_v1 WHERE namespace = $1 AND available_at <= now()
AND (dispatch_state = 'pending' OR (dispatch_state = 'claimed' AND claimed_until <= now()))
ORDER BY available_at, created_at, event_id FOR UPDATE SKIP LOCKED LIMIT $2)
UPDATE app_forge_outbox_v1 o SET dispatch_state = 'claimed', claim_token = $3::uuid,
claimed_until = now() + $4 * interval '1 second', dispatch_attempts = dispatch_attempts + 1
FROM candidates c WHERE o.event_id = c.event_id
RETURNING o.event_id::text, o.destination, o.payload, o.delay_seconds, o.max_attempts, o.dedup_id, o.traceparent, o.tracestate, o.baggage`, f.namespace, int64(options.BatchSize), claim, options.ClaimFor.Seconds())
	if err != nil {
		return OutboxRelayReport{}, postgresError("queue.outbox_once", err)
	}
	claimed := make([]claimedOutboxRow, 0, options.BatchSize)
	for rows.Next() {
		var row claimedOutboxRow
		if err := rows.Scan(&row.id, &row.destination, &row.payload, &row.delay, &row.maxAttempts, &row.dedupID, &row.traceparent, &row.tracestate, &row.baggage); err != nil {
			rows.Close()
			return OutboxRelayReport{}, postgresError("queue.outbox_once", err)
		}
		claimed = append(claimed, row)
	}
	if err := rows.Err(); err != nil {
		rows.Close()
		return OutboxRelayReport{}, postgresError("queue.outbox_once", err)
	}
	rows.Close()
	report := OutboxRelayReport{Claimed: uint32(len(claimed))}
	for _, row := range claimed {
		dedup := ""
		if row.dedupID != nil {
			dedup = *row.dedupID
		}
		value := func(pointer *string) string {
			if pointer == nil {
				return ""
			}
			return *pointer
		}
		traceContext, traceErr := NewTraceContext(value(row.traceparent), value(row.tracestate), value(row.baggage), options.BaggageAllowlist)
		var propagation *TraceContext
		if traceErr == nil && (traceContext.Traceparent != "" || traceContext.Tracestate != "" || traceContext.Baggage != "") {
			propagation = &traceContext
		}
		enqueueErr := traceErr
		if enqueueErr == nil {
			_, enqueueErr = f.Enqueue(ctx, row.destination, row.payload, EnqueueOptions{ID: row.id, Delay: time.Duration(row.delay * float64(time.Second)), MaxAttempts: uint32(row.maxAttempts), DedupID: dedup, TraceContext: propagation})
		}
		if enqueueErr == nil {
			result, markErr := f.pg.Exec(ctx, "UPDATE app_forge_outbox_v1 SET dispatch_state = 'dispatched', dispatched_at = now(), claimed_until = NULL, claim_token = NULL, failure_summary = NULL WHERE event_id = $1::uuid AND namespace = $3 AND dispatch_state = 'claimed' AND claim_token = $2::uuid", row.id, claim, f.namespace)
			if markErr != nil {
				return report, postgresError("queue.outbox_once", markErr)
			}
			if result.RowsAffected() == 1 {
				report.Dispatched++
			}
			continue
		}
		summary := outboxErrorSummary(enqueueErr)
		if _, updateErr := f.pg.Exec(ctx, "UPDATE app_forge_outbox_v1 SET dispatch_state = 'pending', available_at = now() + $3 * interval '1 second', claimed_until = NULL, claim_token = NULL, failure_summary = $4 WHERE event_id = $1::uuid AND namespace = $5 AND dispatch_state = 'claimed' AND claim_token = $2::uuid", row.id, claim, options.FailureBackoff.Seconds(), summary, f.namespace); updateErr != nil {
			return report, postgresError("queue.outbox_once", updateErr)
		}
		report.Failed++
	}
	var pending int64
	var oldest *float64
	if err := f.pg.QueryRow(ctx, "SELECT count(*), EXTRACT(EPOCH FROM (now() - min(created_at))) * 1000 FROM app_forge_outbox_v1 WHERE namespace = $1 AND dispatch_state <> 'dispatched'", f.namespace).Scan(&pending, &oldest); err != nil && err != pgx.ErrNoRows {
		return report, postgresError("queue.outbox_once", err)
	}
	report.Pending = uint64(pending)
	report.OldestPendingAgeMs = oldest
	return report, nil
}

func (f *Forge) RunOutboxRelay(ctx context.Context, options OutboxRelayOptions, onError func(error)) error {
	options, err := defaultOutboxOptions(options)
	if err != nil {
		return err
	}
	relayCtx, stop := context.WithCancel(ctx)
	defer stop()
	go func() {
		select {
		case <-f.shutdown:
			stop()
		case <-relayCtx.Done():
		}
	}()
	attempt := uint32(0)
	for {
		report, runErr := f.RunOutboxOnce(relayCtx, options)
		if runErr == nil {
			attempt = 0
			if report.Claimed > 0 {
				continue
			}
			if !waitContext(relayCtx, options.IdleDelay) {
				if ctx.Err() != nil {
					return ctx.Err()
				}
				return nil
			}
			continue
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
		if relayCtx.Err() != nil {
			return nil
		}
		if onError != nil {
			onError(runErr)
		}
		attempt++
		if !waitContext(relayCtx, jitterRetry(options.FailureBackoff, attempt)) {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return nil
		}
	}
}

func outboxErrorSummary(err error) string {
	switch ErrorCodeOf(err) {
	case CodeUnavailable:
		return "queue unavailable"
	case CodePrecondition:
		return "queue precondition failed"
	case CodeLimit:
		return "queue limit exceeded"
	case CodeInvalid:
		return "outbox row is invalid"
	case CodeNotConfigured:
		return "queue is not configured"
	default:
		return "queue dispatch failed"
	}
}

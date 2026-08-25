package forge

import (
	"context"
	"time"

	"github.com/jackc/pgx/v5"
)

func (f *Forge) pgScheduleAt(ctx context.Context, when time.Time, queue string, payload []byte, options ScheduleOptions) (string, error) {
	id, err := randomID(f.random, "")
	if err != nil {
		return "", err
	}
	maxAttempts := int32(options.MaxAttempts)
	if maxAttempts == 0 {
		maxAttempts = 5
	}
	policy, maxCatchUp, policyErr := schedulePolicy(options)
	if policyErr != nil {
		return "", policyErr
	}
	_, err = f.postgres(PrimitiveSchedule).Exec(ctx, "INSERT INTO forge_schedules (name, kind, target_queue, payload, job_id, next_run, app, max_attempts, misfire_policy, max_catch_up) VALUES ($1, 'at', $2, $3, $4::uuid, $5, $6, $7, $8, $9)", "at:"+id, f.pgScoped(queue), payload, id, when.UTC(), f.namespace, maxAttempts, string(policy), int32(maxCatchUp))
	if err != nil {
		return "", postgresError("schedule.at", err)
	}
	return id, nil
}

func (f *Forge) pgScheduleCron(ctx context.Context, name, expression, queue string, payload []byte, options ScheduleOptions, next time.Time) error {
	maxAttempts := int32(options.MaxAttempts)
	if maxAttempts == 0 {
		maxAttempts = 5
	}
	policy, maxCatchUp, policyErr := schedulePolicy(options)
	if policyErr != nil {
		return policyErr
	}
	_, err := f.postgres(PrimitiveSchedule).Exec(ctx, `INSERT INTO forge_schedules (name, kind, cron_expr, target_queue, payload, next_run, app, max_attempts, misfire_policy, max_catch_up)
VALUES ($1, 'cron', $2, $3, $4, $5, $6, $7, $8, $9)
ON CONFLICT (name, app) DO UPDATE SET kind = 'cron', cron_expr = EXCLUDED.cron_expr, target_queue = EXCLUDED.target_queue, payload = EXCLUDED.payload, next_run = EXCLUDED.next_run, max_attempts = EXCLUDED.max_attempts, misfire_policy = EXCLUDED.misfire_policy, max_catch_up = EXCLUDED.max_catch_up`, name, expression, f.pgScoped(queue), payload, next.UTC(), f.namespace, maxAttempts, string(policy), int32(maxCatchUp))
	return postgresError("schedule.cron", err)
}

func (f *Forge) pgScheduleCancel(ctx context.Context, name string) (bool, error) {
	result, err := f.postgres(PrimitiveSchedule).Exec(ctx, "DELETE FROM forge_schedules WHERE name = $1 AND app = $2", name, f.namespace)
	if err != nil {
		return false, postgresError("schedule.cancel", err)
	}
	return result.RowsAffected() == 1, nil
}

func (f *Forge) pgScheduleList(ctx context.Context, after string, limit uint32) (SchedulePage, error) {
	rows, err := f.postgres(PrimitiveSchedule).Query(ctx, "SELECT name, kind, cron_expr, target_queue, next_run, last_run, paused, misfire_policy, max_catch_up FROM forge_schedules WHERE app = $1 AND name > $2 ORDER BY name LIMIT $3", f.namespace, after, int64(limit)+1)
	if err != nil {
		return SchedulePage{}, postgresError("schedule.list", err)
	}
	defer rows.Close()
	items := make([]ScheduleInfo, 0, limit+1)
	for rows.Next() {
		var info ScheduleInfo
		var queue string
		var next time.Time
		var last *time.Time
		if err := rows.Scan(&info.Name, &info.Kind, &info.CronExpr, &queue, &next, &last, &info.Paused, &info.MisfirePolicy, &info.MaxCatchUp); err != nil {
			return SchedulePage{}, postgresError("schedule.list", err)
		}
		info.Queue = queue[len(f.namespace)+1:]
		info.NextRunMs = float64(next.UnixMilli())
		if last != nil {
			value := float64(last.UnixMilli())
			info.LastRunMs = &value
		}
		items = append(items, info)
	}
	if err := rows.Err(); err != nil {
		return SchedulePage{}, postgresError("schedule.list", err)
	}
	page := SchedulePage{Items: items}
	if uint32(len(items)) > limit {
		page.Items = items[:limit]
		cursor := encodeCursor(page.Items[len(page.Items)-1].Name)
		page.Cursor = &cursor
	}
	return page, nil
}

func (f *Forge) pgScheduleInspect(ctx context.Context, name string) (*ScheduleInfo, error) {
	var info ScheduleInfo
	var queue string
	var next time.Time
	var last *time.Time
	err := f.postgres(PrimitiveSchedule).QueryRow(ctx, "SELECT name, kind, cron_expr, target_queue, next_run, last_run, paused, misfire_policy, max_catch_up FROM forge_schedules WHERE app = $1 AND name = $2", f.namespace, name).Scan(&info.Name, &info.Kind, &info.CronExpr, &queue, &next, &last, &info.Paused, &info.MisfirePolicy, &info.MaxCatchUp)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("schedule.inspect", err)
	}
	info.Queue = f.pgLogical(queue)
	info.NextRunMs = float64(next.UnixMilli())
	if last != nil {
		value := float64(last.UnixMilli())
		info.LastRunMs = &value
	}
	return &info, nil
}

func (f *Forge) pgSchedulePaused(ctx context.Context, name string, paused bool) (bool, error) {
	result, err := f.postgres(PrimitiveSchedule).Exec(ctx, "UPDATE forge_schedules SET paused = $3 WHERE app = $1 AND name = $2", f.namespace, name, paused)
	if err != nil {
		return false, postgresError("schedule.pause", err)
	}
	return result.RowsAffected() == 1, nil
}

func (f *Forge) pgSchedulerDiagnostics(ctx context.Context) (SchedulerDiagnostics, error) {
	var due int64
	var lagMs *float64
	err := f.postgres(PrimitiveSchedule).QueryRow(ctx, "SELECT COUNT(*)::bigint, EXTRACT(EPOCH FROM (now() - MIN(next_run)))::float8 * 1000 FROM forge_schedules WHERE app = $1 AND paused = FALSE AND next_run <= now()", f.namespace).Scan(&due, &lagMs)
	if err != nil {
		return SchedulerDiagnostics{}, postgresError("schedule.diagnostics", err)
	}
	var last *time.Time
	var failures int64
	err = f.postgres(PrimitiveSchedule).QueryRow(ctx, "SELECT last_successful_tick, enqueue_failures FROM forge_scheduler_state WHERE app = $1", f.namespace).Scan(&last, &failures)
	if err != nil && err != pgx.ErrNoRows {
		return SchedulerDiagnostics{}, postgresError("schedule.diagnostics", err)
	}
	result := SchedulerDiagnostics{DueCount: uint64(due), EnqueueFailures: uint64(failures)}
	result.LagMs = lagMs
	if last != nil {
		value := float64(last.UnixMilli())
		result.LastSuccessfulTickMs = &value
	}
	return result, nil
}

func (f *Forge) pgRunSchedulerOnce(ctx context.Context, limit uint32) (uint64, error) {
	tx, err := f.postgres(PrimitiveSchedule).BeginTx(ctx, pgx.TxOptions{})
	if err != nil {
		return 0, postgresError("schedule.process_due", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	rows, err := tx.Query(ctx, "SELECT name, kind, cron_expr, target_queue, payload, job_id::text, next_run, max_attempts, misfire_policy, max_catch_up FROM forge_schedules WHERE app = $1 AND paused = FALSE AND next_run <= now() ORDER BY next_run FOR UPDATE SKIP LOCKED LIMIT $2", f.namespace, limit)
	if err != nil {
		return 0, postgresError("schedule.process_due", err)
	}
	type dueSchedule struct {
		name, kind, expression, queue, jobID string
		payload                              []byte
		next                                 time.Time
		maxAttempts                          int32
		policy                               string
		maxCatchUp                           int32
	}
	due := make([]dueSchedule, 0, limit)
	for rows.Next() {
		var item dueSchedule
		var expression, jobID *string
		if err := rows.Scan(&item.name, &item.kind, &expression, &item.queue, &item.payload, &jobID, &item.next, &item.maxAttempts, &item.policy, &item.maxCatchUp); err != nil {
			rows.Close()
			return 0, postgresError("schedule.process_due", err)
		}
		if expression != nil {
			item.expression = *expression
		}
		if jobID != nil {
			item.jobID = *jobID
		}
		due = append(due, item)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return 0, postgresError("schedule.process_due", err)
	}
	now := time.Now().UTC()
	var processed uint64
	for _, item := range due {
		policy, maximum, policyErr := schedulePolicy(ScheduleOptions{MisfirePolicy: MisfirePolicy(item.policy), MaxCatchUp: uint32(item.maxCatchUp)})
		if policyErr != nil {
			return processed, policyErr
		}
		var occurrences []time.Time
		var next time.Time
		if item.kind == "cron" {
			occurrences, next, err = planCronOccurrences(item.expression, item.next, now, policy, maximum)
			if err != nil {
				return processed, forgeError(CodeInvalid, "schedule.process_due", err.Error())
			}
		} else if policy != MisfireSkip {
			occurrences = []time.Time{item.next}
		}
		queue := f.pgLogical(item.queue)
		for _, occurrence := range occurrences {
			jobID := item.jobID
			if jobID == "" {
				jobID = f.scheduleTickID(item.name, occurrence)
			}
			if _, err := f.pgEnqueue(ctx, queue, item.payload, EnqueueOptions{ID: jobID, MaxAttempts: uint32(item.maxAttempts)}); err != nil {
				_ = tx.Rollback(ctx)
				_, _ = f.postgres(PrimitiveSchedule).Exec(ctx, "INSERT INTO forge_scheduler_state (app, enqueue_failures) VALUES ($1, 1) ON CONFLICT (app) DO UPDATE SET enqueue_failures = forge_scheduler_state.enqueue_failures + 1", f.namespace)
				return processed, err
			}
			processed++
		}
		if item.kind == "at" {
			if _, err := tx.Exec(ctx, "DELETE FROM forge_schedules WHERE name = $1 AND app = $2", item.name, f.namespace); err != nil {
				return 0, postgresError("schedule.process_due", err)
			}
		} else if !next.IsZero() {
			var lastRun *time.Time
			if len(occurrences) > 0 {
				value := occurrences[len(occurrences)-1]
				lastRun = &value
			}
			if _, err := tx.Exec(ctx, "UPDATE forge_schedules SET last_run = COALESCE($3, last_run), next_run = $4 WHERE name = $1 AND app = $2", item.name, f.namespace, lastRun, next); err != nil {
				return 0, postgresError("schedule.process_due", err)
			}
		} else if _, err := tx.Exec(ctx, "DELETE FROM forge_schedules WHERE name = $1 AND app = $2", item.name, f.namespace); err != nil {
			return 0, postgresError("schedule.process_due", err)
		}
	}
	if _, err := tx.Exec(ctx, "INSERT INTO forge_scheduler_state (app, last_successful_tick) VALUES ($1, $2) ON CONFLICT (app) DO UPDATE SET last_successful_tick = EXCLUDED.last_successful_tick", f.namespace, now); err != nil {
		return 0, postgresError("schedule.process_due", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return 0, postgresError("schedule.process_due", err)
	}
	return processed, nil
}

package forge

import (
	"context"
	"crypto/sha256"
	"fmt"
	"sort"
	"strconv"
	"strings"
	"time"
)

func (f *Forge) scheduleTickID(name string, at time.Time) string {
	hash := sha256.New()
	hash.Write([]byte("forge:schedule:tick:v1\x00"))
	hash.Write([]byte(f.namespace))
	hash.Write([]byte{0})
	hash.Write([]byte(name))
	hash.Write([]byte{0})
	hash.Write([]byte(strconv.FormatInt(at.UnixNano(), 10)))
	var value [16]byte
	copy(value[:], hash.Sum(nil))
	value[6] = (value[6] & 0x0f) | 0x80
	value[8] = (value[8] & 0x3f) | 0x80
	return formatUUID(value)
}

type ScheduleOptions struct {
	MaxAttempts   uint32
	MisfirePolicy MisfirePolicy
	MaxCatchUp    uint32
}

type MisfirePolicy string

const (
	MisfireSkip        MisfirePolicy = "skip"
	MisfireRunOnce     MisfirePolicy = "run_once"
	MisfireCatchUp     MisfirePolicy = "catch_up"
	MaxScheduleCatchUp uint32        = 100
)

func schedulePolicy(options ScheduleOptions) (MisfirePolicy, uint32, error) {
	policy := options.MisfirePolicy
	if policy == "" {
		policy = MisfireRunOnce
	}
	switch policy {
	case MisfireSkip, MisfireRunOnce:
		if options.MaxCatchUp != 0 {
			return "", 0, forgeError(CodeInvalid, "schedule.options", "max catch-up is only valid with catch_up")
		}
		return policy, 0, nil
	case MisfireCatchUp:
		maximum := options.MaxCatchUp
		if maximum == 0 {
			maximum = 10
		}
		if maximum > MaxScheduleCatchUp {
			return "", 0, forgeError(CodeLimit, "schedule.options", "max catch-up exceeds 100")
		}
		return policy, maximum, nil
	default:
		return "", 0, forgeError(CodeInvalid, "schedule.options", "misfire policy must be skip, run_once, or catch_up")
	}
}

type memorySchedule struct {
	name          string
	kind          string
	expression    string
	queue         string
	payload       []byte
	jobID         string
	nextRun       time.Time
	lastRun       time.Time
	maxAttempts   uint32
	paused        bool
	misfirePolicy MisfirePolicy
	maxCatchUp    uint32
}

func (f *Forge) ScheduleAt(ctx context.Context, when time.Time, queue string, payload []byte, options ScheduleOptions) (string, error) {
	if err := f.ready(ctx, "schedule.at"); err != nil {
		return "", err
	}
	if queue == "" || when.IsZero() {
		return "", forgeError(CodeInvalid, "schedule.at", "time and queue are required")
	}
	if len(payload) > MaxQueuePayloadBytes {
		return "", forgeError(CodeLimit, "schedule.at", "payload exceeds 256 KiB")
	}
	policy, maxCatchUp, err := schedulePolicy(options)
	if err != nil {
		return "", err
	}
	if f.mode == ModePostgres {
		return f.pgScheduleAt(ctx, when, queue, payload, options)
	}
	id, err := randomID(f.random, "")
	if err != nil {
		return "", err
	}
	schedule := memorySchedule{
		name:          "at:" + id,
		kind:          "at",
		queue:         queue,
		payload:       append([]byte(nil), payload...),
		jobID:         id,
		nextRun:       when,
		maxAttempts:   options.MaxAttempts,
		misfirePolicy: policy,
		maxCatchUp:    maxCatchUp,
	}
	f.store.mu.Lock()
	f.store.schedules[f.scoped(schedule.name)] = schedule
	f.store.mu.Unlock()
	return id, nil
}

func (f *Forge) ScheduleCron(ctx context.Context, name, expression, queue string, payload []byte, options ScheduleOptions) error {
	if err := f.ready(ctx, "schedule.cron"); err != nil {
		return err
	}
	if name == "" || queue == "" {
		return forgeError(CodeInvalid, "schedule.cron", "name and queue are required")
	}
	next, err := nextCron(expression, f.now())
	if err != nil {
		return forgeError(CodeInvalid, "schedule.cron", err.Error())
	}
	if len(payload) > MaxQueuePayloadBytes {
		return forgeError(CodeLimit, "schedule.cron", "payload exceeds 256 KiB")
	}
	policy, maxCatchUp, err := schedulePolicy(options)
	if err != nil {
		return err
	}
	if f.mode == ModePostgres {
		return f.pgScheduleCron(ctx, name, expression, queue, payload, options, next)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	key := f.scoped(name)
	current := f.store.schedules[key]
	f.store.schedules[key] = memorySchedule{
		name:          name,
		kind:          "cron",
		expression:    expression,
		queue:         queue,
		payload:       append([]byte(nil), payload...),
		nextRun:       next,
		lastRun:       current.lastRun,
		maxAttempts:   options.MaxAttempts,
		paused:        current.paused,
		misfirePolicy: policy,
		maxCatchUp:    maxCatchUp,
	}
	return nil
}

func (f *Forge) ScheduleCancel(ctx context.Context, name string) (bool, error) {
	if err := f.ready(ctx, "schedule.cancel"); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgScheduleCancel(ctx, name)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	key := f.scoped(name)
	_, ok := f.store.schedules[key]
	delete(f.store.schedules, key)
	return ok, nil
}

func (f *Forge) ScheduleCancelAt(ctx context.Context, jobID string) (bool, error) {
	return f.ScheduleCancel(ctx, "at:"+jobID)
}

func scheduleInfo(schedule memorySchedule) ScheduleInfo {
	var expression *string
	if schedule.kind == "cron" {
		value := schedule.expression
		expression = &value
	}
	var lastRun *float64
	if !schedule.lastRun.IsZero() {
		value := float64(schedule.lastRun.UnixMilli())
		lastRun = &value
	}
	return ScheduleInfo{
		Name: schedule.name, Kind: schedule.kind, CronExpr: expression, Queue: schedule.queue,
		NextRunMs: float64(schedule.nextRun.UnixMilli()), LastRunMs: lastRun,
		Paused: schedule.paused, MisfirePolicy: string(schedule.misfirePolicy), MaxCatchUp: schedule.maxCatchUp,
	}
}

func (f *Forge) ScheduleInspect(ctx context.Context, name string) (*ScheduleInfo, error) {
	if err := f.ready(ctx, "schedule.inspect"); err != nil {
		return nil, err
	}
	if f.mode == ModePostgres {
		return f.pgScheduleInspect(ctx, name)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	schedule, ok := f.store.schedules[f.scoped(name)]
	if !ok {
		return nil, nil
	}
	info := scheduleInfo(schedule)
	return &info, nil
}

func (f *Forge) SchedulePause(ctx context.Context, name string) (bool, error) {
	if err := f.ready(ctx, "schedule.pause"); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgSchedulePaused(ctx, name, true)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	key := f.scoped(name)
	schedule, ok := f.store.schedules[key]
	if !ok {
		return false, nil
	}
	schedule.paused = true
	f.store.schedules[key] = schedule
	return true, nil
}

func (f *Forge) ScheduleResume(ctx context.Context, name string) (bool, error) {
	if err := f.ready(ctx, "schedule.resume"); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgSchedulePaused(ctx, name, false)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	key := f.scoped(name)
	schedule, ok := f.store.schedules[key]
	if !ok {
		return false, nil
	}
	schedule.paused = false
	f.store.schedules[key] = schedule
	return true, nil
}

func (f *Forge) SchedulerDiagnostics(ctx context.Context) (SchedulerDiagnostics, error) {
	if err := f.ready(ctx, "schedule.diagnostics"); err != nil {
		return SchedulerDiagnostics{}, err
	}
	if f.mode == ModePostgres {
		return f.pgSchedulerDiagnostics(ctx)
	}
	now := f.now()
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	var due uint64
	var oldest time.Time
	for key, schedule := range f.store.schedules {
		if !strings.HasPrefix(key, f.namespace+"\x00") || schedule.paused || schedule.nextRun.After(now) {
			continue
		}
		due++
		if oldest.IsZero() || schedule.nextRun.Before(oldest) {
			oldest = schedule.nextRun
		}
	}
	var lag *float64
	if !oldest.IsZero() {
		value := float64(now.Sub(oldest).Milliseconds())
		lag = &value
	}
	var last *float64
	if value := f.store.schedulerLastSuccess[f.namespace]; !value.IsZero() {
		ms := float64(value.UnixMilli())
		last = &ms
	}
	return SchedulerDiagnostics{LagMs: lag, LastSuccessfulTickMs: last, DueCount: due, EnqueueFailures: f.store.schedulerEnqueueFailures[f.namespace]}, nil
}

func (f *Forge) ScheduleList(ctx context.Context, cursor *string, limit uint32) (SchedulePage, error) {
	if err := f.ready(ctx, "schedule.list"); err != nil {
		return SchedulePage{}, err
	}
	if limit == 0 {
		limit = 100
	}
	after, err := decodeCursor(cursor)
	if err != nil {
		return SchedulePage{}, forgeError(CodeInvalid, "schedule.list", "cursor is malformed")
	}
	if f.mode == ModePostgres {
		return f.pgScheduleList(ctx, after, limit)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	names := make([]string, 0)
	prefix := f.namespace + "\x00"
	for key, schedule := range f.store.schedules {
		if strings.HasPrefix(key, prefix) && schedule.name > after {
			names = append(names, schedule.name)
		}
	}
	sort.Strings(names)
	pageNames := names
	var next *string
	if uint32(len(names)) > limit {
		pageNames = names[:limit]
		value := encodeCursor(pageNames[len(pageNames)-1])
		next = &value
	}
	items := make([]ScheduleInfo, 0, len(pageNames))
	for _, name := range pageNames {
		schedule := f.store.schedules[f.scoped(name)]
		items = append(items, scheduleInfo(schedule))
	}
	return SchedulePage{Items: items, Cursor: next}, nil
}

func (f *Forge) RunSchedulerOnce(ctx context.Context, limit uint32) (uint64, error) {
	if err := f.ready(ctx, "schedule.process_due"); err != nil {
		return 0, err
	}
	if limit == 0 {
		limit = 100
	}
	if f.mode == ModePostgres {
		return f.pgRunSchedulerOnce(ctx, limit)
	}
	now := f.now()
	type dueItem struct {
		key         string
		schedule    memorySchedule
		occurrences []time.Time
		next        time.Time
	}
	due := make([]dueItem, 0, limit)
	f.store.mu.Lock()
	for key, schedule := range f.store.schedules {
		if schedule.name == "" || !strings.HasPrefix(key, f.namespace+"\x00") || schedule.paused || schedule.nextRun.After(now) {
			continue
		}
		var occurrences []time.Time
		var next time.Time
		if schedule.kind == "cron" {
			var err error
			occurrences, next, err = planCronOccurrences(schedule.expression, schedule.nextRun, now, schedule.misfirePolicy, schedule.maxCatchUp)
			if err != nil {
				f.store.mu.Unlock()
				return 0, forgeError(CodeInvalid, "schedule.process_due", err.Error())
			}
		} else if schedule.misfirePolicy != MisfireSkip {
			occurrences = []time.Time{schedule.nextRun}
		}
		due = append(due, dueItem{key: key, schedule: schedule, occurrences: occurrences, next: next})
	}
	sort.Slice(due, func(i, j int) bool { return due[i].schedule.nextRun.Before(due[j].schedule.nextRun) })
	if uint32(len(due)) > limit {
		due = due[:limit]
	}
	f.store.mu.Unlock()

	var processed uint64
	for _, item := range due {
		for _, occurrence := range item.occurrences {
			jobID := item.schedule.jobID
			if jobID == "" {
				jobID = f.scheduleTickID(item.schedule.name, occurrence)
			}
			_, err := f.Enqueue(ctx, item.schedule.queue, item.schedule.payload, EnqueueOptions{ID: jobID, MaxAttempts: item.schedule.maxAttempts})
			if err != nil {
				f.store.mu.Lock()
				f.store.schedulerEnqueueFailures[f.namespace]++
				f.store.mu.Unlock()
				return processed, err
			}
			processed++
		}
		f.store.mu.Lock()
		current, ok := f.store.schedules[item.key]
		if ok && current.nextRun.Equal(item.schedule.nextRun) && !current.paused {
			if item.schedule.kind == "cron" && !item.next.IsZero() {
				current.nextRun = item.next
				if len(item.occurrences) > 0 {
					current.lastRun = item.occurrences[len(item.occurrences)-1]
				}
				f.store.schedules[item.key] = current
			} else {
				delete(f.store.schedules, item.key)
			}
		}
		f.store.mu.Unlock()
	}
	f.store.mu.Lock()
	f.store.schedulerLastSuccess[f.namespace] = now
	f.store.mu.Unlock()
	return processed, nil
}

func planCronOccurrences(expression string, first, now time.Time, policy MisfirePolicy, maximum uint32) ([]time.Time, time.Time, error) {
	next, err := nextCron(expression, now)
	if err != nil {
		return nil, time.Time{}, err
	}
	if policy == MisfireSkip {
		return nil, next, nil
	}
	latest, err := previousCron(expression, now)
	if err != nil {
		return nil, time.Time{}, err
	}
	if latest.Before(first) {
		return nil, next, nil
	}
	count := uint32(1)
	if policy == MisfireCatchUp {
		count = maximum
	}
	occurrences := make([]time.Time, 0, count)
	current := latest
	for uint32(len(occurrences)) < count && !current.Before(first) {
		occurrences = append(occurrences, current)
		current, err = previousCron(expression, current.Add(-time.Minute))
		if err != nil {
			break
		}
	}
	for left, right := 0, len(occurrences)-1; left < right; left, right = left+1, right-1 {
		occurrences[left], occurrences[right] = occurrences[right], occurrences[left]
	}
	return occurrences, next, nil
}

func previousCron(expression string, at time.Time) (time.Time, error) {
	fields := strings.Fields(expression)
	if len(fields) != 5 {
		return time.Time{}, fmt.Errorf("cron expression must have five fields")
	}
	minute, err := cronField(fields[0], 0, 59)
	if err != nil {
		return time.Time{}, err
	}
	hour, err := cronField(fields[1], 0, 23)
	if err != nil {
		return time.Time{}, err
	}
	day, err := cronField(fields[2], 1, 31)
	if err != nil {
		return time.Time{}, err
	}
	month, err := cronField(fields[3], 1, 12)
	if err != nil {
		return time.Time{}, err
	}
	weekday, err := cronField(fields[4], 0, 6)
	if err != nil {
		return time.Time{}, err
	}
	candidate := at.UTC().Truncate(time.Minute)
	for attempts := 0; attempts < 60*24*366*5; attempts++ {
		if matchesCron(minute, candidate.Minute()) && matchesCron(hour, candidate.Hour()) && matchesCron(day, candidate.Day()) && matchesCron(month, int(candidate.Month())) && matchesCron(weekday, int(candidate.Weekday())) {
			return candidate, nil
		}
		candidate = candidate.Add(-time.Minute)
	}
	return time.Time{}, fmt.Errorf("cron expression has no occurrence within five years")
}

func nextCron(expression string, after time.Time) (time.Time, error) {
	fields := strings.Fields(expression)
	if len(fields) != 5 {
		return time.Time{}, fmt.Errorf("cron expression must have five fields")
	}
	minute, err := cronField(fields[0], 0, 59)
	if err != nil {
		return time.Time{}, err
	}
	hour, err := cronField(fields[1], 0, 23)
	if err != nil {
		return time.Time{}, err
	}
	day, err := cronField(fields[2], 1, 31)
	if err != nil {
		return time.Time{}, err
	}
	month, err := cronField(fields[3], 1, 12)
	if err != nil {
		return time.Time{}, err
	}
	weekday, err := cronField(fields[4], 0, 6)
	if err != nil {
		return time.Time{}, err
	}
	candidate := after.UTC().Truncate(time.Minute).Add(time.Minute)
	for attempts := 0; attempts < 60*24*366*5; attempts++ {
		if matchesCron(minute, candidate.Minute()) &&
			matchesCron(hour, candidate.Hour()) &&
			matchesCron(day, candidate.Day()) &&
			matchesCron(month, int(candidate.Month())) &&
			matchesCron(weekday, int(candidate.Weekday())) {
			return candidate, nil
		}
		candidate = candidate.Add(time.Minute)
	}
	return time.Time{}, fmt.Errorf("cron expression has no occurrence within five years")
}

func cronField(value string, minimum, maximum int) (*int, error) {
	if value == "*" {
		return nil, nil
	}
	parsed, err := strconv.Atoi(value)
	if err != nil || parsed < minimum || parsed > maximum {
		return nil, fmt.Errorf("cron field %q is outside %d..%d", value, minimum, maximum)
	}
	return &parsed, nil
}

func matchesCron(expected *int, actual int) bool {
	return expected == nil || *expected == actual
}

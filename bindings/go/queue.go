package forge

import (
	"context"
	"encoding/json"
	"errors"
	"sort"
	"strings"
	"time"
	"unicode/utf8"
)

const MaxQueuePayloadBytes = 256 * 1024

type Priority string

const (
	PriorityLow    Priority = "low"
	PriorityNormal Priority = "normal"
	PriorityHigh   Priority = "high"
)

type JobState string

const (
	JobQueued          JobState = "queued"
	JobDelayed         JobState = "delayed"
	JobLeased          JobState = "leased"
	JobRetrying        JobState = "retrying"
	JobSucceeded       JobState = "succeeded"
	JobDead            JobState = "dead"
	JobCancelRequested JobState = "cancel_requested"
	JobCancelled       JobState = "cancelled"
)

type JobStatus struct {
	ID             string     `json:"id"`
	Queue          string     `json:"queue"`
	State          JobState   `json:"state"`
	AttemptCount   uint32     `json:"attempt_count"`
	MaxAttempts    uint32     `json:"max_attempts"`
	Priority       Priority   `json:"priority"`
	ConcurrencyKey *string    `json:"concurrency_key"`
	EnqueuedAt     time.Time  `json:"enqueued_at"`
	AvailableAt    time.Time  `json:"available_at"`
	CompletedAt    *time.Time `json:"completed_at"`
}

type JobStatusFilter struct {
	Queue  string
	States []JobState
	Cursor string
	Limit  uint32
}

type JobStatusPage struct {
	Items  []JobStatus `json:"items"`
	Cursor *string     `json:"cursor"`
}

type ArtifactRef struct {
	URI         string `json:"uri"`
	ContentType string `json:"content_type,omitempty"`
	Version     string `json:"version,omitempty"`
}

type QueueEnvelope struct {
	Version       uint16        `json:"version"`
	Schema        string        `json:"schema"`
	ContentType   string        `json:"content_type"`
	CorrelationID string        `json:"correlation_id,omitempty"`
	TraceContext  *TraceContext `json:"trace_context,omitempty"`
	Artifacts     []ArtifactRef `json:"artifacts,omitempty"`
	Body          []byte        `json:"-"`
}

func NewQueueEnvelope(schema, contentType string, body []byte) QueueEnvelope {
	return QueueEnvelope{Version: 1, Schema: schema, ContentType: contentType, Body: append([]byte(nil), body...)}
}

func (envelope QueueEnvelope) Encode() ([]byte, error) {
	if envelope.Version != 1 || envelope.Schema == "" || envelope.ContentType == "" {
		return nil, forgeError(CodeInvalid, "queue.envelope.encode", "version 1, schema, and content type are required")
	}
	if len(envelope.Schema) > 256 || len(envelope.ContentType) > 128 || len(envelope.CorrelationID) > 256 || len(envelope.Artifacts) > 32 {
		return nil, forgeError(CodeLimit, "queue.envelope.encode", "envelope metadata exceeds its limit")
	}
	if envelope.TraceContext != nil {
		if envelope.TraceContext.Traceparent == "" {
			return nil, forgeError(CodeInvalid, "queue.envelope.encode", "traceparent is required")
		}
		baggageItems := strings.Split(envelope.TraceContext.Baggage, ",")
		allowlist := make([]string, 0, len(baggageItems))
		for _, item := range baggageItems {
			key, _, ok := strings.Cut(strings.TrimSpace(item), "=")
			if item != "" && (!ok || key == "") {
				return nil, forgeError(CodeInvalid, "queue.envelope.encode", "trace context baggage is invalid")
			}
			if key != "" {
				allowlist = append(allowlist, key)
			}
		}
		if len(allowlist) > maxBaggageItems {
			return nil, forgeError(CodeLimit, "queue.envelope.encode", "trace context baggage has too many items")
		}
		if _, err := NewTraceContext(envelope.TraceContext.Traceparent, envelope.TraceContext.Tracestate, envelope.TraceContext.Baggage, allowlist); err != nil {
			return nil, err
		}
	}
	for _, artifact := range envelope.Artifacts {
		if artifact.URI == "" || len(artifact.URI) > 2048 || len(artifact.ContentType) > 128 || len(artifact.Version) > 256 {
			return nil, forgeError(CodeLimit, "queue.envelope.encode", "artifact metadata exceeds its limit")
		}
	}
	body := make([]uint16, len(envelope.Body))
	for index, value := range envelope.Body {
		body[index] = uint16(value)
	}
	wire := struct {
		Version       uint16        `json:"version"`
		Schema        string        `json:"schema"`
		ContentType   string        `json:"content_type"`
		CorrelationID string        `json:"correlation_id,omitempty"`
		TraceContext  *TraceContext `json:"trace_context,omitempty"`
		Artifacts     []ArtifactRef `json:"artifacts,omitempty"`
		Body          []uint16      `json:"body"`
	}{envelope.Version, envelope.Schema, envelope.ContentType, envelope.CorrelationID, envelope.TraceContext, envelope.Artifacts, body}
	encoded, err := json.Marshal(wire)
	if err != nil {
		return nil, errorWithCause(CodeInvalid, "queue.envelope.encode", "", "could not encode envelope", err)
	}
	if len(encoded) > MaxQueuePayloadBytes {
		return nil, forgeError(CodeLimit, "queue.envelope.encode", "encoded envelope exceeds 256 KiB; use blob references for large bodies")
	}
	return encoded, nil
}

func DecodeQueueEnvelope(encoded []byte) (QueueEnvelope, error) {
	var wire struct {
		Version       uint16        `json:"version"`
		Schema        string        `json:"schema"`
		ContentType   string        `json:"content_type"`
		CorrelationID string        `json:"correlation_id"`
		TraceContext  *TraceContext `json:"trace_context"`
		Artifacts     []ArtifactRef `json:"artifacts"`
		Body          []uint16      `json:"body"`
	}
	if len(encoded) > MaxQueuePayloadBytes {
		return QueueEnvelope{}, forgeError(CodeLimit, "queue.envelope.decode", "encoded envelope exceeds 256 KiB")
	}
	if err := json.Unmarshal(encoded, &wire); err != nil {
		return QueueEnvelope{}, errorWithCause(CodeInvalid, "queue.envelope.decode", "", "could not decode envelope", err)
	}
	body := make([]byte, len(wire.Body))
	for index, value := range wire.Body {
		if value > 255 {
			return QueueEnvelope{}, forgeError(CodeInvalid, "queue.envelope.decode", "body contains a non-byte value")
		}
		body[index] = byte(value)
	}
	envelope := QueueEnvelope{wire.Version, wire.Schema, wire.ContentType, wire.CorrelationID, wire.TraceContext, wire.Artifacts, body}
	if _, err := envelope.Encode(); err != nil {
		return QueueEnvelope{}, err
	}
	return envelope, nil
}

type EnqueueOptions struct {
	ID             string
	MaxAttempts    uint32
	DedupID        string
	Delay          time.Duration
	TraceContext   *TraceContext
	Priority       Priority
	ConcurrencyKey string
}

type DequeueOptions struct {
	Visibility             time.Duration
	Wait                   time.Duration
	ConcurrencyLimitPerKey uint32
}

type NackOptions struct {
	RetryIn        time.Duration
	FailureSummary string
}

type RedriveOptions struct {
	Destination string
	DedupPolicy string
}

const maxEnqueueBatch = 100
const maxDequeueBatch = 10

type BatchEnqueueItem struct {
	Payload []byte
	Options EnqueueOptions
}

type memoryQueueCounter struct {
	startedAt time.Time
	enqueued  uint64
	settled   uint64
	dead      uint64
	cancelled uint64
}

type memoryJob struct {
	id              string
	namespace       string
	queue           string
	payload         []byte
	payloadRetained bool
	status          string
	attempts        uint32
	maxAttempts     uint32
	availableAt     time.Time
	leasedUntil     time.Time
	receipt         string
	enqueuedAt      time.Time
	completedAt     time.Time
	deadLetteredAt  time.Time
	deadAttempts    uint32
	failureSummary  string
	traceContext    *TraceContext
	priority        Priority
	concurrencyKey  string
	cancelRequested bool
}

type memoryDedup struct {
	jobID     string
	expiresAt time.Time
}

func (f *Forge) Enqueue(ctx context.Context, queue string, payload []byte, options EnqueueOptions) (string, error) {
	if err := f.ready(ctx, "queue.enqueue"); err != nil {
		return "", err
	}
	if queue == "" {
		return "", forgeError(CodeInvalid, "queue.enqueue", "queue cannot be empty")
	}
	if len(payload) > MaxQueuePayloadBytes {
		return "", forgeError(CodeLimit, "queue.enqueue", "payload exceeds 256 KiB")
	}
	if options.Delay < 0 {
		return "", forgeError(CodeInvalid, "queue.enqueue", "delay cannot be negative")
	}
	if options.ID != "" && !looksLikeUUID(options.ID) {
		return "", forgeError(CodeInvalid, "queue.enqueue", "job ID must be a UUID")
	}
	if options.MaxAttempts == 0 {
		options.MaxAttempts = 5
	}
	if options.Priority == "" {
		options.Priority = PriorityNormal
	}
	if options.Priority != PriorityLow && options.Priority != PriorityNormal && options.Priority != PriorityHigh {
		return "", forgeError(CodeInvalid, "queue.enqueue", "priority must be low, normal, or high")
	}
	if len(options.ConcurrencyKey) > 256 {
		return "", forgeError(CodeLimit, "queue.enqueue", "concurrency key exceeds 256 bytes")
	}
	if options.TraceContext != nil {
		validated, err := NewTraceContext(options.TraceContext.Traceparent, options.TraceContext.Tracestate, options.TraceContext.Baggage, baggageKeys(options.TraceContext.Baggage))
		if err != nil {
			return "", err
		}
		options.TraceContext = &validated
	}
	if f.mode == ModePostgres {
		return f.pgEnqueue(ctx, queue, payload, options)
	}

	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	now := f.now()
	dedupKey := f.scoped(queue + "\x00" + options.DedupID)
	if options.DedupID != "" {
		if reservation, ok := f.store.dedup[dedupKey]; ok && now.Before(reservation.expiresAt) {
			if existing := f.store.jobs[reservation.jobID]; existing != nil && existing.status != "dead" {
				if options.ID != "" && options.ID != existing.id {
					return "", forgeError(CodePrecondition, "queue.enqueue", "deduplication ID is reserved by a different job ID")
				}
				return existing.id, nil
			}
			delete(f.store.dedup, dedupKey)
		}
	}
	id := options.ID
	if id == "" {
		var err error
		id, err = randomID(f.random, "")
		if err != nil {
			return "", err
		}
	}
	if existing := f.store.jobs[id]; existing != nil {
		if existing.namespace != f.namespace || existing.queue != queue {
			return "", forgeError(CodePrecondition, "queue.enqueue", "job ID already belongs to another queue or namespace")
		}
		return existing.id, nil
	}
	job := &memoryJob{
		id:              id,
		namespace:       f.namespace,
		queue:           queue,
		payload:         append([]byte(nil), payload...),
		payloadRetained: true,
		status:          "available",
		maxAttempts:     options.MaxAttempts,
		availableAt:     now.Add(options.Delay),
		enqueuedAt:      now,
		traceContext:    options.TraceContext,
		priority:        options.Priority,
		concurrencyKey:  options.ConcurrencyKey,
	}
	f.store.jobs[id] = job
	f.store.jobOrder = append(f.store.jobOrder, id)
	counter := f.store.queueCounters[f.scoped(queue)]
	if counter == nil {
		counter = &memoryQueueCounter{startedAt: now}
		f.store.queueCounters[f.scoped(queue)] = counter
	}
	counter.enqueued++
	if options.DedupID != "" {
		f.store.dedup[dedupKey] = memoryDedup{jobID: id, expiresAt: now.Add(5 * time.Minute)}
	}
	return id, nil
}

// EnqueueBatch returns one ordered result per item; failed items do not roll back siblings.
func (f *Forge) EnqueueBatch(ctx context.Context, queue string, items []BatchEnqueueItem) ([]BatchEnqueueResult, error) {
	if len(items) == 0 || len(items) > maxEnqueueBatch {
		return nil, forgeError(CodeLimit, "queue.enqueue_batch", "batch size must be in 1..=100")
	}
	results := make([]BatchEnqueueResult, len(items))
	for index, item := range items {
		jobID, err := f.Enqueue(ctx, queue, item.Payload, item.Options)
		if err == nil {
			results[index].JobID = &jobID
			continue
		}
		code := string(ErrorCodeOf(err))
		message := err.Error()
		var forgeErr *Error
		if errors.As(err, &forgeErr) {
			message = forgeErr.Message
			results[index].Retryable = forgeErr.Retryable
		}
		results[index].ErrorCode = &code
		results[index].Message = &message
	}
	return results, nil
}

func (f *Forge) Dequeue(ctx context.Context, queue string, options DequeueOptions) (*Job, error) {
	if err := f.ready(ctx, "queue.dequeue"); err != nil {
		return nil, err
	}
	if queue == "" || options.Visibility <= 0 || options.Wait < 0 {
		return nil, forgeError(CodeInvalid, "queue.dequeue", "queue and positive visibility are required")
	}
	if f.mode == ModePostgres {
		return f.pgDequeue(ctx, queue, options)
	}
	deadline := f.now().Add(options.Wait)
	for {
		f.store.mu.Lock()
		if f.store.queuePaused[f.scoped(queue)] {
			f.store.mu.Unlock()
			return nil, nil
		}
		job, err := f.dequeueLocked(queue, options.Visibility, options.ConcurrencyLimitPerKey)
		f.store.mu.Unlock()
		if err != nil || job != nil {
			return job, err
		}
		if options.Wait == 0 || !f.now().Before(deadline) {
			return nil, nil
		}
		wait := 10 * time.Millisecond
		remaining := time.Until(deadline)
		if remaining < wait {
			wait = remaining
		}
		timer := time.NewTimer(wait)
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, errorWithCause(CodeUnavailable, "queue.dequeue", "", "dequeue was cancelled", ctx.Err())
		case <-timer.C:
		}
	}
}

// DequeueBatch long-polls only for the first lease; subsequent claims are immediate.
func (f *Forge) DequeueBatch(ctx context.Context, queue string, maxItems int, options DequeueOptions) ([]*Job, error) {
	if maxItems < 1 || maxItems > maxDequeueBatch {
		return nil, forgeError(CodeLimit, "queue.dequeue_batch", "batch size must be in 1..=10")
	}
	jobs := make([]*Job, 0, maxItems)
	for len(jobs) < maxItems {
		job, err := f.Dequeue(ctx, queue, options)
		if err != nil {
			return nil, err
		}
		if job == nil {
			break
		}
		jobs = append(jobs, job)
		options.Wait = 0
	}
	return jobs, nil
}

func (f *Forge) dequeueLocked(queue string, visibility time.Duration, concurrencyLimit uint32) (*Job, error) {
	now := f.now()
	var selected *memoryJob
	for _, id := range f.store.jobOrder {
		job := f.store.jobs[id]
		if job == nil || job.namespace != f.namespace || job.queue != queue {
			continue
		}
		if job.status == "leased" && !now.Before(job.leasedUntil) {
			if job.cancelRequested {
				job.status = "cancelled"
				job.completedAt = now
				job.receipt = ""
				continue
			}
			job.status = "available"
			job.attempts++
			job.receipt = ""
			if job.attempts >= job.maxAttempts {
				f.releaseDedupLocked(job.id)
				job.deadAttempts = job.attempts
				job.failureSummary = "visibility timeout expired"
				job.deadLetteredAt = now
				if len(job.queue) >= 4 && job.queue[len(job.queue)-4:] == ".dlq" {
					job.status = "dead"
					job.completedAt = now
				} else {
					job.queue += ".dlq"
					job.status = "available"
					job.attempts = 0
					job.availableAt = now
				}
				continue
			}
		}
		if job.status != "available" || now.Before(job.availableAt) {
			continue
		}
		if selected == nil || priorityRank(job.priority) > priorityRank(selected.priority) {
			if job.concurrencyKey != "" && concurrencyLimit > 0 && f.leasedForKeyLocked(queue, job.concurrencyKey, now) >= concurrencyLimit {
				continue
			}
			selected = job
		}
	}
	if selected != nil {
		job := selected
		receipt, err := randomID(f.random, "r_")
		if err != nil {
			return nil, err
		}
		job.status = "leased"
		job.receipt = receipt
		job.leasedUntil = now.Add(visibility)
		f.store.receipts[receipt] = job.id
		traceparent, tracestate, baggage := tracePointers(job.traceContext)
		return &Job{
			ID:            job.id,
			Receipt:       receipt,
			Payload:       append([]byte(nil), job.payload...),
			Attempt:       job.attempts + 1,
			MaxAttempts:   job.maxAttempts,
			LeasedUntilMs: float64(job.leasedUntil.UnixMilli()),
			Queue:         queue,
			Traceparent:   traceparent,
			Tracestate:    tracestate,
			Baggage:       baggage,
		}, nil
	}
	return nil, nil
}

func priorityRank(priority Priority) int {
	switch priority {
	case PriorityHigh:
		return 2
	case PriorityLow:
		return 0
	default:
		return 1
	}
}

func (f *Forge) leasedForKeyLocked(queue, key string, now time.Time) uint32 {
	var count uint32
	for _, job := range f.store.jobs {
		if job.namespace == f.namespace && job.queue == queue && job.status == "leased" && !job.cancelRequested && job.concurrencyKey == key && now.Before(job.leasedUntil) {
			count++
		}
	}
	return count
}

func baggageKeys(baggage string) []string {
	keys := make([]string, 0)
	for _, member := range strings.Split(baggage, ",") {
		key, _, ok := strings.Cut(strings.TrimSpace(member), "=")
		if ok && key != "" {
			keys = append(keys, strings.TrimSpace(key))
		}
	}
	return keys
}

func (f *Forge) Ack(ctx context.Context, receipt string) error {
	if err := contextReady(ctx, "queue.ack"); err != nil {
		return err
	}
	if f.mode == ModePostgres {
		return f.pgAck(ctx, receipt)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	id, ok := f.store.receipts[receipt]
	if !ok {
		return forgeError(CodePrecondition, "queue.ack", "receipt is unknown or its lease was lost")
	}
	job := f.store.jobs[id]
	if job == nil || job.namespace != f.namespace || job.receipt != receipt || job.cancelRequested {
		return forgeError(CodePrecondition, "queue.ack", "receipt is unknown or its lease was lost")
	}
	delete(f.store.receipts, receipt)
	job.status = "done"
	job.receipt = ""
	job.completedAt = f.now()
	f.bumpQueueCounterLocked(job.queue, true, false, false)
	return nil
}

func (f *Forge) Nack(ctx context.Context, receipt string, options NackOptions) error {
	if err := contextReady(ctx, "queue.nack"); err != nil {
		return err
	}
	if options.RetryIn < 0 {
		return forgeError(CodeInvalid, "queue.nack", "retry delay cannot be negative")
	}
	if f.mode == ModePostgres {
		return f.pgNack(ctx, receipt, options)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	id, ok := f.store.receipts[receipt]
	if !ok {
		return forgeError(CodePrecondition, "queue.nack", "receipt is unknown or its lease was lost")
	}
	job := f.store.jobs[id]
	if job == nil || job.namespace != f.namespace || job.receipt != receipt || job.status != "leased" || job.cancelRequested {
		return forgeError(CodePrecondition, "queue.nack", "receipt is unknown or its lease was lost")
	}
	delete(f.store.receipts, receipt)
	settledQueue := job.queue
	job.receipt = ""
	job.attempts++
	job.failureSummary = safeFailureSummary(options.FailureSummary, "handler failed")
	if job.attempts >= job.maxAttempts {
		f.releaseDedupLocked(job.id)
		job.deadAttempts += job.attempts
		job.deadLetteredAt = f.now()
		if len(job.queue) >= 4 && job.queue[len(job.queue)-4:] == ".dlq" {
			job.status = "dead"
			job.completedAt = f.now()
		} else {
			job.queue += ".dlq"
			job.status = "available"
			job.attempts = 0
			job.availableAt = f.now()
		}
		f.bumpQueueCounterLocked(settledQueue, true, true, false)
		return nil
	}
	job.status = "available"
	job.availableAt = f.now().Add(options.RetryIn)
	return nil
}

func (f *Forge) Heartbeat(ctx context.Context, receipt string, visibility time.Duration) error {
	if err := contextReady(ctx, "queue.heartbeat"); err != nil {
		return err
	}
	if visibility <= 0 {
		return forgeError(CodeInvalid, "queue.heartbeat", "visibility must be positive")
	}
	if f.mode == ModePostgres {
		return f.pgHeartbeat(ctx, receipt, visibility)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	id, ok := f.store.receipts[receipt]
	job := f.store.jobs[id]
	if !ok || job == nil || job.namespace != f.namespace || job.receipt != receipt || job.status != "leased" || job.cancelRequested || !f.now().Before(job.leasedUntil) {
		return forgeError(CodePrecondition, "queue.heartbeat", "receipt is unknown or its lease was lost")
	}
	job.leasedUntil = f.now().Add(visibility)
	return nil
}

func (f *Forge) CancelJob(ctx context.Context, jobID string) (*JobStatus, error) {
	if err := f.ready(ctx, "queue.cancel"); err != nil {
		return nil, err
	}
	if !looksLikeUUID(jobID) {
		return nil, forgeError(CodeInvalid, "queue.cancel", "job ID must be a UUID")
	}
	if f.mode == ModePostgres {
		return f.pgCancelJob(ctx, jobID)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	job := f.store.jobs[jobID]
	if job == nil || job.namespace != f.namespace {
		return nil, nil
	}
	if job.status == "available" {
		job.status = "cancelled"
		job.completedAt = f.now()
		job.receipt = ""
		f.bumpQueueCounterLocked(job.queue, true, false, true)
	} else if job.status == "leased" {
		job.cancelRequested = true
	}
	f.releaseDedupLocked(jobID)
	status := f.jobStatusLocked(job)
	return &status, nil
}

func (f *Forge) CancellationRequested(ctx context.Context, receipt string) (bool, error) {
	if err := contextReady(ctx, "queue.cancellation_requested"); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgCancellationRequested(ctx, receipt)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	id, ok := f.store.receipts[receipt]
	job := f.store.jobs[id]
	if !ok || job == nil || job.namespace != f.namespace || job.receipt != receipt {
		return false, forgeError(CodePrecondition, "queue.cancellation_requested", "receipt is unknown or its lease was lost")
	}
	return job.cancelRequested || job.status == "cancelled", nil
}

func (f *Forge) FinishCancellation(ctx context.Context, receipt string) error {
	if err := contextReady(ctx, "queue.finish_cancellation"); err != nil {
		return err
	}
	if f.mode == ModePostgres {
		return f.pgFinishCancellation(ctx, receipt)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	id, ok := f.store.receipts[receipt]
	job := f.store.jobs[id]
	if !ok || job == nil || job.namespace != f.namespace || job.receipt != receipt || job.status != "leased" || !job.cancelRequested {
		return forgeError(CodePrecondition, "queue.finish_cancellation", "cancellation fence was lost")
	}
	delete(f.store.receipts, receipt)
	job.receipt = ""
	job.status = "cancelled"
	job.completedAt = f.now()
	f.bumpQueueCounterLocked(job.queue, true, false, true)
	return nil
}

func (f *Forge) bumpQueueCounterLocked(queue string, settled, dead, cancelled bool) {
	key := f.scoped(queue)
	counter := f.store.queueCounters[key]
	if counter == nil {
		counter = &memoryQueueCounter{startedAt: f.now()}
		f.store.queueCounters[key] = counter
	}
	if settled {
		counter.settled++
	}
	if dead {
		counter.dead++
	}
	if cancelled {
		counter.cancelled++
	}
}

func (f *Forge) JobStatus(ctx context.Context, jobID string) (*JobStatus, error) {
	if err := f.ready(ctx, "queue.status"); err != nil {
		return nil, err
	}
	if !looksLikeUUID(jobID) {
		return nil, forgeError(CodeInvalid, "queue.status", "job ID must be a UUID")
	}
	if f.mode == ModePostgres {
		return f.pgJobStatus(ctx, jobID)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	job := f.store.jobs[jobID]
	if job == nil || job.namespace != f.namespace {
		return nil, nil
	}
	status := f.jobStatusLocked(job)
	return &status, nil
}

func (f *Forge) ListJobStatus(ctx context.Context, filter JobStatusFilter) (JobStatusPage, error) {
	if err := f.ready(ctx, "queue.list_status"); err != nil {
		return JobStatusPage{}, err
	}
	if filter.Limit == 0 {
		filter.Limit = 50
	}
	if filter.Limit > 100 {
		return JobStatusPage{}, forgeError(CodeInvalid, "queue.list_status", "limit must be in 1..=100")
	}
	if f.mode == ModePostgres {
		return f.pgListJobStatus(ctx, filter)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	page := JobStatusPage{Items: []JobStatus{}}
	after := filter.Cursor == ""
	for _, id := range f.store.jobOrder {
		if !after {
			after = id == filter.Cursor
			continue
		}
		job := f.store.jobs[id]
		if job == nil || job.namespace != f.namespace || (filter.Queue != "" && job.queue != filter.Queue) {
			continue
		}
		status := f.jobStatusLocked(job)
		if len(filter.States) > 0 && !containsJobState(filter.States, status.State) {
			continue
		}
		if len(page.Items) == int(filter.Limit) {
			cursor := page.Items[len(page.Items)-1].ID
			page.Cursor = &cursor
			break
		}
		page.Items = append(page.Items, status)
	}
	return page, nil
}

func containsJobState(states []JobState, state JobState) bool {
	for _, value := range states {
		if value == state {
			return true
		}
	}
	return false
}

func (f *Forge) jobStatusLocked(job *memoryJob) JobStatus {
	state := JobQueued
	switch {
	case job.status == "available" && job.attempts > 0:
		state = JobRetrying
	case job.status == "available" && f.now().Before(job.availableAt):
		state = JobDelayed
	case job.status == "leased" && job.cancelRequested:
		state = JobCancelRequested
	case job.status == "leased":
		state = JobLeased
	case job.status == "done":
		state = JobSucceeded
	case job.status == "dead":
		state = JobDead
	case job.status == "cancelled":
		state = JobCancelled
	}
	var key *string
	if job.concurrencyKey != "" {
		value := job.concurrencyKey
		key = &value
	}
	var completed *time.Time
	if !job.completedAt.IsZero() {
		value := job.completedAt
		completed = &value
	}
	return JobStatus{job.id, job.queue, state, job.attempts, job.maxAttempts, job.priority, key, job.enqueuedAt, job.availableAt, completed}
}

func (f *Forge) Depth(ctx context.Context, queue string) (QueueDepth, error) {
	if err := f.ready(ctx, "queue.depth"); err != nil {
		return QueueDepth{}, err
	}
	if queue == "" {
		return QueueDepth{}, forgeError(CodeInvalid, "queue.depth", "queue cannot be empty")
	}
	if f.mode == ModePostgres {
		return f.pgDepth(ctx, queue)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	now := f.now()
	var depth QueueDepth
	var oldest time.Time
	for _, job := range f.store.jobs {
		if job.namespace != f.namespace || job.queue != queue {
			continue
		}
		switch {
		case job.status == "leased" && now.Before(job.leasedUntil):
			depth.InFlight++
		case job.status == "available" && now.Before(job.availableAt):
			depth.Delayed++
		case job.status == "available" || job.status == "leased":
			depth.Visible++
			if oldest.IsZero() || job.enqueuedAt.Before(oldest) {
				oldest = job.enqueuedAt
			}
		}
	}
	if !oldest.IsZero() {
		age := float64(now.Sub(oldest).Milliseconds())
		depth.OldestVisibleAgeMs = &age
	}
	return depth, nil
}

func (f *Forge) PauseQueue(ctx context.Context, queue string) error {
	if err := f.ready(ctx, "queue.pause"); err != nil {
		return err
	}
	if queue == "" {
		return forgeError(CodeInvalid, "queue.pause", "queue cannot be empty")
	}
	if f.mode == ModePostgres {
		return f.pgPauseQueue(ctx, queue, true)
	}
	f.store.mu.Lock()
	f.store.queuePaused[f.scoped(queue)] = true
	f.store.mu.Unlock()
	return nil
}

func (f *Forge) ResumeQueue(ctx context.Context, queue string) error {
	if err := f.ready(ctx, "queue.resume"); err != nil {
		return err
	}
	if queue == "" {
		return forgeError(CodeInvalid, "queue.resume", "queue cannot be empty")
	}
	if f.mode == ModePostgres {
		return f.pgPauseQueue(ctx, queue, false)
	}
	f.store.mu.Lock()
	delete(f.store.queuePaused, f.scoped(queue))
	f.store.mu.Unlock()
	return nil
}

func (f *Forge) QueuePaused(ctx context.Context, queue string) (bool, error) {
	if err := f.ready(ctx, "queue.is_paused"); err != nil {
		return false, err
	}
	if queue == "" {
		return false, forgeError(CodeInvalid, "queue.is_paused", "queue cannot be empty")
	}
	if f.mode == ModePostgres {
		return f.pgQueuePaused(ctx, queue)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	return f.store.queuePaused[f.scoped(queue)], nil
}

func (f *Forge) QueueStats(ctx context.Context, queue string) (QueueStats, error) {
	if err := f.ready(ctx, "queue.stats"); err != nil {
		return QueueStats{}, err
	}
	if queue == "" {
		return QueueStats{}, forgeError(CodeInvalid, "queue.stats", "queue cannot be empty")
	}
	if f.mode == ModePostgres {
		return f.pgQueueStats(ctx, queue)
	}
	depth, err := f.Depth(ctx, queue)
	if err != nil {
		return QueueStats{}, err
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	counter := f.store.queueCounters[f.scoped(queue)]
	stats := QueueStats{OldestVisibleAgeMs: depth.OldestVisibleAgeMs, Paused: f.store.queuePaused[f.scoped(queue)]}
	if counter == nil {
		return stats, nil
	}
	minutes := f.now().Sub(counter.startedAt).Seconds() / 60
	if minutes < 1.0/60.0 {
		minutes = 1.0 / 60.0
	}
	stats.EnqueuedTotal = counter.enqueued
	stats.SettledTotal = counter.settled
	stats.DeadTotal = counter.dead
	stats.CancelledTotal = counter.cancelled
	stats.EnqueueRatePerMinute = float64(counter.enqueued) / minutes
	stats.SettleRatePerMinute = float64(counter.settled) / minutes
	return stats, nil
}

func (f *Forge) releaseDedupLocked(jobID string) {
	for key, reservation := range f.store.dedup {
		if reservation.jobID == jobID {
			delete(f.store.dedup, key)
		}
	}
}

func safeFailureSummary(value, fallback string) string {
	if value == "" {
		value = fallback
	}
	bytes := []byte(value)
	if len(bytes) <= 512 {
		return value
	}
	bytes = bytes[:512]
	for len(bytes) > 0 && !utf8.Valid(bytes) {
		bytes = bytes[:len(bytes)-1]
	}
	return string(bytes)
}

func (f *Forge) DeadLetters(ctx context.Context, queue string, cursor *string, limit uint32) (DeadLetterPage, error) {
	if err := f.ready(ctx, "queue.dead_letters"); err != nil {
		return DeadLetterPage{}, err
	}
	if queue == "" || strings.HasSuffix(queue, ".dlq") || limit == 0 || limit > 100 {
		return DeadLetterPage{}, forgeError(CodeInvalid, "queue.dead_letters", "source queue and limit in 1..=100 are required")
	}
	if f.mode == ModePostgres {
		return f.pgDeadLetters(ctx, queue, cursor, limit)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	ids := make([]string, 0)
	for id, job := range f.store.jobs {
		if job.namespace == f.namespace && job.queue == queue+".dlq" && (job.status == "available" || job.status == "dead") && (cursor == nil || id > *cursor) {
			ids = append(ids, id)
		}
	}
	sort.Strings(ids)
	more := len(ids) > int(limit)
	if more {
		ids = ids[:limit]
	}
	page := DeadLetterPage{Items: make([]DeadLetterInfo, 0, len(ids))}
	for _, id := range ids {
		job := f.store.jobs[id]
		var summary *string
		if job.failureSummary != "" {
			value := job.failureSummary
			summary = &value
		}
		deadAt := job.deadLetteredAt
		if deadAt.IsZero() {
			deadAt = job.enqueuedAt
		}
		page.Items = append(page.Items, DeadLetterInfo{JobID: id, Queue: queue, AttemptCount: job.deadAttempts, EnqueuedAtMs: float64(job.enqueuedAt.UnixMilli()), DeadLetteredAtMs: float64(deadAt.UnixMilli()), FailureSummary: summary})
	}
	if more && len(ids) > 0 {
		value := ids[len(ids)-1]
		page.Cursor = &value
	}
	return page, nil
}

func validateRedrive(options RedriveOptions, operation string) error {
	if options.Destination == "" || strings.HasSuffix(options.Destination, ".dlq") {
		return forgeError(CodeInvalid, operation, "an explicit non-DLQ destination is required")
	}
	if options.DedupPolicy != "clear" && options.DedupPolicy != "preserve" {
		return forgeError(CodeInvalid, operation, "dedup policy must be clear or preserve")
	}
	return nil
}

func (f *Forge) Redrive(ctx context.Context, jobID string, options RedriveOptions) (bool, error) {
	if err := f.ready(ctx, "queue.redrive"); err != nil {
		return false, err
	}
	if !looksLikeUUID(jobID) {
		return false, forgeError(CodeInvalid, "queue.redrive", "job ID must be a UUID")
	}
	if err := validateRedrive(options, "queue.redrive"); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgRedrive(ctx, jobID, options)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	job := f.store.jobs[jobID]
	if job == nil || job.namespace != f.namespace || !strings.HasSuffix(job.queue, ".dlq") || (job.status != "available" && job.status != "dead") {
		return false, nil
	}
	if !job.payloadRetained {
		return false, forgeError(CodePrecondition, "queue.redrive", "dead-letter payload retention elapsed; the job cannot be redriven")
	}
	job.queue = options.Destination
	job.status = "available"
	job.attempts = 0
	job.availableAt = f.now()
	job.completedAt = time.Time{}
	job.deadLetteredAt = time.Time{}
	job.deadAttempts = 0
	job.failureSummary = ""
	if options.DedupPolicy == "clear" {
		f.releaseDedupLocked(jobID)
	}
	return true, nil
}

func (f *Forge) RedriveBatch(ctx context.Context, queue string, cursor *string, limit uint32, options RedriveOptions) (RedriveBatchResult, error) {
	if err := validateRedrive(options, "queue.redrive_batch"); err != nil {
		return RedriveBatchResult{}, err
	}
	page, err := f.DeadLetters(ctx, queue, cursor, limit)
	if err != nil {
		return RedriveBatchResult{}, err
	}
	var count uint32
	for _, item := range page.Items {
		ok, err := f.Redrive(ctx, item.JobID, options)
		if err != nil {
			return RedriveBatchResult{}, err
		}
		if ok {
			count++
		}
	}
	return RedriveBatchResult{Redriven: count, Cursor: page.Cursor}, nil
}

func (f *Forge) PurgeDeadLettersDryRun(ctx context.Context, queue string) (uint64, error) {
	pageCount := uint64(0)
	if err := f.ready(ctx, "queue.purge_dead_letters_dry_run"); err != nil {
		return 0, err
	}
	if queue == "" || strings.HasSuffix(queue, ".dlq") {
		return 0, forgeError(CodeInvalid, "queue.purge_dead_letters_dry_run", "source queue is required")
	}
	if f.mode == ModePostgres {
		return f.pgPurgeDeadLettersDryRun(ctx, queue)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	for _, job := range f.store.jobs {
		if job.namespace == f.namespace && job.queue == queue+".dlq" && (job.status == "available" || job.status == "dead") {
			pageCount++
		}
	}
	return pageCount, nil
}

func (f *Forge) PurgeDeadLetters(ctx context.Context, queue, confirmation string) (uint64, error) {
	if confirmation != queue {
		return 0, forgeError(CodePrecondition, "queue.purge_dead_letters", "confirmation must exactly match the source queue")
	}
	if _, err := f.PurgeDeadLettersDryRun(ctx, queue); err != nil {
		return 0, err
	}
	if f.mode == ModePostgres {
		return f.pgPurgeDeadLetters(ctx, queue)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	ids := make([]string, 0)
	for id, job := range f.store.jobs {
		if job.namespace == f.namespace && job.queue == queue+".dlq" && (job.status == "available" || job.status == "dead") {
			ids = append(ids, id)
		}
	}
	for _, id := range ids {
		delete(f.store.jobs, id)
		f.releaseDedupLocked(id)
	}
	return uint64(len(ids)), nil
}

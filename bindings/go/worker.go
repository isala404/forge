package forge

import (
	"context"
	"sync"
	"time"
)

type WorkerOptions struct {
	Concurrency            uint32
	Visibility             time.Duration
	HeartbeatCadence       time.Duration
	RetryBackoff           time.Duration
	DrainDeadline          time.Duration
	Identity               string
	OnError                func(error)
	ConcurrencyLimitPerKey uint32
}

type JobHandler func(context.Context, Job) error

type WorkerState string

const (
	WorkerStatePolling      WorkerState = "polling"
	WorkerStateHandling     WorkerState = "handling"
	WorkerStateHeartbeating WorkerState = "heartbeating"
	WorkerStateSettling     WorkerState = "settling"
	WorkerStateDraining     WorkerState = "draining"
	WorkerStateStopped      WorkerState = "stopped"
)

// WorkerFailure adds low-cardinality worker context without changing the wrapped
// Forge error code or retryability.
type WorkerFailure struct {
	Identity string
	State    WorkerState
	Err      error
}

func (failure *WorkerFailure) Error() string { return failure.Err.Error() }
func (failure *WorkerFailure) Unwrap() error { return failure.Err }

// RunWorker processes jobs until the context is cancelled or Forge closes. Shutdown stops
// dequeuing, lets handlers finish within DrainDeadline, then cancels their contexts and
// releases any remaining leases. Handlers must honor their context for bounded shutdown.
func (f *Forge) RunWorker(ctx context.Context, queue string, handler JobHandler, options WorkerOptions) error {
	f.workerMu.Lock()
	if err := f.ready(ctx, "worker.run"); err != nil {
		f.workerMu.Unlock()
		return err
	}
	f.workers.Add(1)
	f.workerMu.Unlock()
	defer f.workers.Done()
	f.activeWorkers.Add(1)
	defer f.activeWorkers.Add(-1)
	pollCtx, stopPolling := context.WithCancel(ctx)
	defer stopPolling()
	handlerCtx, stopHandlers := context.WithCancel(ctx)
	defer stopHandlers()
	go func() {
		select {
		case <-f.shutdown:
			stopPolling()
		case <-pollCtx.Done():
		}
	}()
	if handler == nil || queue == "" {
		return forgeError(CodeInvalid, "worker.run", "queue and handler are required")
	}
	if options.Concurrency == 0 {
		options.Concurrency = 1
	}
	if options.Visibility == 0 {
		options.Visibility = 30 * time.Second
	}
	if options.HeartbeatCadence == 0 {
		options.HeartbeatCadence = options.Visibility / 3
	}
	if options.RetryBackoff == 0 {
		options.RetryBackoff = time.Second
	}
	if options.DrainDeadline == 0 {
		options.DrainDeadline = 30 * time.Second
	}
	if options.Identity == "" {
		options.Identity = "worker"
	}
	if options.HeartbeatCadence <= 0 || options.HeartbeatCadence >= options.Visibility {
		return forgeError(CodeInvalid, "worker.run", "heartbeat cadence must be positive and shorter than visibility")
	}
	if options.RetryBackoff < 0 || options.RetryBackoff > 30*time.Second || options.DrainDeadline < 0 {
		return forgeError(CodeInvalid, "worker.run", "worker retry and drain options exceed their bounds")
	}

	slots := make(chan struct{}, options.Concurrency)
	var active sync.WaitGroup
	retryAttempt := uint32(0)
	for {
		select {
		case <-pollCtx.Done():
			return drainWorker(&active, options.DrainDeadline, stopHandlers)
		case slots <- struct{}{}:
		}
		job, err := f.Dequeue(pollCtx, queue, DequeueOptions{Visibility: options.Visibility, Wait: time.Second, ConcurrencyLimitPerKey: options.ConcurrencyLimitPerKey})
		if err != nil {
			<-slots
			if pollCtx.Err() != nil {
				return drainWorker(&active, options.DrainDeadline, stopHandlers)
			}
			reportWorkerError(options, WorkerStatePolling, err)
			if !IsRetryable(err) {
				return err
			}
			retryAttempt++
			if !waitContext(pollCtx, jitterRetry(options.RetryBackoff, retryAttempt)) {
				return drainWorker(&active, options.DrainDeadline, stopHandlers)
			}
			continue
		}
		retryAttempt = 0
		if job == nil {
			<-slots
			continue
		}
		if pollCtx.Err() != nil {
			<-slots
			_ = f.Nack(context.Background(), job.Receipt, NackOptions{FailureSummary: "worker stopped before handler start"})
			return drainWorker(&active, options.DrainDeadline, stopHandlers)
		}
		active.Add(1)
		go func(job Job) {
			defer active.Done()
			defer func() { <-slots }()
			f.processJob(handlerCtx, job, handler, options)
		}(*job)
	}
}

func (f *Forge) processJob(ctx context.Context, job Job, handler JobHandler, options WorkerOptions) {
	handlerCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	handlerDone := make(chan error, 1)
	go func() {
		handlerDone <- handler(handlerCtx, job)
	}()
	ticker := time.NewTicker(options.HeartbeatCadence)
	defer ticker.Stop()
	applicationCancelled := false
	for {
		select {
		case err := <-handlerDone:
			if applicationCancelled {
				if finishErr := f.FinishCancellation(context.Background(), job.Receipt); finishErr != nil {
					reportWorkerError(options, WorkerStateSettling, finishErr)
				}
				return
			}
			if err == nil {
				if ackErr := f.Ack(context.Background(), job.Receipt); ackErr != nil {
					reportWorkerError(options, WorkerStateSettling, ackErr)
				}
				return
			}
			reportWorkerError(options, WorkerStateHandling, err)
			if nackErr := f.Nack(context.Background(), job.Receipt, NackOptions{RetryIn: options.RetryBackoff, FailureSummary: "handler returned an error"}); nackErr != nil {
				reportWorkerError(options, WorkerStateSettling, nackErr)
			}
			return
		case <-ticker.C:
			if applicationCancelled {
				continue
			}
			requested, checkErr := f.CancellationRequested(context.Background(), job.Receipt)
			if checkErr != nil {
				reportWorkerError(options, WorkerStateHeartbeating, checkErr)
				continue
			}
			if requested {
				applicationCancelled = true
				cancel()
				continue
			}
			if err := f.Heartbeat(context.Background(), job.Receipt, options.Visibility); err != nil {
				cancel()
				reportWorkerError(options, WorkerStateHeartbeating, err)
				return
			}
		case <-ctx.Done():
			cancel()
			if err := f.Nack(context.Background(), job.Receipt, NackOptions{FailureSummary: "worker shutdown interrupted the handler"}); err != nil {
				reportWorkerError(options, WorkerStateSettling, err)
			}
			return
		}
	}
}

func drainWorker(active *sync.WaitGroup, deadline time.Duration, cancel context.CancelFunc) error {
	done := make(chan struct{})
	go func() {
		active.Wait()
		close(done)
	}()
	timer := time.NewTimer(deadline)
	defer timer.Stop()
	select {
	case <-done:
		return nil
	case <-timer.C:
		cancel()
		<-done
		return forgeError(CodeUnavailable, "worker.close", "worker drain deadline expired")
	}
}

func reportWorkerError(options WorkerOptions, state WorkerState, err error) {
	if options.OnError != nil && err != nil {
		options.OnError(&WorkerFailure{Identity: options.Identity, State: state, Err: err})
	}
}

func waitContext(ctx context.Context, duration time.Duration) bool {
	timer := time.NewTimer(duration)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

func jitterRetry(base time.Duration, attempt uint32) time.Duration {
	if base <= 0 {
		return 0
	}
	multiplier := time.Duration(1 << min(attempt, 5))
	capped := base * multiplier
	if capped > 30*time.Second {
		capped = 30 * time.Second
	}
	spread := 80 + (time.Now().UnixNano()+int64(attempt)*37)%41
	if spread < 80 {
		spread = 80
	}
	return capped * time.Duration(spread) / 100
}

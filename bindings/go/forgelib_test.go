package forge

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestPostgres18IsTheOnlySupportedFloor(t *testing.T) {
	if supportedPostgresVersion(179999) || !supportedPostgresVersion(180000) || !supportedPostgresVersion(180001) || supportedPostgresVersion(190000) {
		t.Fatal("only PostgreSQL 18 should be supported")
	}
}

func TestHealthMetricsAndTraceContextArePerInstance(t *testing.T) {
	first, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	second, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	if !first.IsLive() {
		t.Fatal("new Forge must be live")
	}
	if len(first.BackendCapabilities()) != len(allPrimitives) {
		t.Fatal("backend capability inventory is incomplete")
	}
	channel, err := first.PubsubChannel(context.Background(), "updates")
	if err != nil || channel == "" {
		t.Fatalf("pubsub channel was not resolved: channel=%q err=%v", channel, err)
	}
	report, err := first.Probe(context.Background(), ProbeOptions{Deadline: time.Second, ReadinessBackends: []Primitive{PrimitiveKV, PrimitiveQueue}})
	if err != nil || !report.Live || !report.Ready || len(report.Backends) != len(allPrimitives) {
		t.Fatalf("unexpected health report: report=%+v err=%v", report, err)
	}
	if _, err := first.KVGet(context.Background(), "private-user-key"); err != nil {
		t.Fatal(err)
	}
	metrics := first.RenderPrometheus()
	if !strings.Contains(metrics, "forge_operations_total") || strings.Contains(metrics, "private-user-key") {
		t.Fatalf("metrics missing operation or leaked a raw key: %q", metrics)
	}
	if strings.Contains(second.RenderPrometheus(), "kv.get") {
		t.Fatal("metrics leaked between Forge instances")
	}
	if len(first.MetricsSnapshot()) == 0 {
		t.Fatal("metrics snapshot did not include the recorded operation")
	}

	trace, err := NewTraceContext("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01", "vendor=value", "tenant=acme,secret=drop", []string{"tenant"})
	if err != nil || trace.Baggage != "tenant=acme" {
		t.Fatalf("unexpected trace context: context=%+v err=%v", trace, err)
	}
	if _, err := first.Enqueue(context.Background(), "traced", []byte("body"), EnqueueOptions{TraceContext: &trace}); err != nil {
		t.Fatal(err)
	}
	job, err := first.Dequeue(context.Background(), "traced", DequeueOptions{Visibility: time.Second})
	if err != nil || job == nil || job.Traceparent == nil || *job.Traceparent != trace.Traceparent || job.Baggage == nil || *job.Baggage != "tenant=acme" {
		t.Fatalf("trace context did not round trip: job=%+v err=%v", job, err)
	}
}

func TestMemoryBulkKVExpiryAndAuthRevocation(t *testing.T) {
	clock := NewManualClock(time.Unix(1_700_000_000, 0))
	forge, err := NewMemoryForTesting(
		Config{Environment: EnvironmentTest},
		TestOptions{ManualClock: clock, Random: NewSeededReader(22)},
	)
	if err != nil {
		t.Fatal(err)
	}
	defer forge.Close(context.Background())
	ctx := context.Background()

	if _, err := forge.KVSet(ctx, "first", []byte("one"), SetOptions{}); err != nil {
		t.Fatal(err)
	}
	if _, err := forge.KVSet(ctx, "second", []byte("two"), SetOptions{}); err != nil {
		t.Fatal(err)
	}
	values, err := forge.KVMGet(ctx, []string{"second", "missing", "first"})
	if err != nil || string(values[0]) != "two" || values[1] != nil || string(values[2]) != "one" {
		t.Fatalf("unexpected bulk values: values=%q err=%v", values, err)
	}
	if changed, err := forge.KVExpire(ctx, "first", time.Second); err != nil || !changed {
		t.Fatalf("expiry was not applied: changed=%t err=%v", changed, err)
	}
	clock.Advance(time.Second)
	if value, err := forge.KVGet(ctx, "first"); err != nil || value != nil {
		t.Fatalf("expired key remained visible: value=%q err=%v", value, err)
	}

	firstSession, err := forge.CreateSession(ctx, "owner", SessionOptions{})
	if err != nil {
		t.Fatal(err)
	}
	secondSession, err := forge.CreateSession(ctx, "owner", SessionOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if count, err := forge.RevokeAllSessions(ctx, "owner"); err != nil || count != 2 {
		t.Fatalf("unexpected revoke-all result: count=%d err=%v", count, err)
	}
	for _, token := range []string{firstSession, secondSession} {
		if session, err := forge.ValidateSession(ctx, token); err != nil || session != nil {
			t.Fatalf("revoked session remained valid: session=%+v err=%v", session, err)
		}
	}
	key, err := forge.CreateAPIKey(ctx, "owner", "test")
	if err != nil {
		t.Fatal(err)
	}
	if revoked, err := forge.RevokeAPIKey(ctx, key.ID); err != nil || !revoked {
		t.Fatalf("API key was not revoked: revoked=%t err=%v", revoked, err)
	}
	if info, err := forge.VerifyAPIKey(ctx, key.Secret); err != nil || info != nil {
		t.Fatalf("revoked API key remained valid: info=%+v err=%v", info, err)
	}
}

func TestMemoryProductionGate(t *testing.T) {
	_, err := NewMemory(Config{Environment: EnvironmentProduction})
	if ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expected config error, got %v", err)
	}
}

func TestBulkConfigAndBoundedSnapshot(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	defer forge.Close(context.Background())
	ctx := context.Background()
	if err := forge.ConfigSet(ctx, "color", []byte("blue")); err != nil {
		t.Fatal(err)
	}
	if err := forge.SetFlag(ctx, "theme", FlagRule{Kind: FlagValue, ValueJSON: `"dark"`, Variant: "theme-v1"}); err != nil {
		t.Fatal(err)
	}
	values, err := forge.ConfigGetMany(ctx, []string{"missing", "color", "color"})
	if err != nil || values[0].Value != nil || values[1].Value == nil || *values[1].Value != "blue" || values[2].Value == nil || *values[2].Value != "blue" {
		t.Fatalf("unexpected ordered bulk values: values=%+v err=%v", values, err)
	}
	contextJSON := `{"tenant":"acme"}`
	requests := []FlagEvaluationRequest{{ID: "theme-for-user", Key: "theme", DefaultJSON: `"light"`, ContextJSON: &contextJSON}}
	evaluations, err := forge.FlagDetailsMany(ctx, requests)
	if err != nil || len(evaluations) != 1 || evaluations[0].Evaluation.ValueJSON != `"dark"` || evaluations[0].Evaluation.Variant == nil || *evaluations[0].Evaluation.Variant != "theme-v1" {
		t.Fatalf("unexpected bulk flag details: values=%+v err=%v", evaluations, err)
	}
	snapshot, err := forge.ConfigSnapshot(ctx, []string{"color"}, requests, time.Minute, "no_secrets")
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := forge.EncodeConfigSnapshot(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := forge.DecodeConfigSnapshot(encoded)
	if err != nil {
		t.Fatal(err)
	}
	now := time.UnixMilli(int64(decoded.CreatedAtMs))
	value, err := decoded.ConfigGet("color", now)
	if err != nil || value == nil || *value != "blue" {
		t.Fatalf("snapshot config lookup failed: value=%v err=%v", value, err)
	}
	details, err := decoded.FlagDetails("theme-for-user", now)
	if err != nil || details.ValueJSON != `"dark"` {
		t.Fatalf("snapshot flag lookup failed: details=%+v err=%v", details, err)
	}
	if _, err := decoded.ConfigGet("not-captured", now); ErrorCodeOf(err) != CodeInvalid {
		t.Fatalf("expected out-of-scope snapshot error, got %v", err)
	}
	if _, err := decoded.ConfigGet("color", time.UnixMilli(int64(decoded.ExpiresAtMs)+1)); ErrorCodeOf(err) != CodePrecondition {
		t.Fatalf("expected stale snapshot error, got %v", err)
	}
}

func TestMemoryRejectsPostgresConfigurationBeforeConnecting(t *testing.T) {
	_, err := NewMemory(Config{
		Environment: EnvironmentTest,
		PostgresURL: "postgres://127.0.0.1:1/should-not-connect",
	})
	if ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expected config error, got %v", err)
	}
}

func TestMemoryRejectsS3ConfigurationBeforeConnecting(t *testing.T) {
	_, err := NewMemory(Config{
		Environment: EnvironmentTest,
		BlobBackend: "s3",
		S3:          &S3Config{Bucket: "unused"},
	})
	if ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expected config error, got %v", err)
	}
}

func TestMemoryBlobConditionalCopyAndChecksumCompose(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	defer forge.Close(context.Background())

	const checksum = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
	options := PutOptions{
		ContentType:        "text/plain",
		Metadata:           map[string]string{"purpose": "test"},
		CacheControl:       "public, max-age=60",
		ContentDisposition: `attachment; filename="hello.txt"`,
		ChecksumSHA256:     checksum,
	}
	if err := forge.BlobPut(context.Background(), "source", []byte("hello"), options); err != nil {
		t.Fatal(err)
	}
	info, err := forge.BlobHead(context.Background(), "source")
	if err != nil || info == nil || info.ChecksumSha256 == nil || *info.ChecksumSha256 != checksum {
		t.Fatalf("unexpected blob metadata: info=%+v err=%v", info, err)
	}
	found, err := forge.BlobGetIf(context.Background(), "source", &info.ETag, nil)
	if err != nil || found.State != "found" || found.Body == nil || string(*found.Body) != "hello" {
		t.Fatalf("conditional read failed: result=%+v err=%v", found, err)
	}
	notModified, err := forge.BlobGetIf(context.Background(), "source", nil, &info.ETag)
	if err != nil || notModified.State != "not_modified" || notModified.Body != nil {
		t.Fatalf("not-modified read failed: result=%+v err=%v", notModified, err)
	}
	wrong := "wrong"
	if _, err := forge.BlobGetIf(context.Background(), "source", &wrong, nil); ErrorCodeOf(err) != CodePrecondition {
		t.Fatalf("expected conditional mismatch, got %v", err)
	}
	copied, err := forge.BlobCopy(context.Background(), "source", "copy", PutOptions{})
	if err != nil || copied.CacheControl == nil || *copied.CacheControl != options.CacheControl || copied.ContentDisposition == nil || *copied.ContentDisposition != options.ContentDisposition {
		t.Fatalf("copy did not preserve headers: info=%+v err=%v", copied, err)
	}
	verified, err := forge.BlobVerifyChecksumSHA256(context.Background(), "copy", checksum)
	if err != nil || !verified {
		t.Fatalf("checksum verification failed: verified=%t err=%v", verified, err)
	}
	if _, err := forge.BlobCreateMultipart(context.Background(), "large", PutOptions{}); ErrorCodeOf(err) != CodeNotConfigured {
		t.Fatalf("memory multipart should be unavailable, got %v", err)
	}
}

func TestConfigRejectsUnknownFeatureDatabaseBeforeConnecting(t *testing.T) {
	_, err := InitFromString(context.Background(), "[postgres]\nurl = \"postgres://127.0.0.1:1/unused\"\n[databases.unknown]\nurl = \"postgres://127.0.0.1:1/unused\"\n")
	if ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expected config error, got %v", err)
	}
}

func TestCloseIsIdempotentAndRejectsWork(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	if err := forge.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := forge.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := forge.KVGet(context.Background(), "key"); ErrorCodeOf(err) != CodePrecondition {
		t.Fatalf("expected precondition error, got %v", err)
	}
}

func TestCloseStopsOutboxRelay(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() {
		done <- forge.RunOutboxRelay(context.Background(), OutboxRelayOptions{FailureBackoff: time.Millisecond}, nil)
	}()
	time.Sleep(5 * time.Millisecond)
	if err := forge.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("outbox relay did not stop with Forge")
	}
}

func TestCloseCanRetryAfterWorkerDrainDeadline(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := forge.Enqueue(context.Background(), "work", []byte("payload"), EnqueueOptions{}); err != nil {
		t.Fatal(err)
	}
	started := make(chan struct{})
	workerDone := make(chan error, 1)
	go func() {
		workerDone <- forge.RunWorker(context.Background(), "work", func(_ context.Context, _ Job) error {
			close(started)
			time.Sleep(75 * time.Millisecond)
			return nil
		}, WorkerOptions{Visibility: time.Second, HeartbeatCadence: 100 * time.Millisecond, DrainDeadline: time.Second})
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("worker did not start")
	}
	short, cancel := context.WithTimeout(context.Background(), time.Millisecond)
	defer cancel()
	if err := forge.Close(short); ErrorCodeOf(err) != CodeUnavailable {
		t.Fatalf("expected first close to reach its deadline, got %v", err)
	}
	if err := forge.Close(context.Background()); err != nil {
		t.Fatalf("retry did not finish cleanup: %v", err)
	}
	if err := <-workerDone; err != nil {
		t.Fatalf("worker did not drain cleanly: %v", err)
	}
}

func TestCancelledContextIsUnavailable(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := forge.KVGet(ctx, "key"); ErrorCodeOf(err) != CodeUnavailable || !IsRetryable(err) {
		t.Fatalf("expected retryable unavailable error, got %v", err)
	}
}

func TestConfigEnvironmentOverridesAreCapturedAtConstruction(t *testing.T) {
	t.Setenv("FORGE_CFG_MODE", "before")
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("FORGE_CFG_MODE", "after")
	value, err := forge.ConfigGet(context.Background(), "mode")
	if err != nil || string(value) != "before" {
		t.Fatalf("running Forge must retain its environment snapshot: value=%q err=%v", value, err)
	}
	second, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	value, err = second.ConfigGet(context.Background(), "mode")
	if err != nil || string(value) != "after" {
		t.Fatalf("new Forge must capture the current environment: value=%q err=%v", value, err)
	}
}

func TestDeterministicEnqueueIsConcurrentSafe(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	const producers = 32
	const deterministicID = "11111111-1111-4111-8111-111111111111"
	ids := make(chan string, producers)
	errs := make(chan error, producers)
	var group sync.WaitGroup
	for range producers {
		group.Add(1)
		go func() {
			defer group.Done()
			id, enqueueErr := forge.Enqueue(context.Background(), "email", []byte("same"), EnqueueOptions{ID: deterministicID})
			ids <- id
			errs <- enqueueErr
		}()
	}
	group.Wait()
	close(ids)
	close(errs)
	for enqueueErr := range errs {
		if enqueueErr != nil {
			t.Fatal(enqueueErr)
		}
	}
	for id := range ids {
		if id != deterministicID {
			t.Fatalf("unexpected effective ID %q", id)
		}
	}
	depth, err := forge.Depth(context.Background(), "email")
	if err != nil {
		t.Fatal(err)
	}
	if depth.Visible != 1 {
		t.Fatalf("expected one job, got %+v", depth)
	}
}

func TestDeadLetterOperatorsReleaseDedupAndRedrive(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	const jobID = "11111111-1111-4111-8111-111111111111"
	effective, err := forge.Enqueue(context.Background(), "operator", []byte("one"), EnqueueOptions{ID: jobID, DedupID: "content", MaxAttempts: 1})
	if err != nil || effective != jobID {
		t.Fatalf("unexpected deterministic enqueue: id=%q err=%v", effective, err)
	}
	job, err := forge.Dequeue(context.Background(), "operator", DequeueOptions{Visibility: time.Second})
	if err != nil {
		t.Fatal(err)
	}
	if err := forge.Nack(context.Background(), job.Receipt, NackOptions{RetryIn: 0, FailureSummary: "safe failure"}); err != nil {
		t.Fatal(err)
	}
	replacement, err := forge.Enqueue(context.Background(), "operator", []byte("two"), EnqueueOptions{DedupID: "content"})
	if err != nil || replacement == jobID {
		t.Fatalf("terminal job retained its dedup reservation: id=%q err=%v", replacement, err)
	}
	page, err := forge.DeadLetters(context.Background(), "operator", nil, 10)
	if err != nil || len(page.Items) != 1 || page.Items[0].JobID != jobID || page.Items[0].FailureSummary == nil || *page.Items[0].FailureSummary != "safe failure" {
		t.Fatalf("unexpected dead-letter page: page=%+v err=%v", page, err)
	}
	ok, err := forge.Redrive(context.Background(), jobID, RedriveOptions{Destination: "recovered", DedupPolicy: "clear"})
	if err != nil || !ok {
		t.Fatalf("redrive failed: ok=%v err=%v", ok, err)
	}
	redriven, err := forge.Dequeue(context.Background(), "recovered", DequeueOptions{Visibility: time.Second})
	if err != nil || redriven == nil || redriven.ID != jobID {
		t.Fatalf("unexpected redriven job: job=%+v err=%v", redriven, err)
	}

	for _, payload := range []string{"batch-one", "batch-two"} {
		if _, err := forge.Enqueue(context.Background(), "batch", []byte(payload), EnqueueOptions{MaxAttempts: 1}); err != nil {
			t.Fatal(err)
		}
		job, err := forge.Dequeue(context.Background(), "batch", DequeueOptions{Visibility: time.Second})
		if err != nil || job == nil {
			t.Fatalf("batch dead-letter dequeue failed: job=%+v err=%v", job, err)
		}
		if err := forge.Nack(context.Background(), job.Receipt, NackOptions{FailureSummary: "safe"}); err != nil {
			t.Fatal(err)
		}
	}
	statuses, err := forge.ListJobStatus(context.Background(), JobStatusFilter{Queue: "batch.dlq", Limit: 10})
	if err != nil || len(statuses.Items) != 2 {
		t.Fatalf("unexpected batch status page: page=%+v err=%v", statuses, err)
	}
	for _, status := range statuses.Items {
		if status.State != JobQueued {
			t.Fatalf("dead-letter entry had state %s, want queued", status.State)
		}
	}
	result, err := forge.RedriveBatch(context.Background(), "batch", nil, 10, RedriveOptions{Destination: "batch-recovered", DedupPolicy: "clear"})
	if err != nil || result.Redriven != 2 {
		t.Fatalf("batch redrive failed: result=%+v err=%v", result, err)
	}
	depth, err := forge.Depth(context.Background(), "batch-recovered")
	if err != nil || depth.Visible != 2 {
		t.Fatalf("batch redrive destination mismatch: depth=%+v err=%v", depth, err)
	}
}

func TestWorkerHonorsBoundedConcurrency(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	for range 3 {
		if _, err := forge.Enqueue(context.Background(), "parallel", []byte("job"), EnqueueOptions{}); err != nil {
			t.Fatal(err)
		}
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var active atomic.Int32
	var peak atomic.Int32
	started := make(chan struct{}, 3)
	release := make(chan struct{})
	done := make(chan error, 1)
	go func() {
		done <- forge.RunWorker(ctx, "parallel", func(_ context.Context, _ Job) error {
			current := active.Add(1)
			for {
				previous := peak.Load()
				if current <= previous || peak.CompareAndSwap(previous, current) {
					break
				}
			}
			started <- struct{}{}
			<-release
			active.Add(-1)
			return nil
		}, WorkerOptions{Concurrency: 3, Visibility: time.Second, HeartbeatCadence: 100 * time.Millisecond})
	}()
	for range 3 {
		select {
		case <-started:
		case <-time.After(time.Second):
			t.Fatal("three handlers did not start")
		}
	}
	if peak.Load() != 3 {
		t.Fatalf("expected concurrency 3, got %d", peak.Load())
	}
	close(release)
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func TestWorkerCancellationReleasesLeasedJob(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := forge.Enqueue(context.Background(), "work", []byte("payload"), EnqueueOptions{}); err != nil {
		t.Fatal(err)
	}
	started := make(chan struct{})
	ctx, cancel := context.WithCancel(context.Background())
	workerDone := make(chan error, 1)
	go func() {
		workerDone <- forge.RunWorker(ctx, "work", func(handlerCtx context.Context, job Job) error {
			close(started)
			<-handlerCtx.Done()
			return handlerCtx.Err()
		}, WorkerOptions{Visibility: time.Second, HeartbeatCadence: 100 * time.Millisecond, DrainDeadline: time.Second})
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("worker did not start")
	}
	cancel()
	if err := <-workerDone; err != nil {
		t.Fatal(err)
	}
	depth, err := forge.Depth(context.Background(), "work")
	if err != nil {
		t.Fatal(err)
	}
	if depth.Visible != 1 || depth.InFlight != 0 {
		t.Fatalf("worker did not release the lease: %+v", depth)
	}
}

func TestWorkerReportsHandlerErrorAndRetries(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := forge.Enqueue(context.Background(), "work", []byte("payload"), EnqueueOptions{MaxAttempts: 2}); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var attempts atomic.Uint32
	reported := make(chan error, 2)
	done := make(chan error, 1)
	go func() {
		done <- forge.RunWorker(ctx, "work", func(_ context.Context, _ Job) error {
			if attempts.Add(1) == 1 {
				return errors.New("first attempt")
			}
			cancel()
			return nil
		}, WorkerOptions{RetryBackoff: time.Millisecond, Visibility: time.Second, HeartbeatCadence: 100 * time.Millisecond, OnError: func(err error) { reported <- err }})
	}()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("worker did not finish")
	}
	if attempts.Load() != 2 {
		t.Fatalf("expected two attempts, got %d", attempts.Load())
	}
	select {
	case reportedErr := <-reported:
		var failure *WorkerFailure
		if !errors.As(reportedErr, &failure) || failure.Identity != "worker" || failure.State != WorkerStateHandling {
			t.Fatalf("worker diagnostics were not preserved: %#v", reportedErr)
		}
	default:
		t.Fatal("handler error was not reported")
	}
}

func TestMemoryProfileHasNoPostgresURL(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest, SigningSecret: []byte("0123456789abcdef")})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := forge.PostgresURL(); ErrorCodeOf(err) != CodeNotConfigured {
		t.Fatalf("expected not configured, got %v", err)
	}
}

func TestArgon2idPasswordRoundTrip(t *testing.T) {
	forge, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	hash, err := forge.HashPassword(context.Background(), "correct horse battery staple")
	if err != nil {
		t.Fatal(err)
	}
	if forge.NeedsRehash(hash) {
		t.Fatal("fresh Argon2id hash unexpectedly needs rehash")
	}
	valid, err := forge.VerifyPassword(context.Background(), "correct horse battery staple", hash)
	if err != nil || !valid {
		t.Fatalf("password did not verify: valid=%v err=%v", valid, err)
	}
	valid, err = forge.VerifyPassword(context.Background(), "wrong", hash)
	if err != nil || valid {
		t.Fatalf("wrong password verified: valid=%v err=%v", valid, err)
	}
	if _, err := forge.VerifyPassword(context.Background(), "password", "$argon2id$bad"); ErrorCodeOf(err) != CodeInvalid {
		t.Fatalf("expected invalid malformed hash, got %v", err)
	}
}

func TestInitFromSharedConfig(t *testing.T) {
	t.Setenv("FORGE_TEST_BACKEND", "memory")
	path := filepath.Join(t.TempDir(), "forge.toml")
	config := "[forge]\nnamespace = \"config_test\"\nmode = \"${FORGE_TEST_BACKEND}\"\nenvironment = \"test\"\n[queue]\npayload_retention_secs = 60\nterminal_retention_secs = 120\ndead_retention_secs = 180\ncancelled_retention_secs = 240\n"
	if err := os.WriteFile(path, []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
	forge, err := InitFrom(context.Background(), path)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = forge.Close(context.Background()) }()
	if forge.Mode() != ModeMemory || forge.Namespace() != "config_test" {
		t.Fatalf("unexpected resolved config: mode=%s namespace=%s", forge.Mode(), forge.Namespace())
	}
	if forge.queuePayloadRetention != time.Minute || forge.queueTerminalRetention != 2*time.Minute {
		t.Fatalf("queue retention was not loaded: payload=%s terminal=%s", forge.queuePayloadRetention, forge.queueTerminalRetention)
	}
	if forge.queueSucceededRetention != 2*time.Minute || forge.queueDeadRetention != 3*time.Minute || forge.queueCancelledRetention != 4*time.Minute {
		t.Fatalf("per-state queue retention was not loaded: succeeded=%s dead=%s cancelled=%s", forge.queueSucceededRetention, forge.queueDeadRetention, forge.queueCancelledRetention)
	}
}

func TestInitFromStringAndClose(t *testing.T) {
	forge, err := InitFromString(context.Background(), "[forge]\nmode = \"memory\"\nenvironment = \"test\"\n")
	if err != nil {
		t.Fatal(err)
	}
	if err := forge.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := forge.Close(context.Background()); err != nil {
		t.Fatalf("second close was not idempotent: %v", err)
	}
	if _, err := forge.KVGet(context.Background(), "after-close"); ErrorCodeOf(err) != CodePrecondition {
		t.Fatalf("new work after close should fail with precondition, got %v", err)
	}
}

func TestLongRunningQueueAndWeightedReservations(t *testing.T) {
	clock := NewManualClock(time.Unix(1_700_000_000, 0))
	client, err := NewMemoryForTesting(Config{Environment: EnvironmentTest, QueuePayloadRetention: time.Second, QueueTerminalRetention: 2 * time.Second}, TestOptions{ManualClock: clock, Random: NewSeededReader(10)})
	if err != nil {
		t.Fatal(err)
	}
	first, err := client.Enqueue(context.Background(), "work", []byte("first"), EnqueueOptions{Priority: PriorityHigh, ConcurrencyKey: "tenant-a"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := client.Enqueue(context.Background(), "work", []byte("blocked"), EnqueueOptions{Priority: PriorityHigh, ConcurrencyKey: "tenant-a"}); err != nil {
		t.Fatal(err)
	}
	other, err := client.Enqueue(context.Background(), "work", []byte("other"), EnqueueOptions{ConcurrencyKey: "tenant-b"})
	if err != nil {
		t.Fatal(err)
	}
	options := DequeueOptions{Visibility: time.Minute, ConcurrencyLimitPerKey: 1}
	job, err := client.Dequeue(context.Background(), "work", options)
	if err != nil || job == nil || job.ID != first {
		t.Fatalf("expected first high-priority job: job=%+v err=%v", job, err)
	}
	next, err := client.Dequeue(context.Background(), "work", options)
	if err != nil || next == nil || next.ID != other {
		t.Fatalf("noisy key blocked another key: job=%+v err=%v", next, err)
	}
	status, err := client.CancelJob(context.Background(), first)
	if err != nil || status == nil || status.State != JobCancelRequested {
		t.Fatalf("running cancellation was not observable: status=%+v err=%v", status, err)
	}
	if requested, err := client.CancellationRequested(context.Background(), job.Receipt); err != nil || !requested {
		t.Fatalf("cancellation check failed: requested=%t err=%v", requested, err)
	}
	if err := client.FinishCancellation(context.Background(), job.Receipt); err != nil {
		t.Fatal(err)
	}
	if err := client.Ack(context.Background(), next.Receipt); err != nil {
		t.Fatal(err)
	}

	limit := RateLimitOptions{Max: 10, Per: time.Hour, Cost: 5}
	reservation, err := client.RateLimitReserve(context.Background(), "tokens", "tenant", limit, time.Minute)
	if err != nil || reservation == nil {
		t.Fatalf("reserve failed: reservation=%+v err=%v", reservation, err)
	}
	settled, err := client.RateLimitCommit(context.Background(), reservation.ID, 2)
	if err != nil || settled.CommittedUnits == nil || *settled.CommittedUnits != 2 {
		t.Fatalf("commit failed: reservation=%+v err=%v", settled, err)
	}
	if _, err := client.RateLimitCommit(context.Background(), reservation.ID, 2); err != nil {
		t.Fatalf("same commit was not idempotent: %v", err)
	}
	limit.Cost = 8
	decision, err := client.RateLimitCheck(context.Background(), "tokens", "tenant", limit)
	if err != nil || !decision.Allowed || decision.Remaining != 0 {
		t.Fatalf("unused reservation was not refunded: decision=%+v err=%v", decision, err)
	}

	envelope := NewQueueEnvelope("example.task.v1", "application/octet-stream", []byte{0, 255})
	trace, err := NewTraceContext("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01", "", "", nil)
	if err != nil {
		t.Fatal(err)
	}
	envelope.TraceContext = &trace
	encoded, err := envelope.Encode()
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := DecodeQueueEnvelope(encoded)
	if err != nil || string(decoded.Body) != string(envelope.Body) {
		t.Fatalf("envelope round trip failed: decoded=%+v err=%v", decoded, err)
	}
	if !strings.Contains(string(encoded), `"traceparent"`) || strings.Contains(string(encoded), `"Traceparent"`) {
		t.Fatalf("trace context did not use the portable wire shape: %s", encoded)
	}
	deadID, err := client.Enqueue(context.Background(), "retained", []byte("secret"), EnqueueOptions{MaxAttempts: 1})
	if err != nil {
		t.Fatal(err)
	}
	deadJob, err := client.Dequeue(context.Background(), "retained", DequeueOptions{Visibility: time.Minute})
	if err != nil || deadJob == nil {
		t.Fatalf("dead-letter source dequeue failed: job=%+v err=%v", deadJob, err)
	}
	if err := client.Nack(context.Background(), deadJob.Receipt, NackOptions{}); err != nil {
		t.Fatal(err)
	}
	deadJob, err = client.Dequeue(context.Background(), "retained.dlq", DequeueOptions{Visibility: time.Minute})
	if err != nil || deadJob == nil {
		t.Fatalf("dead-letter dequeue failed: job=%+v err=%v", deadJob, err)
	}
	if err := client.Nack(context.Background(), deadJob.Receipt, NackOptions{}); err != nil {
		t.Fatal(err)
	}

	clock.Advance(time.Second)
	if err := client.Maintain(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Redrive(context.Background(), deadID, RedriveOptions{Destination: "retry", DedupPolicy: "clear"}); ErrorCodeOf(err) != CodePrecondition {
		t.Fatalf("redrive after payload retention should fail with precondition, got %v", err)
	}
	if status, err := client.JobStatus(context.Background(), deadID); err != nil || status == nil {
		t.Fatalf("terminal status should outlive its payload: status=%+v err=%v", status, err)
	}
	clock.Advance(time.Second)
	if err := client.Maintain(context.Background()); err != nil {
		t.Fatal(err)
	}
	if status, err := client.JobStatus(context.Background(), other); err != nil || status != nil {
		t.Fatalf("terminal retention did not remove status: status=%+v err=%v", status, err)
	}
}

func TestBatchQueueOperatorsAndDiagnostics(t *testing.T) {
	client, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	const deterministic = "11111111-1111-4111-8111-111111111111"
	results, err := client.EnqueueBatch(context.Background(), "operator-batch", []BatchEnqueueItem{
		{Payload: []byte("one"), Options: EnqueueOptions{ID: deterministic}},
		{Payload: []byte("two")},
	})
	if err != nil || len(results) != 2 || results[0].JobID == nil || *results[0].JobID != deterministic {
		t.Fatalf("unexpected batch result: results=%+v err=%v", results, err)
	}
	if err := client.PauseQueue(context.Background(), "operator-batch"); err != nil {
		t.Fatal(err)
	}
	if paused, err := client.QueuePaused(context.Background(), "operator-batch"); err != nil || !paused {
		t.Fatalf("queue was not paused: paused=%t err=%v", paused, err)
	}
	job, err := client.Dequeue(context.Background(), "operator-batch", DequeueOptions{Visibility: time.Minute})
	if err != nil || job != nil {
		t.Fatalf("paused queue leased work: job=%+v err=%v", job, err)
	}
	if err := client.ResumeQueue(context.Background(), "operator-batch"); err != nil {
		t.Fatal(err)
	}
	jobs, err := client.DequeueBatch(context.Background(), "operator-batch", 10, DequeueOptions{Visibility: time.Minute})
	if err != nil || len(jobs) != 2 {
		t.Fatalf("unexpected batch dequeue: jobs=%+v err=%v", jobs, err)
	}
	for _, job := range jobs {
		if err := client.Ack(context.Background(), job.Receipt); err != nil {
			t.Fatal(err)
		}
	}
	stats, err := client.QueueStats(context.Background(), "operator-batch")
	if err != nil || stats.EnqueuedTotal != 2 || stats.SettledTotal != 2 {
		t.Fatalf("unexpected queue stats: stats=%+v err=%v", stats, err)
	}
	diagnostics, err := client.Diagnostics(context.Background(), time.Second)
	if err != nil || !diagnostics.Ready {
		t.Fatalf("unexpected diagnostics: report=%+v err=%v", diagnostics, err)
	}
}

func TestSchedulerPauseDiagnosticsAndBoundedCatchUp(t *testing.T) {
	clock := NewManualClock(time.Date(2026, 8, 25, 8, 0, 0, 0, time.UTC))
	client, err := NewMemoryForTesting(Config{Environment: EnvironmentTest}, TestOptions{ManualClock: clock, Random: NewSeededReader(15)})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	options := ScheduleOptions{MisfirePolicy: MisfireCatchUp, MaxCatchUp: 3}
	if err := client.ScheduleCron(ctx, "minute", "* * * * *", "jobs", []byte("x"), options); err != nil {
		t.Fatal(err)
	}
	if paused, err := client.SchedulePause(ctx, "minute"); err != nil || !paused {
		t.Fatalf("pause failed: paused=%t err=%v", paused, err)
	}
	clock.Advance(20 * time.Minute)
	if diagnostics, err := client.SchedulerDiagnostics(ctx); err != nil || diagnostics.DueCount != 0 {
		t.Fatalf("paused schedule became due: diagnostics=%+v err=%v", diagnostics, err)
	}
	if resumed, err := client.ScheduleResume(ctx, "minute"); err != nil || !resumed {
		t.Fatalf("resume failed: resumed=%t err=%v", resumed, err)
	}
	if info, err := client.ScheduleInspect(ctx, "minute"); err != nil || info == nil || info.MisfirePolicy != string(MisfireCatchUp) || info.MaxCatchUp != 3 {
		t.Fatalf("inspect lost policy: info=%+v err=%v", info, err)
	}
	if processed, err := client.RunSchedulerOnce(ctx, 100); err != nil || processed != 3 {
		t.Fatalf("catch-up was not bounded: processed=%d err=%v", processed, err)
	}
	if diagnostics, err := client.SchedulerDiagnostics(ctx); err != nil || diagnostics.DueCount != 0 || diagnostics.LastSuccessfulTickMs == nil {
		t.Fatalf("unexpected scheduler diagnostics: diagnostics=%+v err=%v", diagnostics, err)
	}
}

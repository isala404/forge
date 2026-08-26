// Package forge provides Forge's native Go implementation.
package forge

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

const (
	// ContractVersion is the cross-language Forge contract implemented by this package.
	ContractVersion = "1.1.0"
	// DefaultNamespace is the unnamespaced profile. An explicit namespace is recommended
	// whenever independent applications share one physical backend.
	DefaultNamespace          = ""
	minimumPostgresVersionNum = 180000
	maximumPostgresVersionNum = 190000
)

// ErrorCode is stable across every Forge language package.
type ErrorCode string

const (
	CodeConfig        ErrorCode = "CONFIG"
	CodeNotConfigured ErrorCode = "NOT_CONFIGURED"
	CodeUnavailable   ErrorCode = "UNAVAILABLE"
	CodeNotFound      ErrorCode = "NOT_FOUND"
	CodePrecondition  ErrorCode = "PRECONDITION"
	CodeLimit         ErrorCode = "LIMIT"
	CodeInvalid       ErrorCode = "INVALID"
	CodeBackend       ErrorCode = "BACKEND"
)

// Error exposes only a safe message while retaining the local cause for debugging.
type Error struct {
	Code      ErrorCode
	Retryable bool
	Operation string
	Backend   string
	Message   string
	cause     error
}

func (e *Error) Error() string {
	if e.Operation == "" {
		return e.Message
	}
	return e.Operation + ": " + e.Message
}

func (e *Error) Unwrap() error {
	return e.cause
}

func forgeError(code ErrorCode, operation, message string) error {
	return &Error{Code: code, Retryable: code == CodeUnavailable, Operation: operation, Message: message}
}

func errorWithCause(code ErrorCode, operation, backend, message string, cause error) error {
	return &Error{Code: code, Retryable: code == CodeUnavailable, Operation: operation, Backend: backend, Message: message, cause: cause}
}

// ErrorCodeOf extracts a stable Forge code without matching display text.
func ErrorCodeOf(err error) ErrorCode {
	var forgeErr *Error
	if errors.As(err, &forgeErr) {
		return forgeErr.Code
	}
	return CodeBackend
}

// IsRetryable reports whether retrying may succeed without changing the request.
func IsRetryable(err error) bool {
	var forgeErr *Error
	return errors.As(err, &forgeErr) && forgeErr.Retryable
}

// RuntimeMode is a real runtime variant. Memory never manufactures a database handle.
type RuntimeMode string

const (
	ModeMemory   RuntimeMode = "memory"
	ModePostgres RuntimeMode = "postgres"
)

// Environment lets Forge reject accidental process-local state in production.
type Environment string

const (
	EnvironmentDevelopment Environment = "development"
	EnvironmentTest        Environment = "test"
	EnvironmentProduction  Environment = "production"
)

// Primitive identifies one Forge subsystem for measured PostgreSQL pool isolation.
type Primitive string

const (
	PrimitiveKV        Primitive = "kv"
	PrimitiveQueue     Primitive = "queue"
	PrimitiveBlob      Primitive = "blob"
	PrimitiveAuth      Primitive = "auth"
	PrimitiveConfig    Primitive = "config"
	PrimitiveRateLimit Primitive = "ratelimit"
	PrimitiveSchedule  Primitive = "schedule"
	PrimitivePubsub    Primitive = "pubsub"
)

var allPrimitives = []Primitive{
	PrimitiveKV,
	PrimitiveQueue,
	PrimitiveBlob,
	PrimitiveAuth,
	PrimitiveConfig,
	PrimitiveRateLimit,
	PrimitiveSchedule,
	PrimitivePubsub,
}

// DatabaseConfig defines one optional PostgreSQL bulkhead. Every target receives the
// same canonical Forge schema; only the pool and server placement differ.
type DatabaseConfig struct {
	PostgresURL    string
	MaxConnections int32
	AcquireTimeout time.Duration
}

// Config is the shared construction model. PostgreSQL fields become active in the PostgreSQL profile.
type Config struct {
	Mode                    RuntimeMode
	Environment             Environment
	Namespace               string
	PostgresURL             string
	MaxConnections          int32
	AcquireTimeout          time.Duration
	AutoMigrate             bool
	MigrationLockTimeout    time.Duration
	QueuePayloadRetention   time.Duration
	QueueTerminalRetention  time.Duration
	QueueSucceededRetention time.Duration
	QueueDeadRetention      time.Duration
	QueueCancelledRetention time.Duration
	AllowMemoryInProd       bool
	SigningSecret           []byte
	BlobBackend             string
	S3                      *S3Config
	Databases               map[Primitive]DatabaseConfig
}

// TestOptions are dependency-free injection points for deterministic tests.
type TestOptions struct {
	Clock       func() time.Time
	ManualClock *ManualClock
	Random      io.Reader
	Store       *MemoryStore
}

// MemoryStore is a process-local backing store that can be shared by multiple namespaced Forge handles.
type MemoryStore struct {
	mu                       sync.Mutex
	kv                       map[string]memoryKV
	jobs                     map[string]*memoryJob
	jobOrder                 []string
	receipts                 map[string]string
	dedup                    map[string]memoryDedup
	config                   map[string][]byte
	flags                    map[string]FlagRule
	blobs                    map[string]memoryBlob
	sessions                 map[string]memorySession
	apiKeys                  map[string]memoryAPIKey
	authTokens               map[string]memoryAuthToken
	rates                    map[string]memoryRate
	rateReservations         map[string]*memoryRateReservation
	schedules                map[string]memorySchedule
	schedulerLastSuccess     map[string]time.Time
	schedulerEnqueueFailures map[string]uint64
	subscriptions            map[string]map[uint64]chan []byte
	queuePaused              map[string]bool
	queueCounters            map[string]*memoryQueueCounter
	nextSubID                uint64
}

// NewMemoryStore creates an empty process-local store.
func NewMemoryStore() *MemoryStore {
	return &MemoryStore{
		kv:                       make(map[string]memoryKV),
		jobs:                     make(map[string]*memoryJob),
		receipts:                 make(map[string]string),
		dedup:                    make(map[string]memoryDedup),
		config:                   make(map[string][]byte),
		flags:                    make(map[string]FlagRule),
		blobs:                    make(map[string]memoryBlob),
		sessions:                 make(map[string]memorySession),
		apiKeys:                  make(map[string]memoryAPIKey),
		authTokens:               make(map[string]memoryAuthToken),
		rates:                    make(map[string]memoryRate),
		rateReservations:         make(map[string]*memoryRateReservation),
		schedules:                make(map[string]memorySchedule),
		schedulerLastSuccess:     make(map[string]time.Time),
		schedulerEnqueueFailures: make(map[string]uint64),
		subscriptions:            make(map[string]map[uint64]chan []byte),
		queuePaused:              make(map[string]bool),
		queueCounters:            make(map[string]*memoryQueueCounter),
	}
}

// Forge is the application-owned runtime handle.
type Forge struct {
	mode                    RuntimeMode
	namespace               string
	store                   *MemoryStore
	now                     func() time.Time
	random                  io.Reader
	secret                  []byte
	pg                      *pgxpool.Pool
	featurePools            map[Primitive]*pgxpool.Pool
	postgresURL             string
	subscriptionMu          sync.Mutex
	activeSubscriptions     map[*Subscription]struct{}
	shutdown                chan struct{}
	workerMu                sync.Mutex
	workers                 sync.WaitGroup
	activeWorkers           atomic.Int64
	closed                  atomic.Bool
	closeMu                 sync.Mutex
	resourcesClosed         bool
	s3Blob                  *s3Blob
	metrics                 *instanceMetrics
	configEnvironment       map[string][]byte
	testClock               *ManualClock
	queuePayloadRetention   time.Duration
	queueTerminalRetention  time.Duration
	queueSucceededRetention time.Duration
	queueDeadRetention      time.Duration
	queueCancelledRetention time.Duration
	environment             Environment
	allowMemoryInProd       bool
}

// NewMemory creates an explicit database-free, non-durable, process-local Forge.
func NewMemory(config Config) (*Forge, error) {
	return NewMemoryForTesting(config, TestOptions{})
}

// NewMemoryForTesting creates a memory runtime with optional deterministic dependencies.
func NewMemoryForTesting(config Config, options TestOptions) (*Forge, error) {
	if config.Mode != "" && config.Mode != ModeMemory {
		return nil, forgeError(CodeConfig, "init", "NewMemory requires mode=memory")
	}
	if err := validateEnvironment(config.Environment); err != nil {
		return nil, err
	}
	if config.Environment == EnvironmentProduction && !config.AllowMemoryInProd {
		return nil, forgeError(CodeConfig, "init", "memory mode is disabled in production")
	}
	if strings.TrimSpace(config.PostgresURL) != "" || config.MaxConnections != 0 || config.AcquireTimeout != 0 || config.AutoMigrate || config.MigrationLockTimeout != 0 || len(config.Databases) != 0 {
		return nil, forgeError(CodeConfig, "init", "memory mode cannot configure PostgreSQL")
	}
	if config.BlobBackend != "" && config.BlobBackend != "memory" {
		return nil, forgeError(CodeConfig, "init", "memory mode requires the memory blob backend")
	}
	if config.S3 != nil {
		return nil, forgeError(CodeConfig, "init", "memory mode cannot configure S3")
	}
	namespace := config.Namespace
	if namespace == "" {
		namespace = DefaultNamespace
	}
	if err := validateNamespace(namespace); err != nil {
		return nil, err
	}
	store := options.Store
	if store == nil {
		store = NewMemoryStore()
	}
	clock := options.Clock
	if options.ManualClock != nil {
		clock = options.ManualClock.Now
	} else if clock == nil {
		clock = time.Now
	}
	randomSource := options.Random
	if randomSource == nil {
		randomSource = rand.Reader
	}
	secret := append([]byte(nil), config.SigningSecret...)
	if config.QueuePayloadRetention == 0 {
		config.QueuePayloadRetention = 24 * time.Hour
	}
	if config.QueueTerminalRetention == 0 {
		config.QueueTerminalRetention = 7 * 24 * time.Hour
	}
	if config.QueueSucceededRetention == 0 {
		config.QueueSucceededRetention = config.QueueTerminalRetention
	}
	if config.QueueDeadRetention == 0 {
		config.QueueDeadRetention = config.QueueTerminalRetention
	}
	if config.QueueCancelledRetention == 0 {
		config.QueueCancelledRetention = config.QueueTerminalRetention
	}
	return &Forge{mode: ModeMemory, namespace: namespace, store: store, now: clock, random: randomSource, secret: secret, activeSubscriptions: make(map[*Subscription]struct{}), shutdown: make(chan struct{}), metrics: newInstanceMetrics(), configEnvironment: captureConfigEnvironment(), testClock: options.ManualClock, queuePayloadRetention: config.QueuePayloadRetention, queueTerminalRetention: config.QueueTerminalRetention, queueSucceededRetention: config.QueueSucceededRetention, queueDeadRetention: config.QueueDeadRetention, queueCancelledRetention: config.QueueCancelledRetention, environment: config.Environment, allowMemoryInProd: config.AllowMemoryInProd}, nil
}

// AdvanceTestClock moves time on a client created with TestOptions.ManualClock.
func (f *Forge) AdvanceTestClock(duration time.Duration) error {
	if duration < 0 {
		return forgeError(CodeInvalid, "testing.advance_clock", "duration must not be negative")
	}
	if f.testClock == nil {
		return forgeError(CodePrecondition, "testing.advance_clock", "client has no manual test clock")
	}
	f.testClock.Advance(duration)
	return nil
}

// Init constructs the requested native Go runtime.
func Init(ctx context.Context, config Config) (*Forge, error) {
	if err := ctx.Err(); err != nil {
		return nil, errorWithCause(CodeUnavailable, "init", "", "initialization was cancelled", err)
	}
	if config.Mode == "" {
		config.Mode = ModePostgres
	}
	if config.QueuePayloadRetention == 0 {
		config.QueuePayloadRetention = 24 * time.Hour
	}
	if config.QueueTerminalRetention == 0 {
		config.QueueTerminalRetention = 7 * 24 * time.Hour
	}
	if config.QueueSucceededRetention == 0 {
		config.QueueSucceededRetention = config.QueueTerminalRetention
	}
	if config.QueueDeadRetention == 0 {
		config.QueueDeadRetention = config.QueueTerminalRetention
	}
	if config.QueueCancelledRetention == 0 {
		config.QueueCancelledRetention = config.QueueTerminalRetention
	}
	if config.Mode == ModeMemory {
		return NewMemory(config)
	}
	if config.Mode != ModePostgres {
		return nil, forgeError(CodeConfig, "init", "mode must be memory or postgres")
	}
	if config.BlobBackend == "" {
		config.BlobBackend = "postgres"
	}
	if config.BlobBackend != "postgres" && config.BlobBackend != "s3" {
		return nil, forgeError(CodeConfig, "init", "blob backend must be postgres or s3")
	}
	if config.BlobBackend == "s3" && config.S3 == nil {
		return nil, forgeError(CodeConfig, "init", "S3 blob configuration is required")
	}
	if err := validateEnvironment(config.Environment); err != nil {
		return nil, err
	}
	if strings.TrimSpace(config.PostgresURL) == "" {
		return nil, forgeError(CodeConfig, "init", "PostgreSQL URL is required in postgres mode")
	}
	namespace := config.Namespace
	if namespace == "" {
		namespace = DefaultNamespace
	}
	if err := validateNamespace(namespace); err != nil {
		return nil, err
	}
	if config.MaxConnections == 0 {
		config.MaxConnections = 10
	}
	minimumConnections := int32(1)
	if config.AutoMigrate {
		minimumConnections = 2
	}
	if config.MaxConnections < minimumConnections {
		return nil, forgeError(CodeConfig, "init", fmt.Sprintf("max connections must be at least %d", minimumConnections))
	}
	if config.AcquireTimeout == 0 {
		config.AcquireTimeout = 30 * time.Second
	}
	if config.MigrationLockTimeout == 0 {
		config.MigrationLockTimeout = 30 * time.Second
	}
	pool, err := connectPostgres(ctx, DatabaseConfig{
		PostgresURL:    config.PostgresURL,
		MaxConnections: config.MaxConnections,
		AcquireTimeout: config.AcquireTimeout,
	}, minimumConnections)
	if err != nil {
		return nil, err
	}
	featurePools := make(map[Primitive]*pgxpool.Pool, len(config.Databases))
	closePools := func() {
		for _, featurePool := range featurePools {
			featurePool.Close()
		}
		pool.Close()
	}
	for primitive, database := range config.Databases {
		if !validPrimitive(primitive) {
			closePools()
			return nil, forgeError(CodeConfig, "init", "unknown database primitive: "+string(primitive))
		}
		if database.MaxConnections == 0 {
			database.MaxConnections = config.MaxConnections
		}
		if database.AcquireTimeout == 0 {
			database.AcquireTimeout = config.AcquireTimeout
		}
		config.Databases[primitive] = database
		minimumConnections := int32(1)
		if config.AutoMigrate && database.PostgresURL != config.PostgresURL {
			minimumConnections = 2
		}
		if database.PostgresURL == config.PostgresURL {
			minimumConnections = 1
		}
		featurePool, connectErr := connectPostgres(ctx, database, minimumConnections)
		if connectErr != nil {
			closePools()
			return nil, connectErr
		}
		featurePools[primitive] = featurePool
	}
	connectCtx, cancel := context.WithTimeout(ctx, config.AcquireTimeout)
	defer cancel()
	if config.AutoMigrate {
		var report MigrationReport
		report, err = migratePostgres(connectCtx, pool, "system", config.MigrationLockTimeout)
		if err == nil && report.State != "applied" {
			err = forgeError(CodeConfig, "init", "Forge schema is "+report.State+": "+report.Message)
		}
	} else {
		err = verifyPostgresSchema(connectCtx, pool)
	}
	if err != nil {
		closePools()
		return nil, err
	}
	migrated := map[string]struct{}{config.PostgresURL: {}}
	for primitive, featurePool := range featurePools {
		database := config.Databases[primitive]
		if _, ok := migrated[database.PostgresURL]; ok {
			continue
		}
		migrationCtx, migrationCancel := context.WithTimeout(ctx, database.AcquireTimeout)
		if config.AutoMigrate {
			var report MigrationReport
			report, err = migratePostgres(migrationCtx, featurePool, string(primitive), config.MigrationLockTimeout)
			if err == nil && report.State != "applied" {
				err = forgeError(CodeConfig, "init", "Forge schema is "+report.State+": "+report.Message)
			}
		} else {
			err = verifyPostgresSchema(migrationCtx, featurePool)
		}
		migrationCancel()
		if err != nil {
			closePools()
			return nil, err
		}
		migrated[database.PostgresURL] = struct{}{}
	}
	forge := &Forge{
		mode:                    ModePostgres,
		namespace:               namespace,
		now:                     time.Now,
		random:                  rand.Reader,
		secret:                  append([]byte(nil), config.SigningSecret...),
		pg:                      pool,
		featurePools:            featurePools,
		postgresURL:             config.PostgresURL,
		activeSubscriptions:     make(map[*Subscription]struct{}),
		shutdown:                make(chan struct{}),
		metrics:                 newInstanceMetrics(),
		configEnvironment:       captureConfigEnvironment(),
		queuePayloadRetention:   config.QueuePayloadRetention,
		queueTerminalRetention:  config.QueueTerminalRetention,
		queueSucceededRetention: config.QueueSucceededRetention,
		queueDeadRetention:      config.QueueDeadRetention,
		queueCancelledRetention: config.QueueCancelledRetention,
		environment:             config.Environment,
		allowMemoryInProd:       config.AllowMemoryInProd,
	}
	if config.BlobBackend == "s3" {
		forge.s3Blob, err = newS3Blob(ctx, *config.S3, namespace)
		if err != nil {
			closePools()
			return nil, err
		}
	}
	return forge, nil
}

// Mode returns the resolved runtime variant.
func (f *Forge) Mode() RuntimeMode {
	return f.mode
}

// Namespace returns the immutable application namespace.
func (f *Forge) Namespace() string {
	return f.namespace
}

// BackendCapabilities returns one resolved provider line per primitive without probing it.
func (f *Forge) BackendCapabilities() []BackendInfo {
	provider := "memory"
	durable := false
	if f.mode == ModePostgres {
		provider = "postgres"
		durable = true
	}
	report := make([]BackendInfo, 0, len(allPrimitives))
	for _, primitive := range allPrimitives {
		if primitive == PrimitiveBlob && f.s3Blob != nil {
			report = append(report, BackendInfo{Primitive: string(primitive), Provider: "s3", Durable: true, Caveats: "list is ordered but not a point-in-time snapshot"})
			continue
		}
		itemDurable := durable && primitive != "pubsub"
		caveats := "none"
		if !itemDurable {
			if primitive == "pubsub" {
				caveats = "at-most-once, non-durable"
			} else {
				caveats = "process-local, not shared across replicas"
			}
		}
		report = append(report, BackendInfo{Primitive: string(primitive), Provider: provider, Durable: itemDurable, Caveats: caveats})
	}
	return report
}

// PostgresURL returns one clear error in memory mode.
func (f *Forge) PostgresURL() (string, error) {
	if f.mode != ModePostgres {
		return "", forgeError(CodeNotConfigured, "postgres_url", "PostgreSQL is not configured in memory mode")
	}
	return f.postgresURL, nil
}

// Close is idempotent. It rejects future work and closes active subscriptions.
func (f *Forge) Close(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return errorWithCause(CodeUnavailable, "close", "", "shutdown deadline expired", err)
	}
	if f.closed.CompareAndSwap(false, true) {
		close(f.shutdown)
	}
	f.closeMu.Lock()
	defer f.closeMu.Unlock()
	if f.resourcesClosed {
		return nil
	}
	f.workerMu.Lock()
	workerDone := make(chan struct{})
	go func() {
		f.workers.Wait()
		close(workerDone)
	}()
	f.workerMu.Unlock()
	select {
	case <-workerDone:
	case <-ctx.Done():
		return errorWithCause(CodeUnavailable, "close", "", "shutdown deadline expired while draining workers", ctx.Err())
	}
	f.subscriptionMu.Lock()
	subscriptions := make([]*Subscription, 0, len(f.activeSubscriptions))
	for subscription := range f.activeSubscriptions {
		subscriptions = append(subscriptions, subscription)
	}
	f.subscriptionMu.Unlock()
	for _, subscription := range subscriptions {
		subscription.Close()
	}
	if f.mode == ModePostgres {
		for _, pool := range f.featurePools {
			pool.Close()
		}
		f.pg.Close()
		f.resourcesClosed = true
		return nil
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	prefix := f.namespace + "\x00"
	for topic, subscribers := range f.store.subscriptions {
		if len(topic) < len(prefix) || topic[:len(prefix)] != prefix {
			continue
		}
		for id, channel := range subscribers {
			close(channel)
			delete(subscribers, id)
		}
		delete(f.store.subscriptions, topic)
	}
	f.resourcesClosed = true
	return nil
}

func connectPostgres(ctx context.Context, database DatabaseConfig, minimumConnections int32) (*pgxpool.Pool, error) {
	if strings.TrimSpace(database.PostgresURL) == "" {
		return nil, forgeError(CodeConfig, "init", "PostgreSQL URL is required")
	}
	if database.MaxConnections < minimumConnections {
		return nil, forgeError(CodeConfig, "init", fmt.Sprintf("max connections must be at least %d", minimumConnections))
	}
	if database.AcquireTimeout <= 0 {
		return nil, forgeError(CodeConfig, "init", "acquire timeout must be positive")
	}
	poolConfig, err := pgxpool.ParseConfig(database.PostgresURL)
	if err != nil {
		return nil, errorWithCause(CodeConfig, "init", "postgres", "PostgreSQL URL is invalid", err)
	}
	poolConfig.MaxConns = database.MaxConnections
	connectCtx, cancel := context.WithTimeout(ctx, database.AcquireTimeout)
	defer cancel()
	pool, err := pgxpool.NewWithConfig(connectCtx, poolConfig)
	if err != nil {
		return nil, postgresError("init", err)
	}
	if err := pool.Ping(connectCtx); err != nil {
		pool.Close()
		return nil, postgresError("init", err)
	}
	var serverVersion int
	if err := pool.QueryRow(connectCtx, "SELECT current_setting('server_version_num')::int").Scan(&serverVersion); err != nil {
		pool.Close()
		return nil, errorWithCause(CodeConfig, "init", "postgres", "could not read the PostgreSQL server version", err)
	}
	if !supportedPostgresVersion(serverVersion) {
		pool.Close()
		return nil, forgeError(CodeConfig, "init", fmt.Sprintf("PostgreSQL server_version_num %d is unsupported; Forge requires PostgreSQL 18", serverVersion))
	}
	return pool, nil
}

func supportedPostgresVersion(version int) bool {
	return version >= minimumPostgresVersionNum && version < maximumPostgresVersionNum
}

func validPrimitive(primitive Primitive) bool {
	for _, candidate := range allPrimitives {
		if candidate == primitive {
			return true
		}
	}
	return false
}

func (f *Forge) postgres(primitive Primitive) *pgxpool.Pool {
	if pool := f.featurePools[primitive]; pool != nil {
		return pool
	}
	return f.pg
}

// Maintain performs idempotent expiry and retention work.
func (f *Forge) Maintain(ctx context.Context) error {
	if err := f.ready(ctx, "maintain"); err != nil {
		return err
	}
	if f.mode == ModeMemory {
		f.store.mu.Lock()
		defer f.store.mu.Unlock()
		now := f.now()
		for id, job := range f.store.jobs {
			if job.status != "done" && job.status != "dead" && job.status != "cancelled" {
				continue
			}
			age := now.Sub(job.completedAt)
			if age >= f.queuePayloadRetention {
				job.payload = nil
				job.payloadRetained = false
			}
			retention := f.queueTerminalRetention
			switch job.status {
			case "done":
				retention = f.queueSucceededRetention
			case "dead":
				retention = f.queueDeadRetention
			case "cancelled":
				retention = f.queueCancelledRetention
			}
			if age >= retention {
				delete(f.store.jobs, id)
			}
		}
		return nil
	}
	prefix := f.pgNamespacePrefix()
	queuePool := f.postgres(PrimitiveQueue)
	if _, err := queuePool.Exec(ctx, "UPDATE forge_jobs SET payload=''::bytea,payload_retained=false WHERE status IN ('done','dead','cancelled') AND completed_at<=now()-make_interval(secs=>$1::double precision) AND payload_retained AND left(queue,length($2))=$2", f.queuePayloadRetention.Seconds(), prefix); err != nil {
		return postgresError("maintain", err)
	}
	if _, err := queuePool.Exec(ctx, "DELETE FROM forge_jobs WHERE ((status='done' AND completed_at<=now()-make_interval(secs=>$1::double precision)) OR (status='dead' AND completed_at<=now()-make_interval(secs=>$2::double precision)) OR (status='cancelled' AND completed_at<=now()-make_interval(secs=>$3::double precision))) AND left(queue,length($4))=$4", f.queueSucceededRetention.Seconds(), f.queueDeadRetention.Seconds(), f.queueCancelledRetention.Seconds(), prefix); err != nil {
		return postgresError("maintain", err)
	}
	statements := []struct {
		primitive Primitive
		statement string
		argument  string
	}{
		{PrimitiveKV, "DELETE FROM forge_kv WHERE expires_at IS NOT NULL AND expires_at <= now() AND left(key, length($1)) = $1", f.pgNamespacePrefix()},
		{PrimitiveQueue, "DELETE FROM forge_job_dedup WHERE expires_at <= now() AND left(queue, length($1)) = $1", f.pgNamespacePrefix()},
		{PrimitiveAuth, "DELETE FROM forge_sessions WHERE (idle_deadline <= now() OR abs_deadline <= now()) AND app = $1", f.namespace},
		{PrimitiveAuth, "DELETE FROM forge_auth_tokens WHERE expires_at <= now() AND app = $1", f.namespace},
		{PrimitiveRateLimit, "DELETE FROM forge_ratelimit WHERE updated_at < now() - interval '24 hours' AND left(bucket, length($1)) = $1", f.pgNamespacePrefix()},
		{PrimitiveQueue, "UPDATE forge_jobs SET status = 'available', attempts = attempts + 1, lease_token = NULL, leased_until = NULL WHERE status = 'leased' AND leased_until <= now() AND attempts + 1 < max_attempts AND left(queue, length($1)) = $1", f.pgNamespacePrefix()},
		{PrimitiveQueue, "UPDATE forge_jobs SET status = 'dead', attempts = attempts + 1, lease_token = NULL, leased_until = NULL, completed_at = now() WHERE status = 'leased' AND leased_until <= now() AND attempts + 1 >= max_attempts AND left(queue, length($1)) = $1", f.pgNamespacePrefix()},
	}
	for _, item := range statements {
		if _, err := f.postgres(item.primitive).Exec(ctx, item.statement, item.argument); err != nil {
			return postgresError("maintain", err)
		}
	}
	return nil
}

func (f *Forge) ready(ctx context.Context, operation string) error {
	f.recordOperationStart(operation)
	if err := contextReady(ctx, operation); err != nil {
		return err
	}
	if f.closed.Load() {
		return forgeError(CodePrecondition, operation, "Forge is closed")
	}
	return nil
}

func contextReady(ctx context.Context, operation string) error {
	if err := ctx.Err(); err != nil {
		return errorWithCause(CodeUnavailable, operation, "", "operation was cancelled", err)
	}
	return nil
}

func (f *Forge) scoped(value string) string {
	return f.namespace + "\x00" + value
}

func validateNamespace(namespace string) error {
	if len(namespace) > 128 {
		return forgeError(CodeInvalid, "init", "namespace must contain at most 128 bytes")
	}
	for _, r := range namespace {
		if !(r == '-' || r == '_' || r == '.' || r >= '0' && r <= '9' || r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z') {
			return forgeError(CodeInvalid, "init", "namespace contains an unsupported character")
		}
	}
	return nil
}

func validateEnvironment(environment Environment) error {
	switch environment {
	case "", EnvironmentDevelopment, EnvironmentTest, EnvironmentProduction:
		return nil
	default:
		return forgeError(CodeConfig, "init", "environment must be development, test, or production")
	}
}

func randomID(reader io.Reader, prefix string) (string, error) {
	var bytes [16]byte
	if _, err := io.ReadFull(reader, bytes[:]); err != nil {
		return "", errorWithCause(CodeBackend, "random", "memory", "could not generate a secure identifier", err)
	}
	if prefix == "" {
		bytes[6] = (bytes[6] & 0x0f) | 0x40
		bytes[8] = (bytes[8] & 0x3f) | 0x80
		return formatUUID(bytes), nil
	}
	return fmt.Sprintf("%s%x", prefix, bytes), nil
}

func formatUUID(value [16]byte) string {
	return fmt.Sprintf("%x-%x-%x-%x-%x", value[0:4], value[4:6], value[6:8], value[8:10], value[10:16])
}

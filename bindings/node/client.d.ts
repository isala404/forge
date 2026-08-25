import type {
  BlobPutOptions,
  ForgeClient as NativeForgeClient,
  JsApiKey,
  JsApiKeyInfo,
  JsBackendInfo,
  JsBackendHealth,
  JsBlobInfo,
  JsBlobPage,
  JsBlobSummary,
  JsConditionalBlobGet,
  JsConfigEntry,
  JsConfigSnapshot,
  JsDecision,
  JsDiagnosticsReport,
  JsFlagEvaluation,
  JsFlagEvaluationEntry,
  JsFlagEvaluationRequest,
  JsDeadLetterPage,
  JsJob,
  JsHealthReport,
  JsMetricSample,
  JsMigrationReport,
  JsMultipartPart,
  JsMultipartUpload,
  JsNativePresign,
  JsOutboxRelayReport,
  JsProxyPresign,
  JsQueueDepth,
  JsRedriveBatchResult,
  JsScanPage,
  JsScheduleInfo,
  JsSchedulePage,
  JsSchedulerDiagnostics,
  JsSession,
  JsTokenConsumption,
} from './index';

export { JsSubscription } from './index';
export type {
  BlobPutOptions,
  JsApiKey,
  JsApiKeyInfo,
  JsBackendInfo,
  JsBackendHealth,
  JsBlobInfo,
  JsBlobPage,
  JsBlobSummary,
  JsConditionalBlobGet,
  JsConfigEntry,
  JsConfigSnapshot,
  JsDecision,
  JsDiagnosticsReport,
  JsFlagEvaluation,
  JsFlagEvaluationEntry,
  JsFlagEvaluationRequest,
  JsDeadLetterPage,
  JsJob,
  JsHealthReport,
  JsMetricSample,
  JsMigrationReport,
  JsMultipartPart,
  JsMultipartUpload,
  JsNativePresign,
  JsOutboxRelayReport,
  JsProxyPresign,
  JsQueueDepth,
  JsRedriveBatchResult,
  JsScanPage,
  JsScheduleInfo,
  JsSchedulePage,
  JsSchedulerDiagnostics,
  JsSession,
  JsTokenConsumption,
};

/**
 * Translates between typed values and the string payloads Forge stores. The default
 * for every handle is `jsonCodec` (strict `JSON.stringify`/`JSON.parse`); supply your
 * own only for non-JSON payloads.
 */
export interface Codec<T> {
  encode(value: T): string | Buffer;
  decode(value: string | Buffer): T;
}

/** Options for `KvKey.set`. */
export interface SetOptions {
  /** Expire the key this many seconds after the write. Omit for no expiry. */
  ttlSeconds?: number | null;
  /** Only write if the key does not exist (returns `false` if it does). */
  ifNotExists?: boolean | null;
  /** Only write if the key already exists (returns `false` if it doesn't). */
  ifExists?: boolean | null;
}

/** Options for `Queue.enqueue`. */
export interface EnqueueOptions {
  /** Delivery attempts before the job is dead-lettered. Default: 5. */
  maxAttempts?: number | null;
  /** Idempotency key: a repeated `dedupId` within the dedup window returns the existing job's id (no error). */
  dedupId?: string | null;
  /** Delay first delivery by this many seconds. Default: deliver immediately. */
  delaySeconds?: number | null;
  /** Caller-selected UUID. Repeating it on the same queue is idempotent. */
  jobId?: string | null;
  /** Reserved W3C propagation metadata, stored separately from the payload. */
  traceContext?: {
    traceparent: string;
    tracestate?: string;
    baggage?: string;
  };
  /** Only these baggage keys are preserved. Empty by default. */
  baggageAllowlist?: string[];
  priority?: 'low' | 'normal' | 'high';
  concurrencyKey?: string;
}

/** Options for `Queue.dequeue`. */
export interface DequeueOptions {
  /** Seconds the job stays leased (invisible to other consumers). Default: 30. */
  visibilitySeconds?: number;
  /** Seconds to long-poll for a job before resolving `null`. Default: 20. */
  waitSeconds?: number;
  /** Skip a keyed job while this many jobs with the same key are leased. */
  concurrencyLimitPerKey?: number;
}

export type JobState = 'queued' | 'delayed' | 'leased' | 'retrying' | 'succeeded' | 'dead' | 'cancel_requested' | 'cancelled';
export interface JobStatus { id: string; queue: string; state: JobState; attemptCount: number; maxAttempts: number; priority: 'low' | 'normal' | 'high'; concurrencyKey?: string; enqueuedAtMs: number; availableAtMs: number; completedAtMs?: number; }
export interface JobStatusPage { items: JobStatus[]; cursor?: string; }
export interface InvalidationEventV1 {
  schema_version: 1;
  tags: string[];
  query_keys: unknown[][];
  revision?: string;
}
export function encodeInvalidationEvent(event: InvalidationEventV1): Buffer;
export function decodeInvalidationEvent(encoded: string | Buffer): InvalidationEventV1;

export type CloudEventExtension = null | boolean | number | string;
export interface CloudEventV1 {
  id: string;
  source: string;
  type: string;
  subject?: string;
  time?: string;
  datacontenttype?: string;
  dataschema?: string;
  data?: Buffer;
  extensions?: Record<string, CloudEventExtension>;
}
export interface EnvConfigMapping { key: string; names: string[]; }
/** Encode CloudEvents 1.0 structured JSON, using data_base64 for binary data. */
export function encodeCloudEvent(event: CloudEventV1): Buffer;
/** Decode bounded CloudEvents 1.0 structured JSON into a binary-safe event. */
export function decodeCloudEvent(encoded: string | Buffer): CloudEventV1;
/** Translate an explicit environment snapshot to logical config keys. */
export function importEnvConfig(environment: Record<string, string>, mappings: EnvConfigMapping[]): Record<string, string>;
/** Translate logical config values to each mapping's first canonical environment name. */
export function exportEnvConfig(config: Record<string, string>, mappings: EnvConfigMapping[]): Record<string, string>;

export interface ReadonlyConfigSnapshot {
  readonly schemaVersion: 1;
  readonly createdAtMs: number;
  readonly expiresAtMs: number;
  readonly secretHandling: 'no_secrets' | 'application_protected';
  readonly config: ReadonlyArray<Readonly<JsConfigEntry>>;
  readonly flags: ReadonlyArray<Readonly<JsFlagEvaluationEntry>>;
}
export function encodeConfigSnapshot(snapshot: JsConfigSnapshot | ReadonlyConfigSnapshot): Buffer;
export function decodeConfigSnapshot(encoded: string | Buffer): ReadonlyConfigSnapshot;
export function configSnapshotGet(snapshot: JsConfigSnapshot | ReadonlyConfigSnapshot, key: string, nowMs?: number): string | null;
export function configSnapshotFlagDetails(snapshot: JsConfigSnapshot | ReadonlyConfigSnapshot, id: string, nowMs?: number): JsFlagEvaluation;

/** Options for `QueueJob.nack`. */
export interface NackOptions {
  /** Seconds until the job is redelivered. Default: the queue's backoff. */
  retrySeconds?: number | null;
  /** Bounded, redacted operator diagnostic. */
  failureSummary?: string | null;
}

export interface DeadLetterListOptions {
  cursor?: string | null;
  limit?: number;
}

export interface RedriveOptions {
  destination: string;
  dedupPolicy: 'clear' | 'preserve';
  cursor?: string | null;
  limit?: number;
}

export interface BatchEnqueueResult {
  jobId?: string;
  errorCode?: string;
  retryable: boolean;
  message?: string;
}

export interface QueueStats {
  enqueuedTotal: number;
  settledTotal: number;
  deadTotal: number;
  cancelledTotal: number;
  enqueueRatePerMinute: number;
  settleRatePerMinute: number;
  oldestVisibleAgeMs?: number;
  paused: boolean;
}

/**
 * A leased queue job. Settle it with `ack()` (done) or `nack()` (redeliver);
 * `heartbeat()` extends the lease while a long handler runs.
 */
export declare class QueueJob<T> {
  readonly id: string;
  /** Lease handle for ack/nack/heartbeat — NOT the job `id`; valid in this process only. */
  readonly receipt: string;
  readonly payload: T;
  /** 1-based delivery attempt counter. */
  readonly attempt: number;
  readonly maxAttempts: number;
  /** Lease expiry as a Unix epoch in milliseconds. */
  readonly leasedUntilMs: number;
  readonly queue: string;
  readonly traceContext?: {
    traceparent: string;
    tracestate?: string;
    baggage?: string;
  };
  /** True once `ack()` or `nack()` has been called. */
  readonly settled: boolean;
  /** Set by the managed worker when a heartbeat fails: the job will be redelivered. */
  readonly leaseLost: boolean;
  /** Aborted on Forge shutdown or application-requested cancellation. Handlers must cooperate. */
  readonly signal?: AbortSignal;
  ack(): Promise<void>;
  nack(opts?: NackOptions): Promise<void>;
  heartbeat(): Promise<void>;
}

/** Typed handle over one named queue. Create via `forge.queue(name)`. */
export declare class Queue<T = unknown> {
  readonly client: ForgeClient;
  readonly name: string;
  readonly codec: Codec<T>;
  /** Enqueue a payload; resolves to the job id. */
  enqueue(payload: T, opts?: EnqueueOptions): Promise<string>;
  enqueueBatch(items: Array<{ payload: T } & EnqueueOptions>): Promise<BatchEnqueueResult[]>;
  /** Lease the next job, or `null` after `waitSeconds` of long-polling. */
  dequeue(opts?: DequeueOptions): Promise<QueueJob<T> | null>;
  dequeueBatch(maxItems: number, opts?: DequeueOptions): Promise<QueueJob<T>[]>;
  depth(): Promise<JsQueueDepth>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  isPaused(): Promise<boolean>;
  stats(): Promise<QueueStats>;
  cancel(jobId: string): Promise<JobStatus | null>;
  status(jobId: string): Promise<JobStatus | null>;
  statuses(opts?: { states?: JobState[]; cursor?: string; limit?: number }): Promise<JobStatusPage>;
  deadLetters(opts?: DeadLetterListOptions): Promise<JsDeadLetterPage>;
  redrive(jobId: string, opts: RedriveOptions): Promise<boolean>;
  redriveBatch(opts: RedriveOptions): Promise<JsRedriveBatchResult>;
  purgeDryRun(): Promise<number>;
  /** Destructive; `confirmation` must exactly equal this queue's name. */
  purge(confirmation: string): Promise<number>;
  /** Run a managed worker loop on this queue; see `runWorker`. */
  worker(
    handler: (job: QueueJob<T>) => void | Promise<void>,
    opts?: WorkerOptions<T>,
  ): Promise<void>;
}

/** Typed handle over one KV key. Create via `forge.kv(key)`. */
export declare class KvKey<T = unknown> {
  readonly client: ForgeClient;
  readonly key: string;
  readonly codec: Codec<T>;
  get(): Promise<T | null>;
  getOrDefault(defaultValue: T): Promise<T>;
  /** Write the value. Resolves `false` when an `ifNotExists`/`ifExists` guard failed. */
  set(value: T, opts?: SetOptions): Promise<boolean>;
  delete(): Promise<boolean>;
  exists(): Promise<boolean>;
  /** Reset the key's TTL; resolves `false` if the key doesn't exist. */
  expire(ttlSeconds: number): Promise<boolean>;
  /** Atomic compare-and-swap; pass `null`/`undefined` as `oldValue` for "not set". */
  compareAndSwap(oldValue: T | null | undefined, newValue: T): Promise<boolean>;
}

/** Typed handle over one config key with an optional default. Create via `forge.config(key)`. */
export declare class ConfigKey<T = unknown> {
  readonly client: ForgeClient;
  readonly key: string;
  readonly defaultValue: T | null | undefined;
  readonly codec: Codec<T>;
  get(): Promise<T | null>;
  getOrDefault(defaultValue?: T): Promise<T>;
  set(value: T): Promise<void>;
  delete(): Promise<boolean>;
  /** Evaluate the key as a feature flag (with optional per-subject targeting). */
  flag(targetingKey?: string | null): Promise<boolean>;
}

/** Async iterator over a pubsub subscription; `close()` (or `return()`) unsubscribes. */
export declare class TopicSubscription<T = unknown> implements AsyncIterableIterator<T> {
  next(): Promise<IteratorResult<T>>;
  return(): Promise<IteratorResult<T>>;
  close(): Promise<void>;
  [Symbol.asyncIterator](): AsyncIterableIterator<T>;
}

/** Typed handle over one pubsub topic. Create via `forge.topic(name)`. */
export declare class Topic<T = unknown> {
  readonly client: ForgeClient;
  readonly name: string;
  readonly codec: Codec<T>;
  publish(payload: T): Promise<void>;
  subscribe(): Promise<TopicSubscription<T>>;
  /** The backend channel name for this topic (namespaced; for LISTEN/debug tooling). */
  channel(): string;
}

/** Options for the managed worker loop (`runWorker` / `forge.worker`). */
export interface WorkerOptions<T = unknown> extends DequeueOptions {
  codec?: Codec<T>;
  /** Maximum handlers executing at once. Default: 1. */
  concurrency?: number;
  /** Heartbeat cadence in seconds. Default: visibilitySeconds / 3. */
  heartbeatSeconds?: number;
  /** Base reconnect/dequeue retry delay with bounded jitter. Default: 0.25. */
  retryBackoffSeconds?: number;
  /** Grace period for active handlers before their signals abort. Default: 30. */
  drainDeadlineSeconds?: number;
  /** Low-cardinality diagnostic identity. Never used as a metric dimension. */
  identity?: string;
  /** Redelivery delay in seconds when the handler throws. Default: the queue's backoff. */
  retrySeconds?: number | null;
  /** Abort to stop the loop after the in-flight job settles. */
  signal?: AbortSignal;
  /** Called on every handler/dequeue failure; `job` is absent for dequeue errors. */
  onError?: (
    error: unknown,
    job?: QueueJob<T>,
    diagnostic?: { identity: string; state: string },
  ) => void | Promise<void>;
}

export interface OutboxRelayOptions {
  batchSize?: number;
  claimSeconds?: number;
  failureBackoffSeconds?: number;
  baggageAllowlist?: string[];
  idleSeconds?: number;
  retryBackoffSeconds?: number;
  signal?: AbortSignal;
  identity?: string;
  onError?: (error: unknown, diagnostic: { identity: string }) => void | Promise<void>;
}

export interface RateLimitReservation { id: string; reservedUnits: number; expiresAtMs: number; state: 'pending' | 'committed' | 'released' | 'expired'; committedUnits?: number; }

/**
 * The Forge handle. Construct with `await ForgeClient.init()` (reads `./forge.toml`)
 * or `initFrom(path)`; all connection settings, including `[postgres] embedded = true`,
 * live in that file. Prefer the typed handles (`queue`/`kv`/`config`/`topic`) over the
 * flat native methods inherited from the addon.
 */
export declare class ForgeClient extends NativeForgeClient {
  /** Connect using `./forge.toml`. */
  static init(): Promise<ForgeClient>;
  /** Connect using the config file at `path`. */
  static initFrom(path: string): Promise<ForgeClient>;
  /** Connect using canonical TOML supplied in memory. */
  static initFromString(toml: string): Promise<ForgeClient>;
  static migrate(): Promise<JsMigrationReport[]>;
  static migrateFrom(path: string): Promise<JsMigrationReport[]>;
  static migrateFromString(toml: string): Promise<JsMigrationReport[]>;
  static migrationStatus(): Promise<JsMigrationReport[]>;
  static migrationStatusFrom(path: string): Promise<JsMigrationReport[]>;
  static migrationStatusFromString(toml: string): Promise<JsMigrationReport[]>;
  static validateSchema(): Promise<JsMigrationReport[]>;
  static validateSchemaFrom(path: string): Promise<JsMigrationReport[]>;
  static validateSchemaFromString(toml: string): Promise<JsMigrationReport[]>;
  /** Idempotently drain and close within the caller's deadline. */
  close(timeoutSeconds?: number): Promise<void>;
  /** Static provider capabilities; this performs no I/O. */
  backendCapabilities(): JsBackendInfo[];
  /** Process liveness only. */
  isLive(): boolean;
  /** Bounded live operations against every enabled backend. */
  probe(deadlineSeconds?: number, readinessBackends?: string[]): Promise<JsHealthReport>;
  diagnostics(deadlineSeconds?: number): Promise<JsDiagnosticsReport>;
  metricsSnapshot(): JsMetricSample[];
  renderPrometheus(): string;
  configGetMany(keys: string[]): Promise<JsConfigEntry[]>;
  flagDetailsMany(requests: JsFlagEvaluationRequest[]): Promise<JsFlagEvaluationEntry[]>;
  configSnapshot(configKeys: string[], flagRequests: JsFlagEvaluationRequest[], maxStaleSeconds: number, secretHandling: 'no_secrets' | 'application_protected'): Promise<ReadonlyConfigSnapshot>;
  queue<T = unknown>(name: string, codec?: Codec<T>): Queue<T>;
  kv<T = unknown>(key: string, codec?: Codec<T>): KvKey<T>;
  config<T = unknown>(key: string, defaultValue?: T | null, codec?: Codec<T>): ConfigKey<T>;
  topic<T = unknown>(name: string, codec?: Codec<T>): Topic<T>;
  /** The resolved system-database DSN; use for app tables that share embedded Postgres. */
  postgresUrl(): string;
  /** Shorthand for `runWorker(this, name, handler, opts)`. */
  worker<T = unknown>(
    name: string,
    handler: (job: QueueJob<T>) => void | Promise<void>,
    opts?: WorkerOptions<T>,
  ): Promise<void>;
  runOutboxRelay(opts?: OutboxRelayOptions): Promise<void>;
  reserveRateLimit(bucket: string, key: string, opts: { max: number; perSeconds: number; cost: number; ttlSeconds: number; algo?: 'token_bucket' | 'sliding_window' }): Promise<RateLimitReservation | null>;
  commitRateLimit(reservationId: string, actualUnits: number): Promise<RateLimitReservation>;
  releaseRateLimit(reservationId: string): Promise<RateLimitReservation>;
}

/** Strict JSON codec (the default for every typed handle). */
export declare const jsonCodec: Codec<unknown>;
export declare const bytesCodec: Codec<Buffer>;

export interface ParsedScope { kind: 'kv' | 'blob' | 'rate' | 'topic'; application: string; tenant: string; user: string; resource: string; }
export declare function scopeKvKey(application: string, tenant: string, user: string, resource: string): string;
export declare function scopeBlobKey(application: string, tenant: string, user: string, resource: string): string;
export declare function scopeRateLimitSubject(application: string, tenant: string, user: string, resource: string): string;
export declare function scopeTopic(application: string, tenant: string, user: string, resource: string): string;
export declare function parseScopedName(value: string): Readonly<ParsedScope>;

export interface QueueEnvelope { version?: 1; schema: string; contentType: string; correlationId?: string; traceContext?: { traceparent: string; tracestate?: string; baggage?: string }; artifacts?: Array<{ uri: string; contentType?: string; version?: string }>; body: Buffer | Uint8Array; }
export declare function encodeQueueEnvelope(envelope: QueueEnvelope): Buffer;
export declare function decodeQueueEnvelope(encoded: Buffer | Uint8Array): QueueEnvelope & { version: 1; body: Buffer };

/**
 * Managed worker loop: dequeues jobs, runs `handler`, acks on success, nacks on
 * throw, auto-heartbeats at `visibilitySeconds / 3`, and backs off on dequeue
 * errors. Runs until `opts.signal` aborts.
 */
export declare function runWorker<T = unknown>(
  client: ForgeClient,
  name: string,
  handler: (job: QueueJob<T>) => void | Promise<void>,
  opts?: WorkerOptions<T>,
): Promise<void>;

export declare function runOutboxRelay(
  client: ForgeClient,
  opts?: OutboxRelayOptions,
): Promise<void>;

export declare function queue<T = unknown>(
  client: ForgeClient,
  name: string,
  codec?: Codec<T>,
): Queue<T>;

export declare function kv<T = unknown>(
  client: ForgeClient,
  key: string,
  codec?: Codec<T>,
): KvKey<T>;

export declare function config<T = unknown>(
  client: ForgeClient,
  key: string,
  defaultValue?: T | null,
  codec?: Codec<T>,
): ConfigKey<T>;

export declare function topic<T = unknown>(
  client: ForgeClient,
  name: string,
  codec?: Codec<T>,
): Topic<T>;

export type ForgeErrorCode =
  | 'NOT_FOUND'
  | 'INVALID'
  | 'LIMIT'
  | 'PRECONDITION'
  | 'UNAVAILABLE'
  | 'CONFIG'
  | 'NOT_CONFIGURED'
  | 'BACKEND';

export interface ForgeError extends Error {
  name: 'ForgeError';
  code: ForgeErrorCode;
  retryable: boolean;
  operation: string;
  backend?: string;
  safeMessage: string;
}

/** The stable Forge error code, if the value is a Forge error. */
export declare function forgeErrorCode(error: unknown): ForgeErrorCode | undefined;
/** Whether the structured error is safe to retry without changing the request. */
export declare function forgeErrorRetryable(error: unknown): boolean;

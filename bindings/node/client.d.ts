import type {
  ForgeClient as NativeForgeClient,
  JsApiKey,
  JsApiKeyInfo,
  JsBackendInfo,
  JsBlobInfo,
  JsBlobPage,
  JsDecision,
  JsJob,
  JsQueueDepth,
  JsScanPage,
  JsScheduleInfo,
  JsSchedulePage,
  JsSession,
} from './index';

export { JsSubscription } from './index';
export type {
  JsApiKey,
  JsApiKeyInfo,
  JsBackendInfo,
  JsBlobInfo,
  JsBlobPage,
  JsDecision,
  JsJob,
  JsQueueDepth,
  JsScanPage,
  JsScheduleInfo,
  JsSchedulePage,
  JsSession,
};

/**
 * Translates between typed values and the string payloads Forge stores. The default
 * for every handle is `jsonCodec` (strict `JSON.stringify`/`JSON.parse`); supply your
 * own only for non-JSON payloads.
 */
export interface Codec<T> {
  encode(value: T): string;
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
  /** Idempotency key: enqueueing the same `dedupId` twice raises `PRECONDITION`. */
  dedupId?: string | null;
  /** Delay first delivery by this many seconds. Default: deliver immediately. */
  delaySeconds?: number | null;
}

/** Options for `Queue.dequeue`. */
export interface DequeueOptions {
  /** Seconds the job stays leased (invisible to other consumers). Default: 30. */
  visibilitySeconds?: number;
  /** Seconds to long-poll for a job before resolving `null`. Default: 20. */
  waitSeconds?: number;
}

/** Options for `QueueJob.nack`. */
export interface NackOptions {
  /** Seconds until the job is redelivered. Default: the queue's backoff. */
  retrySeconds?: number | null;
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
  /** True once `ack()` or `nack()` has been called. */
  readonly settled: boolean;
  /** Set by the managed worker when a heartbeat fails: the job will be redelivered. */
  readonly leaseLost: boolean;
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
  /** Lease the next job, or `null` after `waitSeconds` of long-polling. */
  dequeue(opts?: DequeueOptions): Promise<QueueJob<T> | null>;
  depth(): Promise<JsQueueDepth>;
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
  /** Redelivery delay in seconds when the handler throws. Default: the queue's backoff. */
  retrySeconds?: number | null;
  /** Abort to stop the loop after the in-flight job settles. */
  signal?: AbortSignal;
  /** Called on every handler/dequeue failure; `job` is absent for dequeue errors. */
  onError?: (error: unknown, job?: QueueJob<T>) => void | Promise<void>;
}

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
}

/** Strict JSON codec (the default for every typed handle). */
export declare const jsonCodec: Codec<unknown>;

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
  | 'BACKEND';

/** The Forge error class parsed from the `CODE: message` prefix, if the error is one. */
export declare function forgeErrorCode(error: unknown): ForgeErrorCode | undefined;
/**
 * Whether the error is safe to retry: `UNAVAILABLE` always is, and `BACKEND` errors
 * are when the core flags them retryable (surfaced as a `BACKEND(retryable):` prefix).
 */
export declare function forgeErrorRetryable(error: unknown): boolean;

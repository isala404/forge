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

export interface Codec<T> {
  encode(value: T): string;
  decode(value: string | Buffer): T;
}

export interface SetOptions {
  ttlSeconds?: number | null;
  ifNotExists?: boolean | null;
  ifExists?: boolean | null;
}

export interface EnqueueOptions {
  maxAttempts?: number | null;
  dedupId?: string | null;
  delaySeconds?: number | null;
}

export interface DequeueOptions {
  visibilitySeconds?: number;
  waitSeconds?: number;
}

export interface NackOptions {
  retrySeconds?: number | null;
}

export declare class QueueJob<T> {
  readonly id: string;
  readonly receipt: string;
  readonly payload: T;
  readonly attempt: number;
  readonly maxAttempts: number;
  readonly leasedUntilMs: number;
  readonly queue: string;
  readonly settled: boolean;
  ack(): Promise<void>;
  nack(opts?: NackOptions): Promise<void>;
  heartbeat(): Promise<void>;
}

export declare class Queue<T = unknown> {
  readonly client: ForgeClient;
  readonly name: string;
  readonly codec: Codec<T>;
  enqueue(payload: T, opts?: EnqueueOptions): Promise<string>;
  dequeue(opts?: DequeueOptions): Promise<QueueJob<T> | null>;
  depth(): Promise<JsQueueDepth>;
  worker(
    handler: (job: QueueJob<T>) => void | Promise<void>,
    opts?: WorkerOptions<T>,
  ): Promise<void>;
}

export declare class KvKey<T = unknown> {
  readonly client: ForgeClient;
  readonly key: string;
  readonly codec: Codec<T>;
  get(): Promise<T | null>;
  getOrDefault(defaultValue: T): Promise<T>;
  set(value: T, opts?: SetOptions): Promise<boolean>;
  delete(): Promise<boolean>;
  exists(): Promise<boolean>;
  expire(ttlSeconds: number): Promise<boolean>;
  compareAndSwap(oldValue: T | null | undefined, newValue: T): Promise<boolean>;
}

export declare class ConfigKey<T = unknown> {
  readonly client: ForgeClient;
  readonly key: string;
  readonly defaultValue: T | null | undefined;
  readonly codec: Codec<T>;
  get(): Promise<T | null>;
  getOrDefault(defaultValue?: T): Promise<T>;
  set(value: T): Promise<void>;
  delete(): Promise<boolean>;
  flag(targetingKey?: string | null): Promise<boolean>;
}

export declare class TopicSubscription<T = unknown> implements AsyncIterableIterator<T> {
  next(): Promise<IteratorResult<T>>;
  return(): Promise<IteratorResult<T>>;
  close(): Promise<void>;
  [Symbol.asyncIterator](): AsyncIterableIterator<T>;
}

export declare class Topic<T = unknown> {
  readonly client: ForgeClient;
  readonly name: string;
  readonly codec: Codec<T>;
  publish(payload: T): Promise<void>;
  subscribe(): Promise<TopicSubscription<T>>;
  channel(): string;
}

export interface WorkerOptions<T = unknown> extends DequeueOptions {
  codec?: Codec<T>;
  retrySeconds?: number | null;
  signal?: AbortSignal;
  onError?: (error: unknown, job?: QueueJob<T>) => void | Promise<void>;
}

export declare class ForgeClient extends NativeForgeClient {
  static init(): Promise<ForgeClient>;
  static initFrom(path: string): Promise<ForgeClient>;
  queue<T = unknown>(name: string, codec?: Codec<T>): Queue<T>;
  kv<T = unknown>(key: string, codec?: Codec<T>): KvKey<T>;
  config<T = unknown>(key: string, defaultValue?: T | null, codec?: Codec<T>): ConfigKey<T>;
  topic<T = unknown>(name: string, codec?: Codec<T>): Topic<T>;
  worker<T = unknown>(
    name: string,
    handler: (job: QueueJob<T>) => void | Promise<void>,
    opts?: WorkerOptions<T>,
  ): Promise<void>;
}

export declare const jsonCodec: Codec<unknown>;

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

export declare function forgeErrorCode(error: unknown): string | undefined;
export declare function forgeErrorRetryable(error: unknown): boolean;

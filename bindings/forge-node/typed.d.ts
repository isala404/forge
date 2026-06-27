// Typed projection over ForgeClient (see typed.js). Generic handles bind a name +
// JSON codec to a payload/value/event type, so callers never touch a raw queue
// string or JSON.stringify. Mirrors the Rust typed layer (src/typed.rs).

import { ForgeClient, JsQueueDepth } from './index'

/** The stable error code the binding prefixes onto a thrown error's message. */
export type ForgeErrorCode =
  | 'NOT_FOUND'
  | 'INVALID'
  | 'LIMIT'
  | 'PRECONDITION'
  | 'UNAVAILABLE'
  | 'CONFIG'
  | 'BACKEND'
  | 'UNKNOWN'

/** Parse the code Forge prefixes onto an error message (e.g. "INVALID: ..."). */
export function forgeErrorCode(err: unknown): ForgeErrorCode

export interface EnqueueOptions {
  maxAttempts?: number
  dedupId?: string
}
export interface DequeueOptions {
  visibilitySeconds?: number
  waitSeconds?: number
}
export interface TypedJob<T> {
  id: string
  attempt: number
  payload: T
}

/** A typed queue handle: name + codec bound to payload type `T`. */
export class TypedQueue<T> {
  constructor(client: ForgeClient, name: string)
  enqueue(payload: T, opts?: EnqueueOptions): Promise<string>
  dequeue(opts?: DequeueOptions): Promise<TypedJob<T> | null>
  ack(id: string): Promise<void>
  nack(id: string, retrySeconds?: number): Promise<void>
  heartbeat(id: string): Promise<void>
  depth(): Promise<JsQueueDepth>
}

export interface SetOptions {
  ttlSeconds?: number
  ifNotExists?: boolean
}

/** A typed kv key: key + codec bound to value type `T`. */
export class TypedKvKey<T> {
  constructor(client: ForgeClient, key: string)
  get(): Promise<T | null>
  set(value: T, opts?: SetOptions): Promise<boolean>
  delete(): Promise<boolean>
}

/** A typed config key: key + codec + default bound to value type `T`. */
export class TypedConfigKey<T> {
  constructor(client: ForgeClient, key: string, defaultValue: T)
  get(): Promise<T | null>
  getOrDefault(): Promise<T>
  set(value: T): Promise<void>
}

/** A typed pubsub topic: topic + codec bound to event type `T`. */
export class TypedTopic<T> {
  constructor(client: ForgeClient, topic: string)
  publish(event: T): Promise<void>
  subscribe(): Promise<AsyncIterable<T>>
}

export function typedQueue<T>(client: ForgeClient, name: string): TypedQueue<T>
export function typedKv<T>(client: ForgeClient, key: string): TypedKvKey<T>
export function typedConfig<T>(client: ForgeClient, key: string, defaultValue: T): TypedConfigKey<T>
export function typedTopic<T>(client: ForgeClient, topic: string): TypedTopic<T>

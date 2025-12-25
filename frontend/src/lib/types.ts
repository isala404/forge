/**
 * FORGE error type returned from the server.
 */
export interface ForgeError {
  code: string;
  message: string;
  details?: Record<string, unknown>;
}

/**
 * Result of a query operation.
 */
export interface QueryResult<T> {
  loading: boolean;
  data: T | null;
  error: ForgeError | null;
}

/**
 * Result of a subscription operation.
 */
export interface SubscriptionResult<T> extends QueryResult<T> {
  stale: boolean;
}

/**
 * Paginated response wrapper.
 */
export interface Paginated<T> {
  data: T[];
  total: number;
  page: number;
  pageSize: number;
  hasMore: boolean;
}

/**
 * Page request parameters.
 */
export interface Page {
  page: number;
  pageSize: number;
}

/**
 * Sort order specification.
 */
export interface SortOrder {
  field: string;
  direction: 'asc' | 'desc';
}

/**
 * WebSocket connection state.
 */
export type ConnectionState = 'connecting' | 'connected' | 'reconnecting' | 'disconnected';

/**
 * Auth state for the current user.
 */
export interface AuthState {
  user: unknown | null;
  token: string | null;
  loading: boolean;
}

/**
 * Function type definitions for type-safe RPC calls.
 */
export interface QueryFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'query';
}

export interface MutationFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'mutation';
}

export interface ActionFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'action';
}

/**
 * FORGE client interface for making RPC calls.
 */
export interface ForgeClientInterface {
  call<T>(functionName: string, args: unknown): Promise<T>;
  subscribe<T>(functionName: string, args: unknown, callback: (data: T) => void): () => void;
  getConnectionState(): ConnectionState;
  connect(): Promise<void>;
  disconnect(): void;
}

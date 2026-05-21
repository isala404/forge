
/** Wire-format error from the Forge RPC layer. */
export interface ForgeError {
  code: string;
  message: string;
  /** Seconds to wait before retrying. Present on RATE_LIMITED errors. */
  retryAfterSecs?: number;
  details?: Record<string, unknown>;
  isRateLimited(): boolean;
  isUnauthorized(): boolean;
  isValidation(): boolean;
}

export interface QueryResult<T> {
  loading: boolean;
  data: T | null;
  error: ForgeError | null;
}

export interface SubscriptionResult<T> extends QueryResult<T> {
  stale: boolean;
}

export type ConnectionState = "disconnected" | "connecting" | "connected";

export interface AuthState {
  user: unknown | null;
  token: string | null;
  loading: boolean;
}

export type JobStatus =
  | "pending"
  | "claimed"
  | "running"
  | "completed"
  | "retry"
  | "failed"
  | "dead_letter"
  | "cancel_requested"
  | "cancelled";

export interface JobState<TOutput = unknown> {
  jobId: string;
  status: JobStatus;
  progress: number | null;
  message: string | null;
  output: TOutput | null;
  error: string | null;
}

export type WorkflowStatus =
  | "pending"
  | "running"
  | "sleeping"
  | "waiting"
  | "completed"
  | "failed";

export interface WorkflowStepState {
  name: string;
  status: "pending" | "running" | "completed" | "failed" | "compensated" | "skipped";
  error: string | null;
}

export interface WorkflowState<TOutput = unknown> {
  workflowId: string;
  status: WorkflowStatus;
  step: string | null;
  waitingFor: string | null;
  steps: WorkflowStepState[];
  output: TOutput | null;
  error: string | null;
}

/** Shape produced by the generated `toReactiveMutation()` helper. */
export interface ReactiveMutation<TArgs, TResult> {
  mutate: (args: TArgs) => Promise<TResult>;
  pending: boolean;
  error: ForgeError | null;
}


import { ForgeClientError, type ForgeClient } from "./client.js";
import { getForgeClient } from "./context.js";
import type {
  QueryResult,
  SubscriptionResult,
  ForgeError,
  ConnectionState,
  JobState,
  WorkflowState,
} from "./types.js";

export interface Readable<T> {
  subscribe: (run: (value: T) => void) => () => void;
}

export interface ConnectionStatusStore extends Readable<ConnectionState> {
  get(): ConnectionState;
}

export interface QueryStore<T> extends Readable<QueryResult<T>> {
  refetch: () => Promise<void>;
  reset: () => void;
}

export interface SubscriptionStore<T> extends Readable<SubscriptionResult<T>> {
  refetch: () => Promise<void>;
  unsubscribe: () => void;
  reset: () => void;
}

export interface JobStore<TOutput> extends Readable<JobState<TOutput> & { loading: boolean }> {
  unsubscribe: () => void;
}

export interface WorkflowStore<TOutput> extends Readable<WorkflowState<TOutput> & { loading: boolean }> {
  unsubscribe: () => void;
}

export function createConnectionStore(): ConnectionStatusStore {
  const client = getForgeClient();
  const subscribers = new Set<(value: ConnectionState) => void>();
  let currentState: ConnectionState = client.getConnectionState();

  client.onConnectionStateChange((state: ConnectionState) => {
    currentState = state;
    subscribers.forEach((run) => run(state));
  });

  return {
    subscribe(run) {
      subscribers.add(run);
      run(currentState);
      return () => subscribers.delete(run);
    },
    get() {
      return currentState;
    },
  };
}

// Type helper to reject empty objects - use null for no-arg functions
type RejectEmptyObject<T> = T extends Record<string, never> ? never : T;

export function createQueryStore<TArgs, TResult>(
  functionName: string,
  args: RejectEmptyObject<TArgs>
): QueryStore<TResult> {
  const client = getForgeClient();
  const subscribers = new Set<(value: QueryResult<TResult>) => void>();
  let state: QueryResult<TResult> = {
    loading: true,
    data: null,
    error: null,
  };

  const notify = () => subscribers.forEach((run) => run(state));

  const fetchData = async () => {
    state = { ...state, loading: true, error: null };
    notify();

    try {
      const data = await client.call<TResult>(functionName, args);
      state = { loading: false, data, error: null };
    } catch (e) {
      state = { loading: false, data: null, error: e as ForgeError };
    }
    notify();
  };

  fetchData();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => subscribers.delete(run);
    },
    refetch: fetchData,
    reset: () => {
      state = { loading: true, data: null, error: null };
      notify();
    },
  };
}

export function createSubscriptionStore<TArgs, TResult>(
  functionName: string,
  args: RejectEmptyObject<TArgs>
): SubscriptionStore<TResult> {
  const client = getForgeClient();
  const subscribers = new Set<(value: SubscriptionResult<TResult>) => void>();
  let unsubscribeFn: (() => void) | null = null;
  let subscriptionId: string | null = null;
  let state: SubscriptionResult<TResult> = {
    loading: true,
    data: null,
    error: null,
    stale: false,
  };

  const notify = () => subscribers.forEach((run) => run(state));

  const startSubscription = async () => {
    if (unsubscribeFn) {
      unsubscribeFn();
      unsubscribeFn = null;
    }
    if (subscriptionId) {
      client._unregisterQuery(subscriptionId);
    }

    state = { ...state, loading: true, error: null, stale: false };
    notify();

    try {
      subscriptionId = crypto.randomUUID();
      const initialData = await client._registerQuery(subscriptionId, functionName, args);
      state = { loading: false, data: initialData as TResult, error: null, stale: false };
      notify();

      unsubscribeFn = client._subscribe(`sub:${subscriptionId}`, (data: unknown) => {
        state = { loading: false, data: data as TResult, error: null, stale: false };
        notify();
      });
    } catch (e) {
      state = { loading: false, data: null, error: e as ForgeError, stale: false };
      notify();
    }
  };

  startSubscription();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => {
        subscribers.delete(run);
        if (subscribers.size === 0 && unsubscribeFn) {
          unsubscribeFn();
          unsubscribeFn = null;
          if (subscriptionId) {
            client._unregisterQuery(subscriptionId);
          }
        }
      };
    },
    refetch: startSubscription,
    unsubscribe: () => {
      if (unsubscribeFn) {
        unsubscribeFn();
        unsubscribeFn = null;
      }
      if (subscriptionId) {
        client._unregisterQuery(subscriptionId);
        subscriptionId = null;
      }
    },
    reset: () => {
      state = { loading: true, data: null, error: null, stale: false };
      notify();
    },
  };
}

const uuidRegex = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function asValidRecord(
  data: unknown,
  ...requiredStringFields: string[]
): Record<string, unknown> | null {
  if (!data || typeof data !== "object") return null;
  const record = data as Record<string, unknown>;
  for (const field of requiredStringFields) {
    if (typeof record[field] !== "string") return null;
  }
  return record;
}

export function createJobStore<TArgs, TOutput>(
  functionName: string,
  args: RejectEmptyObject<TArgs>
): JobStore<TOutput> {
  const client = getForgeClient();
  const subscribers = new Set<(value: JobState<TOutput> & { loading: boolean }) => void>();
  let unsubscribeFn: (() => void) | null = null;
  let state: JobState<TOutput> & { loading: boolean } = {
    jobId: "",
    status: "pending",
    progress: null,
    message: null,
    output: null,
    error: null,
    loading: true,
  };

  const notify = () => subscribers.forEach((run) => run(state));

  const startJob = async () => {
    try {
      const result = await client.call<{ job_id: string }>(functionName, args);
      const jobId = result.job_id;

      if (!uuidRegex.test(jobId)) {
        throw new Error("Invalid job ID returned from server");
      }

      state = { ...state, jobId, loading: false };
      notify();

      const clientSubId = crypto.randomUUID();
      const initialData = await client._registerJob(clientSubId, jobId);

      const applyJobData = (data: unknown) => {
        const jobData = asValidRecord(data, "job_id", "status");
        if (!jobData) {
          state = { ...state, status: "failed", error: "Invalid job update", loading: false };
          notify();
          return;
        }
        state = {
          jobId: jobData.job_id as string,
          status: jobData.status as JobState<TOutput>["status"],
          progress: typeof jobData.progress === "number" ? jobData.progress : null,
          message: typeof jobData.message === "string" ? jobData.message : null,
          output: (jobData.output ?? null) as TOutput | null,
          error: typeof jobData.error === "string" ? jobData.error : null,
          loading: false,
        };
        notify();
      };

      if (initialData) applyJobData(initialData);

      unsubscribeFn = client._subscribe(`job:${clientSubId}`, applyJobData);
    } catch (e) {
      state = {
        ...state,
        status: "failed",
        error: (e as Error).message,
        loading: false,
      };
      notify();
    }
  };

  startJob();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => {
        subscribers.delete(run);
        if (subscribers.size === 0 && unsubscribeFn) {
          unsubscribeFn();
          unsubscribeFn = null;
        }
      };
    },
    unsubscribe: () => {
      if (unsubscribeFn) {
        unsubscribeFn();
        unsubscribeFn = null;
      }
    },
  };
}

export function createWorkflowStore<TArgs, TOutput>(
  functionName: string,
  args: RejectEmptyObject<TArgs>,
): WorkflowStore<TOutput> {
  const client = getForgeClient();
  const subscribers = new Set<(value: WorkflowState<TOutput> & { loading: boolean }) => void>();
  let unsubscribeFn: (() => void) | null = null;
  let state: WorkflowState<TOutput> & { loading: boolean } = {
    workflowId: "",
    status: "pending",
    step: null,
    waitingFor: null,
    steps: [],
    output: null,
    error: null,
    loading: true,
  };

  const notify = () => subscribers.forEach((run) => run(state));

  const startWorkflow = async () => {
    try {
      const result = await client.call<{ workflow_id: string }>(functionName, args);
      const workflowId = result.workflow_id;

      if (!uuidRegex.test(workflowId)) {
        throw new Error("Invalid workflow ID returned from server");
      }

      state = { ...state, workflowId, loading: false };
      notify();

      const clientSubId = crypto.randomUUID();
      const initialData = await client._registerWorkflow(clientSubId, workflowId);

      const applyWorkflowData = (data: unknown) => {
        const wfData = asValidRecord(data, "workflow_id", "status");
        if (!wfData) {
          state = { ...state, status: "failed", error: "Invalid workflow update", loading: false };
          notify();
          return;
        }
        const rawSteps = Array.isArray(wfData.steps) ? wfData.steps : [];
        state = {
          workflowId: wfData.workflow_id as string,
          status: wfData.status as WorkflowState<TOutput>["status"],
          step: typeof wfData.step === "string" ? wfData.step : null,
          waitingFor: typeof wfData.waiting_for === "string" ? wfData.waiting_for : null,
          steps: rawSteps
            .filter((s): s is Record<string, unknown> => s && typeof s === "object")
            .filter((s) => typeof s.name === "string" && typeof s.status === "string")
            .map((s) => ({
              name: s.name as string,
              status: s.status as "pending" | "running" | "completed" | "failed" | "compensated" | "skipped",
              error: typeof s.error === "string" ? s.error : null,
            })),
          output: (wfData.output ?? null) as TOutput | null,
          error: typeof wfData.error === "string" ? wfData.error : null,
          loading: false,
        };
        notify();
      };

      if (initialData) applyWorkflowData(initialData);

      unsubscribeFn = client._subscribe(`wf:${clientSubId}`, applyWorkflowData);
    } catch (e) {
      state = {
        ...state,
        status: "failed",
        error: (e as Error).message,
        loading: false,
      };
      notify();
    }
  };

  startWorkflow();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => {
        subscribers.delete(run);
        if (subscribers.size === 0 && unsubscribeFn) {
          unsubscribeFn();
          unsubscribeFn = null;
        }
      };
    },
    unsubscribe: () => {
      if (unsubscribeFn) {
        unsubscribeFn();
        unsubscribeFn = null;
      }
    },
  };
}

/**
 * Fire-and-forget wrapper for mutations. Errors route to the global
 * `onMutationError` handler registered on the ForgeClient, or to the
 * optional per-call `onError` callback.
 */
export function fireMutation<TArgs, TResult>(
  mutationFn: (args: TArgs) => Promise<TResult>,
  args: TArgs,
  onError?: (error: ForgeClientError) => void,
): void {
  const client = getForgeClient();
  mutationFn(args).catch((err: unknown) => {
    const error =
      err instanceof ForgeClientError
        ? err
        : new ForgeClientError("UNKNOWN", String(err));
    if (onError) {
      onError(error);
    } else {
      client.notifyMutationError(error);
    }
  });
}

export interface OptimisticMutationStore<TArgs, TData> {
  /** Fire the mutation with optimistic update applied immediately. */
  fire: (args: TArgs) => void;
  /** Readable store of the derived data (subscription + optimistic patches). */
  data: Readable<TData | null>;
}

/**
 * Create an optimistic mutation that layers local patches over a live
 * subscription. Fires the mutation, applies the transform immediately,
 * and auto-reverts on error or TTL expiry (default 3s).
 */
export function createOptimisticMutation<TArgs, TResult, TData>(
  mutationFn: (args: TArgs) => Promise<TResult>,
  subscription: SubscriptionStore<TData>,
  apply: (data: TData, args: TArgs) => TData,
  options?: { ttlMs?: number },
): OptimisticMutationStore<TArgs, TData> {
  const ttlMs = options?.ttlMs ?? 3000;
  const client = getForgeClient();
  const subscribers = new Set<(value: TData | null) => void>();
  let currentView: TData | null = null;
  let latestSubData: TData | null = null;
  let pendingGeneration = 0;
  let ttlTimer: ReturnType<typeof setTimeout> | null = null;

  const notify = () => subscribers.forEach((run) => run(currentView));

  // Track subscription data and handle SSE confirmations
  const unsubscribeSub = subscription.subscribe((result) => {
    latestSubData = result.data;
    if (pendingGeneration > 0) {
      // SSE confirmed: adopt server data, clear pending
      pendingGeneration = 0;
      if (ttlTimer) {
        clearTimeout(ttlTimer);
        ttlTimer = null;
      }
    }
    currentView = result.data;
    notify();
  });

  const data: Readable<TData | null> = {
    subscribe(run) {
      subscribers.add(run);
      run(currentView);
      return () => {
        subscribers.delete(run);
        if (subscribers.size === 0) {
          unsubscribeSub();
          if (ttlTimer) clearTimeout(ttlTimer);
        }
      };
    },
  };

  function fire(args: TArgs): void {
    const snapshot = currentView;

    if (currentView !== null) {
      currentView = apply(currentView, args);
      notify();
    }

    const generation = ++pendingGeneration;

    // TTL safety net
    if (ttlTimer) clearTimeout(ttlTimer);
    ttlTimer = setTimeout(() => {
      if (pendingGeneration === generation) {
        pendingGeneration = 0;
        currentView = latestSubData;
        notify();
      }
    }, ttlMs);

    // Send the actual mutation
    mutationFn(args).catch((err: unknown) => {
      // Rollback on error
      if (pendingGeneration === generation) {
        pendingGeneration = 0;
        if (ttlTimer) {
          clearTimeout(ttlTimer);
          ttlTimer = null;
        }
        currentView = snapshot;
        notify();
      }
      const error =
        err instanceof ForgeClientError
          ? err
          : new ForgeClientError("UNKNOWN", String(err));
      client.notifyMutationError(error);
    });
  }

  return { fire, data };
}


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

/** Shared subscriber bookkeeping for every store: holds the current value,
 *  notifies subscribers on change, and fires `onLastUnsubscribe` when the final
 *  subscriber detaches (so stores can release server-side subscriptions). */
interface StoreCore<T> {
  get(): T;
  set(value: T): void;
  update(fn: (prev: T) => T): void;
  subscribe(run: (value: T) => void, onLastUnsubscribe?: () => void): () => void;
}

function createStoreCore<T>(initial: T): StoreCore<T> {
  const subscribers = new Set<(value: T) => void>();
  let state = initial;
  const notify = () => subscribers.forEach((run) => run(state));
  return {
    get: () => state,
    set(value) {
      state = value;
      notify();
    },
    update(fn) {
      state = fn(state);
      notify();
    },
    subscribe(run, onLastUnsubscribe) {
      subscribers.add(run);
      run(state);
      return () => {
        subscribers.delete(run);
        if (subscribers.size === 0) onLastUnsubscribe?.();
      };
    },
  };
}

export function createConnectionStore(): ConnectionStatusStore {
  const client = getForgeClient();
  const core = createStoreCore<ConnectionState>(client.getConnectionState());
  client.onConnectionStateChange((state) => core.set(state));
  return {
    subscribe: (run) => core.subscribe(run),
    get: () => core.get(),
  };
}

type RejectEmptyObject<T> = T extends Record<string, never> ? never : T;

/** Optional runtime validator for store payloads. Return null to surface a
 *  schema mismatch as an error instead of letting the UI deref garbage. */
export interface StoreOptions<T> {
  validate?: (data: unknown) => T | null;
}

const VALIDATION_ERROR: ForgeError = new ForgeClientError(
  "VALIDATION_ERROR",
  "Response failed runtime validation",
);

export function createQueryStore<TArgs, TResult>(
  functionName: string,
  args: RejectEmptyObject<TArgs>,
  options?: StoreOptions<TResult>,
): QueryStore<TResult> {
  const client = getForgeClient();
  const core = createStoreCore<QueryResult<TResult>>({
    loading: true,
    data: null,
    error: null,
  });

  const fetchData = async () => {
    core.set({ ...core.get(), loading: true, error: null });

    try {
      const raw = await client.call<unknown>(functionName, args);
      const data = options?.validate ? options.validate(raw) : (raw as TResult);
      if (data === null && options?.validate) {
        core.set({ loading: false, data: null, error: VALIDATION_ERROR });
      } else {
        core.set({ loading: false, data: data as TResult, error: null });
      }
    } catch (e) {
      core.set({ loading: false, data: null, error: e as ForgeError });
    }
  };

  fetchData();

  return {
    subscribe: (run) => core.subscribe(run),
    refetch: fetchData,
    reset: () => core.set({ loading: true, data: null, error: null }),
  };
}

export function createSubscriptionStore<TArgs, TResult>(
  functionName: string,
  args: RejectEmptyObject<TArgs>,
  options?: StoreOptions<TResult>,
): SubscriptionStore<TResult> {
  const client = getForgeClient();
  const core = createStoreCore<SubscriptionResult<TResult>>({
    loading: true,
    data: null,
    error: null,
    stale: false,
  });
  let unsubscribeFn: (() => void) | null = null;
  let subscriptionId: string | null = null;

  const startSubscription = async () => {
    if (unsubscribeFn) {
      unsubscribeFn();
      unsubscribeFn = null;
    }
    if (subscriptionId) {
      client._unregisterQuery(subscriptionId);
    }

    core.set({ ...core.get(), loading: true, error: null, stale: false });

    try {
      subscriptionId = crypto.randomUUID();

      // Register the update callback BEFORE the fallible initial registration.
      // If the first registration fails — e.g. an auth-required query subscribed
      // while still anonymous returns 401 — the callback must already be wired so
      // that once a later reconnect re-registers the subscription (after login),
      // reactor pushes are delivered and the store recovers. Wiring it only after
      // a successful registration left such subscriptions permanently dead.
      unsubscribeFn = client._subscribe(`sub:${subscriptionId}`, (raw: unknown) => {
        const data = options?.validate ? options.validate(raw) : (raw as TResult);
        if (data === null && options?.validate) {
          core.set({ loading: false, data: null, error: VALIDATION_ERROR, stale: false });
        } else {
          core.set({ loading: false, data: data as TResult, error: null, stale: false });
        }
      });

      const initialRaw = await client._registerQuery(subscriptionId, functionName, args);
      const initial = options?.validate ? options.validate(initialRaw) : (initialRaw as TResult);
      if (initial === null && options?.validate) {
        core.set({ loading: false, data: null, error: VALIDATION_ERROR, stale: false });
      } else {
        core.set({ loading: false, data: initial as TResult, error: null, stale: false });
      }
    } catch (e) {
      core.set({ loading: false, data: null, error: e as ForgeError, stale: false });
    }
  };

  startSubscription();

  return {
    subscribe: (run) =>
      core.subscribe(run, () => {
        if (unsubscribeFn) {
          unsubscribeFn();
          unsubscribeFn = null;
          if (subscriptionId) {
            client._unregisterQuery(subscriptionId);
          }
        }
      }),
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
    reset: () => core.set({ loading: true, data: null, error: null, stale: false }),
  };
}

const uuidRegex = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const JOB_STATUSES = new Set([
  "pending", "claimed", "running", "completed", "failed",
  "retry", "dead_letter", "cancel_requested", "cancelled",
]);

const WORKFLOW_STATUSES = new Set([
  "pending", "running", "sleeping", "waiting", "completed", "failed",
  "blocked_missing_version", "blocked_signature_mismatch", "blocked_missing_handler",
]);

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
  const core = createStoreCore<JobState<TOutput> & { loading: boolean }>({
    jobId: "",
    status: "pending",
    progress: null,
    message: null,
    output: null,
    error: null,
    loading: true,
  });
  let unsubscribeFn: (() => void) | null = null;
  let clientSubId: string | null = null;

  const startJob = async () => {
    try {
      const result = await client.call<{ job_id: string }>(functionName, args);
      const jobId = result.job_id;

      if (!uuidRegex.test(jobId)) {
        throw new Error("Invalid job ID returned from server");
      }

      core.update((s) => ({ ...s, jobId, loading: false }));

      const applyJobData = (data: unknown) => {
        const jobData = asValidRecord(data, "job_id", "status");
        if (!jobData || !JOB_STATUSES.has(jobData.status as string)) {
          core.update((s) => ({ ...s, status: "failed", error: "Invalid job update", loading: false }));
          return;
        }
        core.set({
          jobId: jobData.job_id as string,
          status: jobData.status as JobState<TOutput>["status"],
          progress: typeof jobData.progress === "number" ? jobData.progress : null,
          message: typeof jobData.message === "string" ? jobData.message : null,
          output: (jobData.output ?? null) as TOutput | null,
          error: typeof jobData.error === "string" ? jobData.error : null,
          loading: false,
        });
      };

      clientSubId = crypto.randomUUID();
      // Register the update callback before the server registration so the
      // subscription survives a reconnect-driven re-registration (the client
      // re-registers job subs whose callback is still present).
      unsubscribeFn = client._subscribe(`job:${clientSubId}`, applyJobData);
      const initialData = await client._registerJob(clientSubId, jobId);
      if (initialData) applyJobData(initialData);
    } catch (e) {
      core.update((s) => ({ ...s, status: "failed", error: (e as Error).message, loading: false }));
    }
  };

  startJob();

  const release = () => {
    unsubscribeFn = null;
    if (clientSubId) {
      client._unregisterJob(clientSubId);
      clientSubId = null;
    }
  };

  return {
    subscribe: (run) => core.subscribe(run, release),
    unsubscribe: release,
  };
}

export function createWorkflowStore<TArgs, TOutput>(
  functionName: string,
  args: RejectEmptyObject<TArgs>,
): WorkflowStore<TOutput> {
  const client = getForgeClient();
  const core = createStoreCore<WorkflowState<TOutput> & { loading: boolean }>({
    workflowId: "",
    status: "pending",
    step: null,
    waitingFor: null,
    steps: [],
    output: null,
    error: null,
    loading: true,
  });
  let unsubscribeFn: (() => void) | null = null;
  let clientSubId: string | null = null;

  const startWorkflow = async () => {
    try {
      const result = await client.call<{ workflow_id: string }>(functionName, args);
      const workflowId = result.workflow_id;

      if (!uuidRegex.test(workflowId)) {
        throw new Error("Invalid workflow ID returned from server");
      }

      core.update((s) => ({ ...s, workflowId, loading: false }));

      const applyWorkflowData = (data: unknown) => {
        const wfData = asValidRecord(data, "workflow_id", "status");
        if (!wfData || !WORKFLOW_STATUSES.has(wfData.status as string)) {
          core.update((s) => ({ ...s, status: "failed", error: "Invalid workflow update", loading: false }));
          return;
        }
        const rawSteps = Array.isArray(wfData.steps) ? wfData.steps : [];
        core.set({
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
        });
      };

      clientSubId = crypto.randomUUID();
      // Register the callback before the server registration so the subscription
      // survives a reconnect-driven re-registration.
      unsubscribeFn = client._subscribe(`wf:${clientSubId}`, applyWorkflowData);
      const initialData = await client._registerWorkflow(clientSubId, workflowId);
      if (initialData) applyWorkflowData(initialData);
    } catch (e) {
      core.update((s) => ({ ...s, status: "failed", error: (e as Error).message, loading: false }));
    }
  };

  startWorkflow();

  const release = () => {
    unsubscribeFn = null;
    if (clientSubId) {
      client._unregisterWorkflow(clientSubId);
      clientSubId = null;
    }
  };

  return {
    subscribe: (run) => core.subscribe(run, release),
    unsubscribe: release,
  };
}

/** Fire-and-forget mutation. Errors go to the per-call `onError` or the global `onMutationError`. */
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
  fire: (args: TArgs) => void;
  data: Readable<TData | null>;
}

/** Optimistic mutation over a live subscription; auto-reverts on error or TTL expiry (default 3s). */
export function createOptimisticMutation<TArgs, TResult, TData>(
  mutationFn: (args: TArgs) => Promise<TResult>,
  subscription: SubscriptionStore<TData>,
  apply: (data: TData, args: TArgs) => TData,
  options?: { ttlMs?: number },
): OptimisticMutationStore<TArgs, TData> {
  const ttlMs = options?.ttlMs ?? 3000;
  const client = getForgeClient();
  const core = createStoreCore<TData | null>(null);
  let latestSubData: TData | null = null;
  let pendingGeneration = 0;
  let ttlTimer: ReturnType<typeof setTimeout> | null = null;

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
    core.set(result.data);
  });

  const data: Readable<TData | null> = {
    subscribe: (run) =>
      core.subscribe(run, () => {
        unsubscribeSub();
        if (ttlTimer) clearTimeout(ttlTimer);
      }),
  };

  function fire(args: TArgs): void {
    const snapshot = core.get();

    if (snapshot !== null) {
      core.set(apply(snapshot, args));
    }

    const generation = ++pendingGeneration;

    if (ttlTimer) clearTimeout(ttlTimer);
    ttlTimer = setTimeout(() => {
      if (pendingGeneration === generation) {
        pendingGeneration = 0;
        core.set(latestSubData);
      }
    }, ttlMs);

    mutationFn(args).catch((err: unknown) => {
      if (pendingGeneration === generation) {
        pendingGeneration = 0;
        if (ttlTimer) {
          clearTimeout(ttlTimer);
          ttlTimer = null;
        }
        core.set(snapshot);
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

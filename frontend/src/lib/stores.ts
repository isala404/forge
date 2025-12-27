import { getForgeClient } from './context.js';
import { isForgeDebugEnabled } from './client.js';
import type {
  QueryResult,
  SubscriptionResult,
  ForgeError,
  QueryFn,
  MutationFn,
  ActionFn,
  ForgeClientInterface
} from './types.js';

/**
 * Internal debug log function.
 */
function debugLog(...args: unknown[]): void {
  if (isForgeDebugEnabled()) {
    console.log('[FORGE]', ...args);
  }
}

/**
 * Readable store interface compatible with Svelte's store contract.
 */
export interface Readable<T> {
  subscribe: (run: (value: T) => void) => () => void;
}

/**
 * Query store with refetch capability.
 */
export interface QueryStore<T> extends Readable<QueryResult<T>> {
  refetch: () => Promise<void>;
}

/**
 * Subscription store with unsubscribe capability.
 */
export interface SubscriptionStore<T> extends Readable<SubscriptionResult<T>> {
  refetch: () => Promise<void>;
  unsubscribe: () => void;
  /** Get current value (for optimistic updates) */
  get: () => SubscriptionResult<T>;
  /** Set data directly (for optimistic updates) */
  set: (data: T) => void;
}

/**
 * Create a query store that fetches data from the server.
 * The query is executed immediately and the result is stored.
 *
 * @example
 * ```svelte
 * <script>
 *   import { query } from '@forge/svelte';
 *   import { getProjects } from '$lib/forge/api';
 *
 *   const projects = query(getProjects, { userId: 'abc' });
 * </script>
 *
 * {#if $projects.loading}
 *   <Spinner />
 * {:else if $projects.error}
 *   <Error message={$projects.error.message} />
 * {:else}
 *   {#each $projects.data as project}
 *     <ProjectCard {project} />
 *   {/each}
 * {/if}
 * ```
 */
export function query<TArgs, TResult>(
  fn: QueryFn<TArgs, TResult>,
  args: TArgs | (() => TArgs)
): QueryStore<TResult> {
  const client = getForgeClient();
  const subscribers = new Set<(value: QueryResult<TResult>) => void>();

  let state: QueryResult<TResult> = {
    loading: true,
    data: null,
    error: null,
  };

  const notify = () => {
    subscribers.forEach(run => run(state));
  };

  const fetchData = async () => {
    state = { ...state, loading: true, error: null };
    notify();

    try {
      const currentArgs = typeof args === 'function' ? (args as () => TArgs)() : args;
      const data = await fn(client, currentArgs);
      state = { loading: false, data, error: null };
    } catch (e) {
      const error = e as ForgeError;
      state = { loading: false, data: null, error };
    }
    notify();
  };

  // Initial fetch
  fetchData();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => subscribers.delete(run);
    },
    refetch: fetchData,
  };
}

/**
 * Create a subscription store that receives real-time updates.
 * The query is executed immediately and then subscribes to updates.
 *
 * @example
 * ```svelte
 * <script>
 *   import { subscribe } from '@forge/svelte';
 *   import { getProjects } from '$lib/forge/api';
 *
 *   const projects = subscribe(getProjects, { userId: user.id });
 * </script>
 *
 * <!-- This list updates in real-time! -->
 * {#each $projects.data ?? [] as project (project.id)}
 *   <ProjectCard {project} />
 * {/each}
 * ```
 */
export function subscribe<TArgs, TResult>(
  fn: QueryFn<TArgs, TResult>,
  args: TArgs | (() => TArgs)
): SubscriptionStore<TResult> {
  const client = getForgeClient();
  const subscribers = new Set<(value: SubscriptionResult<TResult>) => void>();
  let unsubscribeFn: (() => void) | null = null;

  let state: SubscriptionResult<TResult> = {
    loading: true,
    data: null,
    error: null,
    stale: false,
  };

  const notify = () => {
    subscribers.forEach(run => run(state));
  };

  const startSubscription = async () => {
    debugLog('Starting subscription for:', fn.functionName);

    // Clean up previous subscription
    if (unsubscribeFn) {
      unsubscribeFn();
      unsubscribeFn = null;
    }

    state = { ...state, loading: true, error: null, stale: false };
    notify();

    try {
      // First, get initial data
      const currentArgs = typeof args === 'function' ? (args as () => TArgs)() : args;
      debugLog('Fetching initial data for:', fn.functionName, 'args:', currentArgs);
      const initialData = await fn(client, currentArgs);
      debugLog('Got initial data for:', fn.functionName, 'data:', initialData);
      state = { loading: false, data: initialData, error: null, stale: false };
      notify();

      // Then subscribe for updates
      debugLog('Setting up WebSocket subscription for:', fn.functionName);
      unsubscribeFn = client.subscribe(
        fn.functionName,
        currentArgs,
        (data: TResult) => {
          debugLog('Received real-time update for:', fn.functionName, 'data:', data);
          state = { loading: false, data, error: null, stale: false };
          notify();
        }
      );
      debugLog('WebSocket subscription set up for:', fn.functionName);
    } catch (e) {
      const error = e as ForgeError;
      console.error('[FORGE] Subscription error for:', fn.functionName, error);
      state = { loading: false, data: null, error, stale: false };
      notify();
    }
  };

  // Start subscription
  startSubscription();

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => {
        subscribers.delete(run);
        // Clean up WebSocket subscription when no more Svelte subscribers
        if (subscribers.size === 0 && unsubscribeFn) {
          unsubscribeFn();
          unsubscribeFn = null;
        }
      };
    },
    refetch: startSubscription,
    unsubscribe: () => {
      if (unsubscribeFn) {
        unsubscribeFn();
        unsubscribeFn = null;
      }
    },
    get: () => state,
    set: (data: TResult) => {
      state = { ...state, data };
      notify();
    },
  };
}

/**
 * Execute a mutation.
 *
 * @example
 * ```svelte
 * <script>
 *   import { mutate } from '@forge/svelte';
 *   import { createProject } from '$lib/forge/api';
 *
 *   async function handleSubmit() {
 *     const project = await mutate(createProject, { name });
 *     goto(`/projects/${project.id}`);
 *   }
 * </script>
 * ```
 */
export async function mutate<TArgs, TResult>(
  fn: MutationFn<TArgs, TResult>,
  args: TArgs
): Promise<TResult> {
  const client = getForgeClient();
  return fn(client, args);
}

/**
 * Execute an action (external API call).
 *
 * @example
 * ```svelte
 * <script>
 *   import { action } from '@forge/svelte';
 *   import { syncWithStripe } from '$lib/forge/api';
 *
 *   async function handleSync() {
 *     const result = await action(syncWithStripe, { userId: user.id });
 *     toast.success('Synced successfully');
 *   }
 * </script>
 * ```
 */
export async function action<TArgs, TResult>(
  fn: ActionFn<TArgs, TResult>,
  args: TArgs
): Promise<TResult> {
  const client = getForgeClient();
  return fn(client, args);
}

/**
 * Optimistic update options.
 */
export interface OptimisticOptions<TArgs, TResult, TData> {
  input: TArgs;
  optimistic: (current: TData) => TData;
  rollback?: (current: TData, error: ForgeError) => TData;
}

/**
 * Execute a mutation with optimistic updates.
 * Updates the store immediately with the optimistic value,
 * then applies the real result or rolls back on error.
 *
 * @example
 * ```svelte
 * <script>
 *   import { mutateOptimistic } from '@forge/svelte';
 *   import { updateProject } from '$lib/forge/api';
 *
 *   async function handleRename(newName: string) {
 *     await mutateOptimistic(updateProject, projectStore, {
 *       input: { id: project.id, name: newName },
 *       optimistic: (current) => ({ ...current, name: newName }),
 *       rollback: (current, error) => {
 *         toast.error(error.message);
 *         return current;
 *       }
 *     });
 *   }
 * </script>
 * ```
 */
export async function mutateOptimistic<TArgs, TResult, TData>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TData>,
  options: OptimisticOptions<TArgs, TResult, TData>
): Promise<TResult> {
  const client = getForgeClient();

  // 1. Get current store value for potential rollback
  const previousState = store.get();
  const previousData = previousState.data;

  // 2. Apply optimistic update immediately
  if (previousData !== null) {
    const optimisticData = options.optimistic(previousData);
    store.set(optimisticData);
  }

  try {
    // 3. Execute the actual mutation
    const result = await fn(client, options.input);

    // 4. Mutation succeeded - the subscription will update with real data
    // or we can apply the result directly if needed
    return result;
  } catch (e) {
    // 5. Mutation failed - rollback to previous state
    const error = e as ForgeError;

    if (previousData !== null) {
      if (options.rollback) {
        // Use custom rollback function
        const rolledBackData = options.rollback(previousData, error);
        store.set(rolledBackData);
      } else {
        // Default: restore previous data
        store.set(previousData);
      }
    }

    throw e;
  }
}

/**
 * Execute a mutation with optimistic list update (add item).
 * Useful for adding items to a list with immediate UI feedback.
 *
 * @example
 * ```svelte
 * <script>
 *   import { mutateOptimisticAdd } from '@forge/svelte';
 *   import { createProject } from '$lib/forge/api';
 *
 *   async function handleCreate() {
 *     await mutateOptimisticAdd(createProject, projectsStore, {
 *       input: { name: newName },
 *       optimisticItem: { id: tempId(), name: newName, createdAt: new Date() },
 *       getId: (item) => item.id,
 *     });
 *   }
 * </script>
 * ```
 */
export async function mutateOptimisticAdd<TArgs, TItem, TResult extends TItem>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TItem[]>,
  options: {
    input: TArgs;
    optimisticItem: TItem;
    getId: (item: TItem) => string;
    position?: 'start' | 'end';
  }
): Promise<TResult> {
  const client = getForgeClient();
  const previousState = store.get();
  const previousData = previousState.data ?? [];

  // Add optimistic item
  const optimisticList =
    options.position === 'start'
      ? [options.optimisticItem, ...previousData]
      : [...previousData, options.optimisticItem];
  store.set(optimisticList);

  try {
    const result = await fn(client, options.input);

    // Replace optimistic item with real item
    const optimisticId = options.getId(options.optimisticItem);
    const updatedList = previousData
      .filter((item) => options.getId(item) !== optimisticId)
      .concat([result]);
    store.set(
      options.position === 'start' ? [result, ...previousData] : [...previousData, result]
    );

    return result;
  } catch (e) {
    // Rollback: remove optimistic item
    store.set(previousData);
    throw e;
  }
}

/**
 * Execute a mutation with optimistic list update (remove item).
 * Useful for removing items from a list with immediate UI feedback.
 *
 * @example
 * ```svelte
 * <script>
 *   import { mutateOptimisticRemove } from '@forge/svelte';
 *   import { deleteProject } from '$lib/forge/api';
 *
 *   async function handleDelete(projectId: string) {
 *     await mutateOptimisticRemove(deleteProject, projectsStore, {
 *       input: { id: projectId },
 *       itemId: projectId,
 *       getId: (item) => item.id,
 *     });
 *   }
 * </script>
 * ```
 */
export async function mutateOptimisticRemove<TArgs, TItem, TResult>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TItem[]>,
  options: {
    input: TArgs;
    itemId: string;
    getId: (item: TItem) => string;
  }
): Promise<TResult> {
  const client = getForgeClient();
  const previousState = store.get();
  const previousData = previousState.data ?? [];

  // Optimistically remove the item
  const optimisticList = previousData.filter((item) => options.getId(item) !== options.itemId);
  store.set(optimisticList);

  try {
    const result = await fn(client, options.input);
    return result;
  } catch (e) {
    // Rollback: restore the item
    store.set(previousData);
    throw e;
  }
}

/**
 * Execute a mutation with optimistic list update (update item).
 * Useful for updating items in a list with immediate UI feedback.
 *
 * @example
 * ```svelte
 * <script>
 *   import { mutateOptimisticUpdate } from '@forge/svelte';
 *   import { updateProject } from '$lib/forge/api';
 *
 *   async function handleUpdate(projectId: string, name: string) {
 *     await mutateOptimisticUpdate(updateProject, projectsStore, {
 *       input: { id: projectId, name },
 *       itemId: projectId,
 *       getId: (item) => item.id,
 *       update: (item) => ({ ...item, name }),
 *     });
 *   }
 * </script>
 * ```
 */
export async function mutateOptimisticUpdate<TArgs, TItem, TResult extends TItem>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TItem[]>,
  options: {
    input: TArgs;
    itemId: string;
    getId: (item: TItem) => string;
    update: (item: TItem) => TItem;
  }
): Promise<TResult> {
  const client = getForgeClient();
  const previousState = store.get();
  const previousData = previousState.data ?? [];

  // Optimistically update the item
  const optimisticList = previousData.map((item) =>
    options.getId(item) === options.itemId ? options.update(item) : item
  );
  store.set(optimisticList);

  try {
    const result = await fn(client, options.input);

    // Update with real result
    const updatedList = previousData.map((item) =>
      options.getId(item) === options.itemId ? result : item
    );
    store.set(updatedList);

    return result;
  } catch (e) {
    // Rollback: restore original item
    store.set(previousData);
    throw e;
  }
}

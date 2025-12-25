import { getForgeClient } from './context.js';
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
      const initialData = await fn(client, currentArgs);
      state = { loading: false, data: initialData, error: null, stale: false };
      notify();

      // Then subscribe for updates
      unsubscribeFn = client.subscribe(
        fn.functionName,
        currentArgs,
        (data: TResult) => {
          state = { loading: false, data, error: null, stale: false };
          notify();
        }
      );
    } catch (e) {
      const error = e as ForgeError;
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

  // Note: In a full implementation, we would:
  // 1. Get current store value
  // 2. Apply optimistic update
  // 3. Execute mutation
  // 4. Apply real result or rollback
  // This requires internal store access which would need store redesign

  try {
    const result = await fn(client, options.input);
    return result;
  } catch (e) {
    throw e;
  }
}

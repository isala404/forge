import type { ForgeClientInterface, QueryFn, MutationFn, ActionFn } from './types.js';

/**
 * Create a query function binding.
 * Used by generated code to create type-safe query functions.
 *
 * @example
 * ```typescript
 * // Generated code
 * export const getProjects = createQuery<GetProjectsArgs, Project[]>('get_projects');
 *
 * // Usage
 * const projects = query(getProjects, { userId: 'abc' });
 * ```
 */
export function createQuery<TArgs, TResult>(
  name: string
): QueryFn<TArgs, TResult> {
  const fn = async (client: ForgeClientInterface, args: TArgs): Promise<TResult> => {
    return client.call(name, args);
  };
  (fn as QueryFn<TArgs, TResult>).functionName = name;
  (fn as QueryFn<TArgs, TResult>).functionType = 'query';
  return fn as QueryFn<TArgs, TResult>;
}

/**
 * Create a mutation function binding.
 * Used by generated code to create type-safe mutation functions.
 *
 * @example
 * ```typescript
 * // Generated code
 * export const createProject = createMutation<CreateProjectInput, Project>('create_project');
 *
 * // Usage
 * const project = await mutate(createProject, { name: 'My Project' });
 * ```
 */
export function createMutation<TArgs, TResult>(
  name: string
): MutationFn<TArgs, TResult> {
  const fn = async (client: ForgeClientInterface, args: TArgs): Promise<TResult> => {
    return client.call(name, args);
  };
  (fn as MutationFn<TArgs, TResult>).functionName = name;
  (fn as MutationFn<TArgs, TResult>).functionType = 'mutation';
  return fn as MutationFn<TArgs, TResult>;
}

/**
 * Create an action function binding.
 * Used by generated code to create type-safe action functions.
 *
 * @example
 * ```typescript
 * // Generated code
 * export const syncWithStripe = createAction<SyncInput, SyncResult>('sync_with_stripe');
 *
 * // Usage
 * const result = await action(syncWithStripe, { userId: 'abc' });
 * ```
 */
export function createAction<TArgs, TResult>(
  name: string
): ActionFn<TArgs, TResult> {
  const fn = async (client: ForgeClientInterface, args: TArgs): Promise<TResult> => {
    return client.call(name, args);
  };
  (fn as ActionFn<TArgs, TResult>).functionName = name;
  (fn as ActionFn<TArgs, TResult>).functionType = 'action';
  return fn as ActionFn<TArgs, TResult>;
}

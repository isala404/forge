/**
 * @forge/svelte - FORGE Svelte 5 runtime library
 *
 * Provides seamless integration between FORGE backend and Svelte 5 frontends.
 *
 * @example
 * ```svelte
 * <!-- +layout.svelte -->
 * <script>
 *   import { ForgeProvider } from '@forge/svelte';
 *   let { children } = $props();
 * </script>
 *
 * <ForgeProvider url="http://localhost:8080">
 *   {@render children()}
 * </ForgeProvider>
 * ```
 *
 * @example
 * ```svelte
 * <!-- +page.svelte -->
 * <script>
 *   import { query, subscribe, mutate } from '@forge/svelte';
 *   import { getProjects, createProject } from '$lib/forge/api';
 *
 *   const projects = subscribe(getProjects, { userId: 'abc' });
 * </script>
 *
 * {#each $projects.data ?? [] as project}
 *   <ProjectCard {project} />
 * {/each}
 * ```
 */

// Components
export { default as ForgeProvider } from './ForgeProvider.svelte';

// Client
export {
  ForgeClient,
  ForgeClientError,
  createForgeClient,
  type ForgeClientConfig,
} from './client.js';

// Context
export {
  getForgeClient,
  setForgeClient,
  getAuthState,
  setAuthState,
} from './context.js';

// Stores
export {
  query,
  subscribe,
  mutate,
  action,
  mutateOptimistic,
  type Readable,
  type QueryStore,
  type SubscriptionStore,
  type OptimisticOptions,
} from './stores.js';

// Auth
export {
  createAuthStore,
  createPersistentAuthStore,
  useAuth,
  type AuthStore,
} from './auth.js';

// API helpers (for generated code)
export {
  createQuery,
  createMutation,
  createAction,
} from './api.js';

// Types
export type {
  ForgeError,
  QueryResult,
  SubscriptionResult,
  Paginated,
  Page,
  SortOrder,
  ConnectionState,
  AuthState,
  QueryFn,
  MutationFn,
  ActionFn,
  ForgeClientInterface,
} from './types.js';

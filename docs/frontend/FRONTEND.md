# FORGE Frontend System

This document describes the actual implementation of the FORGE frontend system, including the TypeScript client library, Svelte 5 integration, and code generation pipeline.

## Architecture Overview

The FORGE frontend system consists of three main layers:

1. **Runtime Library** (`frontend/src/lib/` or generated `.forge/svelte/`)
   - Core TypeScript client for HTTP and WebSocket communication
   - Svelte 5 context management
   - Reactive store system compatible with Svelte's store contract
   - Authentication utilities

2. **Code Generation** (`crates/forge-codegen/src/typescript/`)
   - Generates TypeScript types from Rust schema
   - Generates API bindings for queries, mutations, and actions
   - Generates client and store boilerplate

3. **CLI Scaffolding** (`crates/forge/src/cli/`)
   - `forge new` creates full project with embedded `@forge/svelte` runtime
   - `forge generate` regenerates runtime when CLI version changes
   - Runtime is stored in `.forge/svelte/` as a local npm package

## File Structure

Generated projects have this structure:

```
frontend/
├── .forge/
│   ├── version              # FORGE CLI version for update detection
│   └── svelte/              # @forge/svelte package
│       ├── package.json
│       ├── index.ts
│       ├── types.ts
│       ├── client.ts
│       ├── context.ts
│       ├── stores.ts
│       ├── api.ts
│       └── ForgeProvider.svelte
├── src/
│   ├── lib/forge/
│   │   ├── types.ts         # Project-specific types (User, etc.)
│   │   ├── api.ts           # Function bindings (getUsers, createUser, etc.)
│   │   └── index.ts         # Re-exports
│   └── routes/
│       ├── +layout.svelte   # ForgeProvider wrapper
│       └── +page.svelte     # Example page
└── package.json             # References @forge/svelte via file:
```

## ForgeClient Class

Location: `frontend/src/lib/client.ts` (source) or generated `.forge/svelte/client.ts`

The `ForgeClient` class handles all communication with the FORGE backend.

### Configuration

```typescript
interface ForgeClientConfig {
  url: string;                                    // Backend URL (e.g., "http://localhost:8080")
  getToken?: () => string | null | Promise<string | null>;  // Auth token provider
  onAuthError?: (error: ForgeError) => void;      // Called on 401/403 responses
  timeout?: number;                               // Request timeout in ms (default: 30000)
  debug?: boolean;                                // Enable debug logging
}
```

### Creating a Client

```typescript
import { createForgeClient, ForgeClient } from '@forge/svelte';

const client = createForgeClient({
  url: 'http://localhost:8080',
  getToken: () => localStorage.getItem('token'),
  onAuthError: (err) => {
    console.error('Auth error:', err);
    window.location.href = '/login';
  },
});
```

### HTTP RPC Calls

The `call` method makes HTTP POST requests to `/rpc/{functionName}`:

```typescript
// Call a function and get typed response
const users = await client.call<User[]>('get_users', {});
const user = await client.call<User>('create_user', { name: 'Alice', email: 'alice@example.com' });
```

Request format:
- **URL**: `POST /rpc/{functionName}`
- **Headers**: `Content-Type: application/json`, optional `Authorization: Bearer {token}`
- **Body**: JSON-serialized args (empty objects `{}` normalized to `null` for Rust unit type)

Response format:
```typescript
interface RpcResponse<T> {
  success: boolean;
  data?: T;
  error?: ForgeError;
}
```

### WebSocket Subscriptions

The `subscribe` method creates real-time subscriptions via WebSocket:

```typescript
const unsubscribe = client.subscribe<User[]>(
  'get_users',
  {},
  (data) => {
    console.log('Users updated:', data);
  }
);

// Later: clean up
unsubscribe();
```

WebSocket protocol:
- **Connect**: `ws://{url}/ws`
- **Subscribe**: `{ type: 'subscribe', id: '{subscriptionId}', function: '{name}', args: {...} }`
- **Unsubscribe**: `{ type: 'unsubscribe', id: '{subscriptionId}' }`
- **Auth**: `{ type: 'auth', token: '{jwt}' }`
- **Data updates**: `{ type: 'data', id: '{subscriptionId}', data: {...} }`

### Connection Management

```typescript
// Connect to WebSocket (optional - HTTP RPC works without it)
await client.connect();

// Check connection state
const state = client.getConnectionState();
// Returns: 'connecting' | 'connected' | 'reconnecting' | 'disconnected'

// Listen for connection changes
const unsubscribe = client.onConnectionStateChange((state) => {
  console.log('Connection state:', state);
});

// Disconnect
client.disconnect();
```

### Reconnection Behavior

- WebSocket connection is optional; HTTP RPC works regardless
- If WebSocket connects successfully, automatic reconnection is enabled
- Exponential backoff: 1s, 2s, 4s, 8s... up to 30s
- Maximum 10 reconnection attempts
- Pending subscriptions are queued and sent when connection reopens

### Job and Workflow Subscriptions

The client supports subscribing to job and workflow progress updates:

```typescript
// Subscribe to job progress
const unsubJob = client.subscribeJob(jobId, (progress) => {
  console.log(`Job ${progress.job_id}: ${progress.progress_percent}%`);
});

// Subscribe to workflow progress
const unsubWorkflow = client.subscribeWorkflow(workflowId, (progress) => {
  console.log(`Workflow ${progress.workflow_id}: ${progress.status}`);
  console.log('Steps:', progress.steps);
});
```

## ForgeProvider Component

Location: `frontend/src/lib/ForgeProvider.svelte` or `.forge/svelte/ForgeProvider.svelte`

The `ForgeProvider` is a Svelte 5 component that provides the client and auth state via context.

### Usage

```svelte
<script lang="ts">
  import { ForgeProvider } from '@forge/svelte';

  interface Props {
    children: import('svelte').Snippet;
  }

  let { children }: Props = $props();
  const apiUrl = import.meta.env.VITE_API_URL || 'http://localhost:8080';
</script>

<ForgeProvider url={apiUrl}>
  {@render children()}
</ForgeProvider>
```

### Props

```typescript
interface Props {
  url: string;                                           // Backend URL
  getToken?: () => string | null | Promise<string | null>;  // Auth token provider
  onAuthError?: (error: ForgeError) => void;             // Auth error handler
  onConnectionChange?: (state: ConnectionState) => void; // Connection state changes
  debug?: boolean;                                       // Enable debug logging
  children: Snippet;                                     // Child components
}
```

### Internal Behavior

1. **Immediate Context Setting**: Client and auth state are set during component initialization (not in `onMount`), ensuring child components can access them immediately.

2. **Auth State**: Uses `$state` rune for reactive auth tracking:
   ```typescript
   const authState: AuthState = $state({
     user: null,
     token: null,
     loading: true,
   });
   ```

3. **Connection**: WebSocket connection is initiated in `onMount`, with connection state tracked.

4. **Cleanup**: Client disconnects in `onDestroy`.

## Context Management

Location: `frontend/src/lib/context.ts` or `.forge/svelte/context.ts`

### Functions

```typescript
// Get client from context (works in component init) or global (works in event handlers)
export function getForgeClient(): ForgeClient;

// Set client in context (used by ForgeProvider)
export function setForgeClient(client: ForgeClient): void;

// Get auth state from context
export function getAuthState(): AuthState;

// Set auth state in context
export function setAuthState(auth: AuthState): void;
```

### Global Client Reference

The context module maintains a global client reference for use outside component initialization:

```typescript
let globalClient: ForgeClient | null = null;

export function getForgeClient(): ForgeClient {
  // Try context first (works during component initialization)
  try {
    const client = getContext<ForgeClient>(FORGE_CLIENT_KEY);
    if (client) return client;
  } catch {}

  // Fall back to global client (works in event handlers)
  if (globalClient) return globalClient;

  throw new Error('FORGE client not found. Wrap your component with ForgeProvider.');
}
```

This enables `mutate()` and `action()` to work in event handlers, not just during component init.

## Store Functions

Location: `frontend/src/lib/stores.ts` or `.forge/svelte/stores.ts`

### query()

Fetches data once and returns a reactive store:

```typescript
function query<TArgs, TResult>(
  fn: QueryFn<TArgs, TResult>,
  args: TArgs | (() => TArgs)
): QueryStore<TResult>;

interface QueryStore<T> extends Readable<QueryResult<T>> {
  refetch: () => Promise<void>;
}

interface QueryResult<T> {
  loading: boolean;
  data: T | null;
  error: ForgeError | null;
}
```

Usage:
```svelte
<script>
  import { query } from '@forge/svelte';
  import { getUsers } from '$lib/forge/api';

  const users = query(getUsers, {});
</script>

{#if $users.loading}
  <p>Loading...</p>
{:else if $users.error}
  <p>Error: {$users.error.message}</p>
{:else}
  {#each $users.data as user}
    <p>{user.name}</p>
  {/each}
{/if}
```

### subscribe()

Creates a real-time subscription with automatic WebSocket updates:

```typescript
function subscribe<TArgs, TResult>(
  fn: QueryFn<TArgs, TResult>,
  args: TArgs | (() => TArgs)
): SubscriptionStore<TResult>;

interface SubscriptionStore<T> extends Readable<SubscriptionResult<T>> {
  refetch: () => Promise<void>;
  unsubscribe: () => void;
  get: () => SubscriptionResult<T>;   // For optimistic updates
  set: (data: T) => void;              // For optimistic updates
}

interface SubscriptionResult<T> extends QueryResult<T> {
  stale: boolean;
}
```

Usage:
```svelte
<script>
  import { subscribe } from '@forge/svelte';
  import { getUsers } from '$lib/forge/api';

  // Updates automatically when data changes on the server!
  const users = subscribe(getUsers, {});
</script>

{#each $users.data ?? [] as user}
  <p>{user.name}</p>
{/each}
```

### mutate()

Executes a mutation:

```typescript
async function mutate<TArgs, TResult>(
  fn: MutationFn<TArgs, TResult>,
  args: TArgs
): Promise<TResult>;
```

Usage:
```svelte
<script>
  import { mutate } from '@forge/svelte';
  import { createUser } from '$lib/forge/api';

  async function handleCreate() {
    const user = await mutate(createUser, { name: 'Alice', email: 'alice@example.com' });
    console.log('Created:', user);
  }
</script>

<button onclick={handleCreate}>Create User</button>
```

### action()

Executes an action (external API call):

```typescript
async function action<TArgs, TResult>(
  fn: ActionFn<TArgs, TResult>,
  args: TArgs
): Promise<TResult>;
```

Usage:
```svelte
<script>
  import { action } from '@forge/svelte';
  import { syncWithStripe } from '$lib/forge/api';

  async function handleSync() {
    await action(syncWithStripe, { userId: 'abc123' });
  }
</script>
```

### Optimistic Update Helpers

For instant UI feedback with rollback on failure:

```typescript
// Generic optimistic update
async function mutateOptimistic<TArgs, TResult, TData>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TData>,
  options: {
    input: TArgs;
    optimistic: (current: TData) => TData;
    rollback?: (current: TData, error: ForgeError) => TData;
  }
): Promise<TResult>;

// Add item to list
async function mutateOptimisticAdd<TArgs, TItem, TResult>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TItem[]>,
  options: {
    input: TArgs;
    optimisticItem: TItem;
    getId: (item: TItem) => string;
    position?: 'start' | 'end';
  }
): Promise<TResult>;

// Remove item from list
async function mutateOptimisticRemove<TArgs, TItem, TResult>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TItem[]>,
  options: {
    input: TArgs;
    itemId: string;
    getId: (item: TItem) => string;
  }
): Promise<TResult>;

// Update item in list
async function mutateOptimisticUpdate<TArgs, TItem, TResult>(
  fn: MutationFn<TArgs, TResult>,
  store: SubscriptionStore<TItem[]>,
  options: {
    input: TArgs;
    itemId: string;
    getId: (item: TItem) => string;
    update: (item: TItem) => TItem;
  }
): Promise<TResult>;
```

## Job and Workflow Trackers

Location: `.forge/svelte/stores.ts`

### createJobTracker()

Factory function for tracking background job progress:

```typescript
interface JobTracker<TArgs> extends Readable<JobProgress | null> {
  start: (args: TArgs) => Promise<string>;  // Returns job ID
  resume: (jobId: string) => void;          // Resume tracking existing job
  cleanup: () => void;                       // Cleanup subscription
}

interface JobProgress {
  job_id: string;
  status: JobStatus;
  progress_percent: number | null;
  progress_message: string | null;
  output: unknown | null;
  error: string | null;
}

type JobStatus = 'pending' | 'claimed' | 'running' | 'completed' | 'retry' | 'failed' | 'dead_letter';
```

Usage:
```svelte
<script>
  import { onDestroy } from 'svelte';
  import { createJobTracker } from '@forge/svelte';

  interface ExportArgs {
    format: 'csv' | 'json';
  }

  const exportJob = createJobTracker<ExportArgs>('export_users', 'http://localhost:8080');
  onDestroy(() => exportJob.cleanup());

  async function startExport() {
    const jobId = await exportJob.start({ format: 'csv' });
    localStorage.setItem('active_job_id', jobId);
  }

  // Resume on page load
  const savedJobId = localStorage.getItem('active_job_id');
  if (savedJobId) exportJob.resume(savedJobId);
</script>

{#if $exportJob}
  <div class="progress-bar" style="width: {$exportJob.progress_percent ?? 0}%"></div>
  <p>{$exportJob.progress_message ?? $exportJob.status}</p>
{:else}
  <button onclick={startExport}>Start Export</button>
{/if}
```

### createWorkflowTracker()

Factory function for tracking multi-step workflow progress:

```typescript
interface WorkflowTracker<TArgs> extends Readable<WorkflowProgress | null> {
  start: (args: TArgs) => Promise<string>;  // Returns workflow ID
  resume: (workflowId: string) => void;     // Resume tracking existing workflow
  cleanup: () => void;                       // Cleanup subscription
}

interface WorkflowProgress {
  workflow_id: string;
  status: WorkflowStatus;
  current_step: string | null;
  steps: WorkflowStep[];
  output: unknown | null;
  error: string | null;
}

interface WorkflowStep {
  name: string;
  status: WorkflowStepStatus;
  started_at: string | null;
  completed_at: string | null;
  error: string | null;
}

type WorkflowStatus = 'created' | 'running' | 'waiting' | 'completed' | 'compensating' | 'compensated' | 'failed';
type WorkflowStepStatus = 'pending' | 'running' | 'completed' | 'failed' | 'compensated' | 'skipped';
```

Usage:
```svelte
<script>
  import { onDestroy } from 'svelte';
  import { createWorkflowTracker } from '@forge/svelte';

  interface VerifyArgs {
    user_id: string;
  }

  const verifyWorkflow = createWorkflowTracker<VerifyArgs>('account_verification', 'http://localhost:8080');
  onDestroy(() => verifyWorkflow.cleanup());

  async function startVerification() {
    const workflowId = await verifyWorkflow.start({ user_id: 'abc123' });
    localStorage.setItem('active_workflow_id', workflowId);
  }
</script>

{#if $verifyWorkflow}
  <div class="steps">
    {#each $verifyWorkflow.steps as step}
      <div class="step {step.status}">
        <span>{step.name}</span>
        <span>{step.status}</span>
      </div>
    {/each}
  </div>
{:else}
  <button onclick={startVerification}>Start Verification</button>
{/if}
```

## Auth Store

Location: `frontend/src/lib/auth.ts`

### createAuthStore()

Creates a reactive authentication store:

```typescript
interface AuthStore extends Readable<AuthState> {
  login: (token: string, user?: unknown) => void;
  logout: () => void;
  setUser: (user: unknown) => void;
}

interface AuthState {
  user: unknown | null;
  token: string | null;
  loading: boolean;
}

function createAuthStore(
  initialToken?: string | null,
  initialUser?: unknown
): AuthStore;
```

### createPersistentAuthStore()

Creates an auth store that persists to localStorage:

```typescript
function createPersistentAuthStore(): AuthStore;
```

This automatically:
- Restores token and user from localStorage on creation
- Saves token to `forge_token` and user to `forge_user` on login
- Clears localStorage on logout

Usage:
```svelte
<script>
  import { createPersistentAuthStore } from '@forge/svelte';

  const auth = createPersistentAuthStore();

  async function handleLogin(email: string, password: string) {
    const response = await fetch('/api/login', {
      method: 'POST',
      body: JSON.stringify({ email, password }),
    });
    const { token, user } = await response.json();
    auth.login(token, user);
  }
</script>

{#if $auth.user}
  <p>Welcome, {$auth.user.name}!</p>
  <button onclick={() => auth.logout()}>Logout</button>
{:else}
  <LoginForm onSubmit={handleLogin} />
{/if}
```

## Generated TypeScript Types

Location: `crates/forge-codegen/src/typescript/types.rs`

The `TypeGenerator` converts Rust schema to TypeScript:

### Type Mappings

| Rust Type | TypeScript Type |
|-----------|-----------------|
| `String` | `string` |
| `i32`, `i64`, `f32`, `f64` | `number` |
| `bool` | `boolean` |
| `Uuid` | `string` |
| `DateTime`, `Timestamp` | `string` |
| `Date` | `string` |
| `Json` | `unknown` |
| `Vec<u8>` | `Uint8Array` |
| `Option<T>` | `T \| null` |
| `Vec<T>` | `T[]` |
| `()` | `void` |

### Field Name Conversion

Rust `snake_case` fields are converted to TypeScript `camelCase`:
- `created_at` -> `createdAt`
- `user_id` -> `userId`
- `avatar_url` -> `avatarUrl`

### Common Utility Types

Generated in every `types.ts`:

```typescript
interface Paginated<T> {
  data: T[];
  total: number;
  page: number;
  pageSize: number;
  hasMore: boolean;
}

interface Page {
  page: number;
  pageSize: number;
}

interface SortOrder {
  field: string;
  direction: 'asc' | 'desc';
}

interface ForgeError {
  code: string;
  message: string;
  details?: Record<string, unknown>;
}
```

## Generated API Bindings

Location: `crates/forge-codegen/src/typescript/api.rs`

The `ApiGenerator` creates type-safe function bindings:

### Factory Functions

```typescript
// Creates a typed query function
function createQuery<TArgs, TResult>(name: string): QueryFn<TArgs, TResult>;

// Creates a typed mutation function
function createMutation<TArgs, TResult>(name: string): MutationFn<TArgs, TResult>;

// Creates a typed action function
function createAction<TArgs, TResult>(name: string): ActionFn<TArgs, TResult>;
```

### Function Type Interfaces

```typescript
interface QueryFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'query';
}

interface MutationFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'mutation';
}

interface ActionFn<TArgs, TResult> {
  (client: ForgeClientInterface, args: TArgs): Promise<TResult>;
  functionName: string;
  functionType: 'action';
}
```

### Generated Bindings Example

From Rust functions:
```rust
#[forge::query]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> { ... }

#[forge::mutation]
pub async fn create_user(ctx: &MutationContext, email: String, name: String) -> Result<User> { ... }
```

Generated TypeScript:
```typescript
export const getUsers = createQuery<Record<string, never>, User[]>('get_users');
export const createUser = createMutation<{ email: string; name: string }, User>('create_user');
```

## Code Generation Pipeline

### TypeScriptGenerator

Location: `crates/forge-codegen/src/typescript/mod.rs`

The main generator orchestrates all code generation:

```rust
impl TypeScriptGenerator {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self;

    pub fn generate(&self, registry: &SchemaRegistry) -> Result<(), Error>;
}
```

It generates:
- `types.ts` - TypeScript interfaces and enums
- `api.ts` - Function bindings
- `client.ts` - ForgeClient class
- `stores.ts` - Svelte store functions
- `index.ts` - Barrel exports

### Runtime Generator

Location: `crates/forge/src/cli/runtime_generator.rs`

Generates the `.forge/svelte/` package with the complete runtime:

```rust
pub fn generate_runtime(frontend_dir: &Path) -> Result<()>;
pub fn needs_update(frontend_dir: &Path) -> bool;
pub fn get_installed_version(frontend_dir: &Path) -> Option<String>;
```

The runtime includes:
- Version tracking in `.forge/version`
- `package.json` with peer dependency on Svelte 5
- All runtime TypeScript files (client, context, stores, api, types)
- `ForgeProvider.svelte` component

## Common Patterns

### Protected Routes

```svelte
<script>
  import { getAuthState } from '@forge/svelte';
  import { goto } from '$app/navigation';

  const auth = getAuthState();

  $effect(() => {
    if (!auth.loading && !auth.token) {
      goto('/login');
    }
  });
</script>

{#if auth.token}
  <slot />
{/if}
```

### Error Handling

```svelte
<script>
  import { mutate, ForgeClientError } from '@forge/svelte';
  import { createUser } from '$lib/forge/api';

  async function handleCreate() {
    try {
      await mutate(createUser, { name, email });
    } catch (e) {
      if (e instanceof ForgeClientError) {
        switch (e.code) {
          case 'VALIDATION':
            toast.error('Invalid input');
            break;
          case 'UNAUTHORIZED':
            goto('/login');
            break;
          default:
            toast.error(e.message);
        }
      }
    }
  }
</script>
```

### Reactive Arguments

```svelte
<script>
  import { subscribe } from '@forge/svelte';
  import { getProjectTasks } from '$lib/forge/api';

  let { projectId } = $props();

  // Re-subscribes automatically when projectId changes
  const tasks = subscribe(getProjectTasks, () => ({ projectId }));
</script>
```

### Derived State

```svelte
<script>
  import { subscribe } from '@forge/svelte';
  import { getNotifications } from '$lib/forge/api';

  const notifications = subscribe(getNotifications, {});

  const unreadCount = $derived(
    $notifications.data?.filter(n => !n.read).length ?? 0
  );

  $effect(() => {
    document.title = unreadCount > 0 ? `(${unreadCount}) My App` : 'My App';
  });
</script>
```

## Debug Logging

Enable debug logging to troubleshoot issues:

```typescript
import { setForgeDebug } from '@forge/svelte';

// Enable globally
setForgeDebug(true);

// Or via client config
const client = createForgeClient({
  url: 'http://localhost:8080',
  debug: true,
});
```

Debug logs are prefixed with `[FORGE]` and include:
- WebSocket connection events
- Subscription creation/updates
- RPC calls and responses
- Error details

## Differences from Proposal

The implementation closely follows the proposal in `/proposal/frontend/FRONTEND.md` with these notable additions:

1. **Job/Workflow Trackers**: `createJobTracker()` and `createWorkflowTracker()` for background task monitoring (not in original proposal)

2. **Global Client Reference**: Context module maintains a global reference for use in event handlers

3. **Optimistic Update Helpers**: `mutateOptimisticAdd`, `mutateOptimisticRemove`, `mutateOptimisticUpdate` for list operations

4. **Runtime Package**: `.forge/svelte/` as a local npm package instead of embedded files

5. **Version Tracking**: `.forge/version` file for detecting when runtime needs regeneration

6. **Debug Mode**: Comprehensive debug logging system with `setForgeDebug()`

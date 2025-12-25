# Svelte Stores

> *Auto-generated reactive stores*

---

## Overview

FORGE generates Svelte 5 stores from your schema that:

- **Auto-update** when data changes (via subscriptions)
- **Type-safe** — Full TypeScript inference
- **Optimistic** — Instant UI feedback
- **Cached** — Smart caching with invalidation

---

## Generated Stores

From your schema, FORGE generates:

```typescript
// generated/stores.ts

import { createForgeStore } from '$lib/forge/runtime';
import type { User, Project } from './types';

// Reactive stores
export const currentUser = createForgeStore<User | null>('currentUser');
export const projects = createForgeStore<Project[]>('projects');
export const projectById = createForgeStore<Record<string, Project>>('projectById');
```

---

## Using Stores

### Basic Usage (Svelte 5)

```svelte
<script lang="ts">
  import { projects } from '$lib/forge/stores';
  import { subscribe } from '$lib/forge';
  import { get_projects } from '$lib/forge/api';
  
  // Subscribe to query - auto-updates on changes
  const projectsState = subscribe(get_projects, { userId: $page.data.userId });
</script>

{#if $projectsState.loading}
  <Spinner />
{:else if $projectsState.error}
  <ErrorMessage error={$projectsState.error} />
{:else}
  {#each $projectsState.data as project (project.id)}
    <ProjectCard {project} />
  {/each}
{/if}
```

### Svelte 5 Runes

```svelte
<script lang="ts">
  import { subscribe } from '$lib/forge';
  import { get_projects } from '$lib/forge/api';
  
  let userId = $state('');
  
  // Reactive subscription - re-subscribes when userId changes
  const projects = $derived(subscribe(get_projects, { userId }));
</script>

<input bind:value={userId} placeholder="User ID" />

{#if projects.data}
  <p>Found {projects.data.length} projects</p>
{/if}
```

---

## Store State

Every store has consistent state:

```typescript
interface StoreState<T> {
  data: T | null;       // The data
  loading: boolean;     // Initial load in progress
  error: Error | null;  // Error if failed
  stale: boolean;       // Data may be outdated (reconnecting)
  updatedAt: Date;      // Last update time
}
```

---

## Mutations and Store Updates

Mutations automatically update related stores:

```svelte
<script lang="ts">
  import { subscribe, mutate } from '$lib/forge';
  import { get_projects, create_project } from '$lib/forge/api';
  
  const projects = subscribe(get_projects, { userId });
  
  async function handleCreate() {
    // After mutation completes, projects store auto-updates
    await mutate(create_project, { name: 'New Project' });
    // No manual refetch needed!
  }
</script>

<button onclick={handleCreate}>Create Project</button>

{#each $projects.data ?? [] as project}
  <div>{project.name}</div>
{/each}
```

---

## Optimistic Updates

For instant UI feedback:

```svelte
<script lang="ts">
  import { mutateOptimistic } from '$lib/forge';
  
  async function handleDelete(projectId: string) {
    await mutateOptimistic(delete_project, {
      args: { projectId },
      
      // Immediately update UI
      optimistic: (current) => current.filter(p => p.id !== projectId),
      
      // Revert if mutation fails
      rollback: (current, error) => {
        showError('Failed to delete');
        return [...current, deletedProject];
      }
    });
  }
</script>
```

---

## Manual Store Control

```typescript
import { projects } from '$lib/forge/stores';

// Force refresh
await projects.refresh();

// Set data manually
projects.set(newData);

// Update data
projects.update(current => [...current, newProject]);

// Clear
projects.reset();
```

---

## Related Documentation

- [Reactivity](../core/REACTIVITY.md) — How subscriptions work
- [RPC Client](RPC_CLIENT.md) — Calling functions
- [WebSocket](WEBSOCKET.md) — Connection management

# Frontend Integration

> *Svelte 5 + FORGE = Real-time magic*

---

## Overview

FORGE provides a first-class Svelte 5 integration with:

- **Auto-generated types** — From your Rust schema
- **Reactive stores** — Using Svelte 5 runes
- **Type-safe RPC** — Call functions with full type safety
- **Real-time subscriptions** — Automatic updates when data changes

---

## Project Structure

```
frontend/
├── src/
│   ├── lib/
│   │   ├── forge/              # Auto-generated
│   │   │   ├── client.ts       # FORGE client
│   │   │   ├── types.ts        # TypeScript types from schema
│   │   │   ├── api.ts          # Function bindings
│   │   │   └── stores.ts       # Reactive stores
│   │   └── components/
│   ├── routes/
│   │   ├── +layout.svelte
│   │   └── +page.svelte
│   └── app.html
├── svelte.config.js
└── vite.config.ts
```

---

## Setup

### 1. Initialize Frontend

```bash
forge init --frontend svelte
```

This creates the Svelte 5 project with FORGE integration.

### 2. Generate Client

```bash
forge generate
```

Generates TypeScript types and function bindings from your Rust schema.

### 3. Connect to FORGE

```svelte
<!-- src/routes/+layout.svelte -->
<script>
  import { ForgeProvider } from '$lib/forge';
  
  let { children } = $props();
</script>

<ForgeProvider url="http://localhost:8080">
  {@render children()}
</ForgeProvider>
```

---

## Calling Functions

### Queries

```svelte
<script>
  import { query } from '$lib/forge';
  import { getProjects } from '$lib/forge/api';
  
  // One-time query
  const projects = query(getProjects, { userId: 'abc' });
</script>

{#if $projects.loading}
  <Spinner />
{:else if $projects.error}
  <Error message={$projects.error.message} />
{:else}
  {#each $projects.data as project}
    <ProjectCard {project} />
  {/each}
{/if}
```

### Mutations

```svelte
<script>
  import { mutate } from '$lib/forge';
  import { createProject } from '$lib/forge/api';
  
  let name = $state('');
  let loading = $state(false);
  
  async function handleSubmit() {
    loading = true;
    try {
      const project = await mutate(createProject, { name });
      goto(`/projects/${project.id}`);
    } catch (error) {
      toast.error(error.message);
    } finally {
      loading = false;
    }
  }
</script>

<form onsubmit={handleSubmit}>
  <input bind:value={name} placeholder="Project name" />
  <button disabled={loading}>
    {loading ? 'Creating...' : 'Create Project'}
  </button>
</form>
```

### Actions

```svelte
<script>
  import { action } from '$lib/forge';
  import { syncWithStripe } from '$lib/forge/api';
  
  async function handleSync() {
    const result = await action(syncWithStripe, { userId: user.id });
    toast.success('Synced successfully');
  }
</script>
```

---

## Real-Time Subscriptions

```svelte
<script>
  import { subscribe } from '$lib/forge';
  import { getProjects } from '$lib/forge/api';
  
  const user = getContext('user');
  
  // Automatically updates when ANY mutation changes projects
  const projects = subscribe(getProjects, { userId: user.id });
</script>

<!-- This list updates in real-time! -->
{#each $projects.data ?? [] as project (project.id)}
  <ProjectCard {project} />
{/each}
```

### Reactive Arguments

```svelte
<script>
  import { subscribe } from '$lib/forge';
  import { getProjectTasks } from '$lib/forge/api';
  
  let { projectId } = $props();
  
  // Re-subscribes when projectId changes
  const tasks = subscribe(getProjectTasks, () => ({ projectId }));
</script>
```

---

## Svelte 5 Runes Integration

FORGE stores work seamlessly with Svelte 5 runes:

```svelte
<script>
  import { subscribe } from '$lib/forge';
  import { getNotifications } from '$lib/forge/api';
  
  const notifications = subscribe(getNotifications, {});
  
  // Derived state using runes
  const unreadCount = $derived(
    $notifications.data?.filter(n => !n.read).length ?? 0
  );
  
  // Effect when data changes
  $effect(() => {
    if (unreadCount > 0) {
      document.title = `(${unreadCount}) My App`;
    } else {
      document.title = 'My App';
    }
  });
</script>

<NotificationBell count={unreadCount} />
```

---

## Generated Types

From your Rust schema:

```rust
// schema/models.rs
#[forge::model]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub status: ProjectStatus,
    pub owner_id: Uuid,
    pub created_at: Timestamp,
}

#[forge::enum]
pub enum ProjectStatus {
    Draft,
    Active,
    Archived,
}
```

Generated TypeScript:

```typescript
// lib/forge/types.ts
export interface Project {
  id: string;
  name: string;
  status: ProjectStatus;
  ownerId: string;
  createdAt: Date;
}

export type ProjectStatus = 'draft' | 'active' | 'archived';

export interface CreateProjectInput {
  name: string;
}

export interface UpdateProjectInput {
  name?: string;
  status?: ProjectStatus;
}
```

---

## Type-Safe Function Calls

```typescript
// Generated from your Rust functions
import { createProject, updateProject, deleteProject } from '$lib/forge/api';

// Full type safety
const project = await mutate(createProject, { 
  name: 'My Project' 
});
// project: Project

await mutate(updateProject, {
  id: project.id,
  name: 'Renamed',
  // status: 'invalid'  // ← TypeScript error!
});
```

---

## Authentication

```svelte
<!-- src/routes/+layout.svelte -->
<script>
  import { ForgeProvider, useAuth } from '$lib/forge';
  
  const auth = useAuth();
  
  let { children } = $props();
</script>

<ForgeProvider 
  url="http://localhost:8080"
  getToken={() => auth.token}
  onAuthError={() => auth.logout()}
>
  {@render children()}
</ForgeProvider>
```

### Protected Routes

```svelte
<script>
  import { useAuth } from '$lib/forge';
  import { goto } from '$app/navigation';
  
  const auth = useAuth();
  
  $effect(() => {
    if (!$auth.user) {
      goto('/login');
    }
  });
</script>

{#if $auth.user}
  <slot />
{/if}
```

---

## Error Handling

```svelte
<script>
  import { mutate, ForgeError } from '$lib/forge';
  import { createProject } from '$lib/forge/api';
  
  async function handleCreate() {
    try {
      await mutate(createProject, { name });
    } catch (error) {
      if (error instanceof ForgeError) {
        switch (error.code) {
          case 'VALIDATION_ERROR':
            showValidationErrors(error.details);
            break;
          case 'UNAUTHORIZED':
            goto('/login');
            break;
          case 'DUPLICATE_NAME':
            toast.error('A project with this name already exists');
            break;
          default:
            toast.error('Something went wrong');
        }
      }
    }
  }
</script>
```

---

## Related Documentation

- [Stores](STORES.md) — Reactive store details
- [RPC Client](RPC_CLIENT.md) — Function calling
- [WebSocket](WEBSOCKET.md) — Real-time connection
- [Schema](../core/SCHEMA.md) — Type generation source

# RPC Client

> *Type-safe function calls from Svelte*

---

## Overview

FORGE generates a fully typed RPC client for calling backend functions:

```typescript
// All type-safe - errors caught at compile time
const user = await query(get_user, { userId: '123' });
const project = await mutate(create_project, { name: 'My App' });
const result = await action(sync_stripe, { userId: '123' });
```

---

## Generated Client

```typescript
// generated/api.ts

// Queries
export const get_user: Query<{ userId: string }, User>;
export const get_projects: Query<{ ownerId: string }, Project[]>;

// Mutations  
export const create_project: Mutation<CreateProjectInput, Project>;
export const update_project: Mutation<UpdateProjectInput, Project>;

// Actions
export const sync_stripe: Action<{ userId: string }, SyncResult>;
```

---

## Calling Functions

### Queries

```typescript
import { query } from '$lib/forge';
import { get_projects } from '$lib/forge/api';

// One-time query
const projects = await query(get_projects, { ownerId: userId });

// With options
const projects = await query(get_projects, { ownerId: userId }, {
  cache: 'no-cache',  // Skip cache
  timeout: 5000,      // 5 second timeout
});
```

### Mutations

```typescript
import { mutate } from '$lib/forge';
import { create_project } from '$lib/forge/api';

try {
  const project = await mutate(create_project, {
    name: 'My Project',
    description: 'A great project',
  });
  showToast(`Created ${project.name}`);
} catch (error) {
  if (error.code === 'VALIDATION_ERROR') {
    showErrors(error.fields);
  }
}
```

### Actions

```typescript
import { action } from '$lib/forge';
import { process_payment } from '$lib/forge/api';

const result = await action(process_payment, {
  orderId: '123',
  paymentMethod: 'card',
});
```

---

## Error Handling

```typescript
import { ForgeError } from '$lib/forge';

try {
  await mutate(create_project, input);
} catch (error) {
  if (error instanceof ForgeError) {
    switch (error.code) {
      case 'VALIDATION_ERROR':
        // error.fields contains field-specific errors
        break;
      case 'NOT_FOUND':
        // Resource not found
        break;
      case 'FORBIDDEN':
        // Not authorized
        break;
      case 'RATE_LIMITED':
        // Too many requests
        break;
    }
  }
}
```

---

## Type Safety

```typescript
// ✅ Correct - TypeScript happy
await mutate(create_project, { name: 'Valid' });

// ❌ Error - missing required field
await mutate(create_project, {});
//                            ^ Property 'name' is missing

// ❌ Error - wrong type
await mutate(create_project, { name: 123 });
//                                   ^ Type 'number' is not assignable to 'string'
```

---

## Related Documentation

- [Functions](../core/FUNCTIONS.md) — Backend function definitions
- [Stores](STORES.md) — Reactive data stores
- [WebSocket](WEBSOCKET.md) — Real-time connection

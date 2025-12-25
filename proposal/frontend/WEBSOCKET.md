# WebSocket Connection

> *Real-time connection handling*

---

## Overview

FORGE maintains a WebSocket connection for:

- **Subscriptions** — Real-time query updates
- **Presence** — Online status
- **Events** — Server-pushed notifications

---

## Connection Lifecycle

```
┌───────────────┐
│  CONNECTING   │ ◄── Initial connection
└───────┬───────┘
        │
        ▼
┌───────────────┐
│   CONNECTED   │ ◄── Subscriptions active
└───────┬───────┘
        │
   ┌────┴────┐
   ▼         ▼
Network    Close
error      requested
   │         │
   ▼         ▼
┌───────────────┐  ┌───────────────┐
│ RECONNECTING  │  │ DISCONNECTED  │
└───────┬───────┘  └───────────────┘
        │
        ▼
   Auto-retry with backoff
```

---

## Configuration

```typescript
// src/lib/forge/client.ts

import { createForgeClient } from '@forge/client';

export const forge = createForgeClient({
  httpUrl: 'https://api.example.com',
  wsUrl: 'wss://api.example.com/ws',
  
  reconnect: {
    enabled: true,
    maxAttempts: Infinity,
    delay: 1000,
    maxDelay: 30000,
    backoff: 'exponential',
  },
  
  auth: {
    getToken: () => localStorage.getItem('token'),
  },
});
```

---

## Connection Status

```svelte
<script>
  import { connectionStatus } from '$lib/forge';
</script>

{#if $connectionStatus === 'connected'}
  <span class="online">●</span>
{:else if $connectionStatus === 'reconnecting'}
  <span class="reconnecting">Reconnecting...</span>
{:else}
  <span class="offline">Offline</span>
{/if}
```

---

## Handling Disconnection

```typescript
import { onConnectionChange } from '$lib/forge';

onConnectionChange((status, error) => {
  if (status === 'disconnected') {
    showToast('Connection lost. Retrying...');
  } else if (status === 'connected') {
    showToast('Connected!');
  }
});
```

---

## Manual Control

```typescript
import { forge } from '$lib/forge/client';

// Disconnect
forge.disconnect();

// Reconnect
forge.connect();

// Check status
const isConnected = forge.isConnected();
```

---

## Related Documentation

- [Reactivity](../core/REACTIVITY.md) — Subscription system
- [Stores](STORES.md) — Reactive stores
- [RPC Client](RPC_CLIENT.md) — Function calls

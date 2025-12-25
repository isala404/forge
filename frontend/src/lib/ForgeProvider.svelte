<script lang="ts">
  import { onMount, onDestroy, type Snippet } from 'svelte';
  import { createForgeClient, type ForgeClientConfig } from './client.js';
  import { setForgeClient, setAuthState } from './context.js';
  import type { ForgeError, AuthState, ConnectionState } from './types.js';

  interface Props {
    url: string;
    getToken?: () => string | null | Promise<string | null>;
    onAuthError?: (error: ForgeError) => void;
    onConnectionChange?: (state: ConnectionState) => void;
    children: Snippet;
  }

  let {
    url,
    getToken,
    onAuthError,
    onConnectionChange,
    children
  }: Props = $props();

  // Create the client
  const client = createForgeClient({
    url,
    getToken,
    onAuthError,
  });

  // Set up context
  setForgeClient(client);

  // Initialize auth state
  let authState: AuthState = $state({
    user: null,
    token: null,
    loading: true,
  });

  setAuthState(authState);

  // Track connection state
  let connectionState: ConnectionState = $state('disconnected');

  // Connect on mount
  onMount(async () => {
    // Set up connection state listener
    const unsubscribe = client.onConnectionStateChange((state) => {
      connectionState = state;
      onConnectionChange?.(state);
    });

    // Connect to WebSocket
    try {
      await client.connect();
    } catch (e) {
      console.error('Failed to connect to FORGE server:', e);
    }

    // Get initial auth state
    if (getToken) {
      const token = await getToken();
      authState = {
        user: null, // Would be populated from a separate auth endpoint
        token,
        loading: false,
      };
    } else {
      authState = {
        user: null,
        token: null,
        loading: false,
      };
    }

    return () => {
      unsubscribe();
    };
  });

  // Disconnect on destroy
  onDestroy(() => {
    client.disconnect();
  });
</script>

{@render children()}

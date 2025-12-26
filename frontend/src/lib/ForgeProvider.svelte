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

  let props: Props = $props();

  // Create the client IMMEDIATELY so it's available for child components
  // This must happen during component initialization, not in onMount
  const client = createForgeClient({
    url: props.url,
    getToken: props.getToken,
    onAuthError: props.onAuthError,
  });

  // Set context immediately so children can access it
  setForgeClient(client);

  // Initialize auth state - use an object that we mutate rather than reassign
  // This ensures context consumers always have the reactive reference
  const authState: AuthState = $state({
    user: null,
    token: null,
    loading: true,
  });

  // Set auth state context immediately
  setAuthState(authState);

  // Track connection state
  let connectionState: ConnectionState = $state('disconnected');

  // Connect on mount
  onMount(async () => {
    // Set up connection state listener
    const unsubscribe = client.onConnectionStateChange((state) => {
      connectionState = state;
      props.onConnectionChange?.(state);
    });

    // Connect to WebSocket
    try {
      await client.connect();
    } catch (e) {
      console.error('Failed to connect to FORGE server:', e);
    }

    // Get initial auth state - mutate properties instead of reassigning
    if (props.getToken) {
      const token = await props.getToken();
      authState.token = token;
      authState.loading = false;
    } else {
      authState.loading = false;
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

{@render props.children()}

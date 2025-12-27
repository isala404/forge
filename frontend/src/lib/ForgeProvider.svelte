<script lang="ts">
  import { onMount, onDestroy, type Snippet } from 'svelte';
  import { createForgeClient, type ForgeClientConfig, isForgeDebugEnabled } from './client.js';
  import { setForgeClient, setAuthState } from './context.js';
  import type { ForgeError, AuthState, ConnectionState } from './types.js';

  interface Props {
    url: string;
    getToken?: () => string | null | Promise<string | null>;
    onAuthError?: (error: ForgeError) => void;
    onConnectionChange?: (state: ConnectionState) => void;
    /** Enable debug logging */
    debug?: boolean;
    children: Snippet;
  }

  let props: Props = $props();

  // Helper to conditionally log
  function debugLog(...args: unknown[]): void {
    if (isForgeDebugEnabled()) {
      console.log('[FORGE]', ...args);
    }
  }

  // Create the client IMMEDIATELY so it's available for child components
  // This must happen during component initialization, not in onMount
  const client = createForgeClient({
    url: props.url,
    getToken: props.getToken,
    onAuthError: props.onAuthError,
    debug: props.debug,
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
  onMount(() => {
    debugLog('ForgeProvider mounted, connecting to:', props.url);

    // Set up connection state listener
    const unsubscribe = client.onConnectionStateChange((state) => {
      debugLog('Connection state changed:', state);
      connectionState = state;
      props.onConnectionChange?.(state);
    });

    // Run async initialization
    (async () => {
      // Connect to WebSocket
      try {
        debugLog('Calling client.connect()...');
        await client.connect();
        debugLog('client.connect() resolved');
      } catch (e) {
        console.error('[FORGE] Failed to connect to FORGE server:', e);
      }

      // Get initial auth state - mutate properties instead of reassigning
      if (props.getToken) {
        const token = await props.getToken();
        authState.token = token;
        authState.loading = false;
      } else {
        authState.loading = false;
      }
    })();

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

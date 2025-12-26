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

  // Create the client lazily to avoid capturing initial values warning
  let client: ReturnType<typeof createForgeClient> | null = null;

  function getClient() {
    if (!client) {
      client = createForgeClient({
        url: props.url,
        getToken: props.getToken,
        onAuthError: props.onAuthError,
      });
      setForgeClient(client);
    }
    return client;
  }

  // Initialize auth state
  let authState: AuthState = $state({
    user: null,
    token: null,
    loading: true,
  });

  // Track connection state
  let connectionState: ConnectionState = $state('disconnected');

  // Connect on mount
  onMount(async () => {
    const forgeClient = getClient();

    // Set auth state in context
    setAuthState(authState);

    // Set up connection state listener
    const unsubscribe = forgeClient.onConnectionStateChange((state) => {
      connectionState = state;
      props.onConnectionChange?.(state);
    });

    // Connect to WebSocket
    try {
      await forgeClient.connect();
    } catch (e) {
      console.error('Failed to connect to FORGE server:', e);
    }

    // Get initial auth state
    if (props.getToken) {
      const token = await props.getToken();
      authState = {
        user: null,
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
    client?.disconnect();
  });
</script>

{@render props.children()}

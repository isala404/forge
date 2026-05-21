<script lang="ts">
  import { onMount, onDestroy, type Snippet } from 'svelte';
  import { createForgeClient } from './client.js';
  import { setForgeClient, setAuthState } from './context.js';
  import { ForgeSignals, type SignalsConfig } from './signals.js';
  import { setForgeSignals } from './signals-context.js';
  import type { AuthState, ConnectionState } from './types.js';

  interface Props {
    url: string;
    getToken?: () => string | null | Promise<string | null>;
    onAuthError?: (error: import('./types.js').ForgeError) => void;
    onMutationError?: (error: import('./client.js').ForgeClientError) => void;
    onConnectionChange?: (state: ConnectionState) => void;
    signals?: SignalsConfig | false;
    children: Snippet;
  }

  let { url, getToken, onAuthError, onMutationError, onConnectionChange, signals: signalsConfig, children }: Props = $props();

  // svelte-ignore state_referenced_locally -- url and getToken are stable config, not reactive state
  const client = createForgeClient({
    url,
    getToken,
    onAuthError,
    onMutationError,
  });

  setForgeClient(client);

  // svelte-ignore state_referenced_locally -- signalsConfig is stable mount-time config, same as url/getToken
  const signalsCfg = signalsConfig === false ? { enabled: false } : (signalsConfig ?? {});
  const signals = new ForgeSignals(client, signalsCfg);
  setForgeSignals(signals);

  client.setSignals(signals);

  const authState: AuthState = $state({ user: null, token: null, loading: true });
  setAuthState(authState);

  onMount(() => {
    const unsubscribe = client.onConnectionStateChange((state) => {
      onConnectionChange?.(state);
    });

    client.connect().then(async () => {
      if (getToken) {
        authState.token = await getToken();
      }
      authState.loading = false;
    }).catch(() => {
      authState.loading = false;
    });

    return unsubscribe;
  });

  onDestroy(() => {
    signals.destroy();
    client.disconnect();
  });
</script>

{@render children()}

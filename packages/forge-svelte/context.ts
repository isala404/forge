
import { getContext, setContext } from "svelte";
import type { ForgeClient } from "./client.js";
import type { AuthState } from "./types.js";

const FORGE_CLIENT_KEY = Symbol("forge-client");
const FORGE_AUTH_KEY = Symbol("forge-auth");

// Module-level fallback for callers outside Svelte's component tree (rare,
// but used by ad-hoc test harnesses). Multiple providers in the same process
// silently shared this slot — now we warn loudly so the test/embedded app
// catches it instead of debugging stale-client bugs.
let globalClient: ForgeClient | null = null;

export function getForgeClient(): ForgeClient {
  try {
    const client = getContext<ForgeClient>(FORGE_CLIENT_KEY);
    if (client) return client;
  } catch {}
  if (globalClient) return globalClient;
  throw new Error(
    "FORGE client not found. Wrap your component with ForgeProvider.",
  );
}

export function setForgeClient(client: ForgeClient): void {
  setContext(FORGE_CLIENT_KEY, client);
  if (globalClient && globalClient !== client && typeof console !== "undefined") {
    console.warn(
      "[forge] setForgeClient called with a second client. The module-level " +
        "fallback now points at the new instance; any code still using getForgeClient() " +
        "without a Svelte context will see the replacement. Mount one ForgeProvider per app.",
    );
  }
  globalClient = client;
}

export function getAuthState(): AuthState {
  const auth = getContext<AuthState>(FORGE_AUTH_KEY);
  if (!auth) throw new Error("Auth state not found.");
  return auth;
}

export function setAuthState(auth: AuthState): void {
  setContext(FORGE_AUTH_KEY, auth);
}

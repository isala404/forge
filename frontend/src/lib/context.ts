import { getContext, setContext } from 'svelte';
import type { ForgeClient } from './client.js';
import type { AuthState } from './types.js';

/**
 * Context key for the FORGE client.
 */
const FORGE_CLIENT_KEY = Symbol('forge-client');

/**
 * Context key for auth state.
 */
const FORGE_AUTH_KEY = Symbol('forge-auth');

/**
 * Module-level client reference for use outside component initialization.
 * This is set by ForgeProvider and used by mutate/action functions.
 */
let globalClient: ForgeClient | null = null;

/**
 * Get the FORGE client from context (during component initialization)
 * or from the global reference (in event handlers).
 */
export function getForgeClient(): ForgeClient {
  // Try context first (works during component initialization)
  try {
    const client = getContext<ForgeClient>(FORGE_CLIENT_KEY);
    if (client) return client;
  } catch {
    // getContext throws outside component initialization, fall through
  }

  // Fall back to global client (works in event handlers)
  if (globalClient) {
    return globalClient;
  }

  throw new Error(
    'FORGE client not found. ' +
    'Make sure your component is wrapped with ForgeProvider.'
  );
}

/**
 * Set the FORGE client in context.
 * Used internally by ForgeProvider.
 */
export function setForgeClient(client: ForgeClient): void {
  setContext(FORGE_CLIENT_KEY, client);
  // Also set global reference for use in event handlers
  globalClient = client;
}

/**
 * Get the auth state from context.
 * Must be called within a component that is a descendant of ForgeProvider.
 */
export function getAuthState(): AuthState {
  const auth = getContext<AuthState>(FORGE_AUTH_KEY);
  if (!auth) {
    throw new Error(
      'Auth state not found. ' +
      'Make sure your component is wrapped with ForgeProvider.'
    );
  }
  return auth;
}

/**
 * Set the auth state in context.
 * Used internally by ForgeProvider.
 */
export function setAuthState(auth: AuthState): void {
  setContext(FORGE_AUTH_KEY, auth);
}

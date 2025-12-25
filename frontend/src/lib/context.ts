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
 * Get the FORGE client from context.
 * Must be called within a component that is a descendant of ForgeProvider.
 */
export function getForgeClient(): ForgeClient {
  const client = getContext<ForgeClient>(FORGE_CLIENT_KEY);
  if (!client) {
    throw new Error(
      'FORGE client not found. ' +
      'Make sure your component is wrapped with ForgeProvider.'
    );
  }
  return client;
}

/**
 * Set the FORGE client in context.
 * Used internally by ForgeProvider.
 */
export function setForgeClient(client: ForgeClient): void {
  setContext(FORGE_CLIENT_KEY, client);
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

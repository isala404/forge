import { getAuthState } from './context.js';
import type { AuthState } from './types.js';
import type { Readable } from './stores.js';

/**
 * Auth store with methods for login/logout.
 */
export interface AuthStore extends Readable<AuthState> {
  login: (token: string, user?: unknown) => void;
  logout: () => void;
  setUser: (user: unknown) => void;
}

/**
 * Create an auth store for managing authentication state.
 * This provides a reactive store that can be used to track and modify auth state.
 *
 * @example
 * ```svelte
 * <script>
 *   import { createAuthStore } from '@forge/svelte';
 *
 *   const auth = createAuthStore();
 *
 *   async function handleLogin(email: string, password: string) {
 *     const response = await fetch('/api/login', {
 *       method: 'POST',
 *       body: JSON.stringify({ email, password })
 *     });
 *     const { token, user } = await response.json();
 *     auth.login(token, user);
 *   }
 * </script>
 *
 * {#if $auth.user}
 *   <UserMenu user={$auth.user} onLogout={() => auth.logout()} />
 * {:else}
 *   <LoginButton onclick={handleLogin} />
 * {/if}
 * ```
 */
export function createAuthStore(
  initialToken?: string | null,
  initialUser?: unknown
): AuthStore {
  const subscribers = new Set<(value: AuthState) => void>();

  let state: AuthState = {
    user: initialUser ?? null,
    token: initialToken ?? null,
    loading: false,
  };

  const notify = () => {
    subscribers.forEach(run => run(state));
  };

  return {
    subscribe(run) {
      subscribers.add(run);
      run(state);
      return () => subscribers.delete(run);
    },

    login(token: string, user?: unknown) {
      state = {
        user: user ?? null,
        token,
        loading: false,
      };
      // Store in localStorage for persistence
      if (typeof localStorage !== 'undefined') {
        localStorage.setItem('forge_token', token);
        if (user) {
          localStorage.setItem('forge_user', JSON.stringify(user));
        }
      }
      notify();
    },

    logout() {
      state = {
        user: null,
        token: null,
        loading: false,
      };
      // Clear from localStorage
      if (typeof localStorage !== 'undefined') {
        localStorage.removeItem('forge_token');
        localStorage.removeItem('forge_user');
      }
      notify();
    },

    setUser(user: unknown) {
      state = { ...state, user };
      if (typeof localStorage !== 'undefined' && user) {
        localStorage.setItem('forge_user', JSON.stringify(user));
      }
      notify();
    },
  };
}

/**
 * Get the auth state from context.
 * This is a convenience function that returns the current auth state.
 * For reactive updates, use the auth store from createAuthStore().
 */
export function useAuth(): AuthState {
  return getAuthState();
}

/**
 * Create an auth store that persists to localStorage.
 * Automatically restores the auth state on page load.
 */
export function createPersistentAuthStore(): AuthStore {
  let initialToken: string | null = null;
  let initialUser: unknown = null;

  if (typeof localStorage !== 'undefined') {
    initialToken = localStorage.getItem('forge_token');
    const userJson = localStorage.getItem('forge_user');
    if (userJson) {
      try {
        initialUser = JSON.parse(userJson);
      } catch {
        // Invalid JSON, ignore
      }
    }
  }

  return createAuthStore(initialToken, initialUser);
}

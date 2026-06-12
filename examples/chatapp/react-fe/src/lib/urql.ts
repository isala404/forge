import { Client, fetchExchange, subscriptionExchange } from 'urql'
import { authExchange } from '@urql/exchange-auth'
import { cacheExchange } from '@urql/exchange-graphcache'
import { createClient as createWsClient } from 'graphql-ws'
import { GRAPHQL_HTTP, GRAPHQL_WS } from './config'
import { getToken, onTokenChange, setToken } from './token'
import { cacheConfig } from './cache'

// Forge pubsub is at-most-once and connected-only: anything published while the
// socket was down is gone, with no replay. The durable record lives in Postgres,
// so on every (re)connect we tell the live views to refetch from the network and
// reconcile missed events against the normalized cache (entities dedup by id).
const reconnectListeners = new Set<() => void>()

export function onWsReconnect(fn: () => void): () => void {
  reconnectListeners.add(fn)
  return () => reconnectListeners.delete(fn)
}

// One graphql-ws socket. connectionParams are read lazily so a fresh login/logout
// is picked up on the next (re)connect; a token change forces a reconnect.
const wsClient = createWsClient({
  url: GRAPHQL_WS,
  lazy: true,
  connectionParams: () => {
    const token = getToken()
    return token ? { authorization: `Bearer ${token}` } : {}
  },
  on: {
    connected: () => {
      for (const fn of reconnectListeners) fn()
    },
    // Connection-level auth failure. In-band UNAUTHENTICATED errors on individual
    // operations are handled by the HTTP authExchange; this only covers the socket
    // itself being rejected. We key off the close code so a transient network drop
    // (which graphql-ws will retry and reconnect on its own) does not log the user
    // out — only an auth-specific close clears the session, via the same setToken
    // path as the HTTP side, dropping the app back to login.
    closed: (event) => {
      const code = (event as CloseEvent)?.code
      if (getToken() && (code === 4401 || code === 4403)) {
        setToken(null)
      }
    },
  },
})

onTokenChange(() => {
  // Drop the socket so the next subscription reconnects with the new principal.
  void wsClient.dispose()
})

export const client = new Client({
  url: GRAPHQL_HTTP,
  exchanges: [
    cacheExchange(cacheConfig),
    authExchange(async (utils) => ({
      addAuthToOperation(operation) {
        const token = getToken()
        if (!token) return operation
        return utils.appendHeaders(operation, { Authorization: `Bearer ${token}` })
      },
      didAuthError(error) {
        return error.graphQLErrors.some(
          (e) => e.extensions?.code === 'UNAUTHENTICATED',
        )
      },
      async refreshAuth() {
        // No refresh token in this model. An auth error means the session is gone;
        // clear it so the app drops back to the login screen.
        setToken(null)
      },
      willAuthError() {
        return false
      },
    })),
    fetchExchange,
    subscriptionExchange({
      forwardSubscription(request) {
        const input = { ...request, query: request.query ?? '' }
        return {
          subscribe(sink) {
            const unsubscribe = wsClient.subscribe(input, sink)
            return { unsubscribe }
          },
        }
      },
    }),
  ],
})

// A failed presigned PUT, carrying the HTTP status so the caller can tell an
// expired/invalid signature (403, re-presignable) from other failures.
export class UploadError extends Error {
  readonly status: number
  constructor(status: number) {
    super(`upload failed (${status})`)
    this.status = status
    this.name = 'UploadError'
  }
}

// Plain fetch wrapper for the presigned PUT upload, which is not a GraphQL call.
export async function putToUrl(url: string, file: File): Promise<void> {
  const res = await fetch(url, {
    method: 'PUT',
    headers: { 'Content-Type': file.type || 'application/octet-stream' },
    body: file,
  })
  if (!res.ok) {
    throw new UploadError(res.status)
  }
}

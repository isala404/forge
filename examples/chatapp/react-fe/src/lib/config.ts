// Backend endpoints. Default to node-be on 8082; any of the three backends works
// because they all serve the one canonical schema.
//
// Resolution order:
//   1. window.__ENV__  -> injected at container start by docker-entrypoint.sh,
//      so one prebuilt nginx image can target any backend without rebuilding.
//   2. import.meta.env -> Vite build-time vars (used by `vite dev`).
//   3. localhost:8082  -> node-be default.
const httpDefault = 'http://localhost:8082/graphql'
const wsDefault = 'ws://localhost:8082/graphql'

declare global {
  interface Window {
    __ENV__?: { VITE_GRAPHQL_HTTP?: string; VITE_GRAPHQL_WS?: string }
  }
}

const runtime = typeof window !== 'undefined' ? window.__ENV__ : undefined

export const GRAPHQL_HTTP =
  runtime?.VITE_GRAPHQL_HTTP || import.meta.env.VITE_GRAPHQL_HTTP || httpDefault
export const GRAPHQL_WS =
  runtime?.VITE_GRAPHQL_WS || import.meta.env.VITE_GRAPHQL_WS || wsDefault

// The backend presence kv expires after PRESENCE_TTL_MS. Beating at <= TTL/2 means
// a single dropped beat still leaves a second one inside the window before the user
// flips to offline. Jitter spreads many tabs' beats so they don't synchronize into
// a thundering herd against the backend.
export const PRESENCE_TTL_MS = 30_000
export const PRESENCE_HEARTBEAT_MS = PRESENCE_TTL_MS / 2
export const PRESENCE_HEARTBEAT_JITTER_MS = 3_000

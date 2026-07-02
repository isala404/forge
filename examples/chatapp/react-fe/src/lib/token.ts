// Session token: in-memory copy is authoritative for the running tab, localStorage
// is the durable fallback that survives reloads. Subscribers (the WS client, the
// auth exchange) read through getToken so both stores stay in sync.
const STORAGE_KEY = 'chatapp.token'

let current: string | null = localStorage.getItem(STORAGE_KEY)
const listeners = new Set<(token: string | null) => void>()

export function getToken(): string | null {
  return current
}

export function setToken(token: string | null): void {
  current = token
  if (token) localStorage.setItem(STORAGE_KEY, token)
  else localStorage.removeItem(STORAGE_KEY)
  for (const fn of listeners) fn(token)
}

export function onTokenChange(fn: (token: string | null) => void): () => void {
  listeners.add(fn)
  return () => listeners.delete(fn)
}

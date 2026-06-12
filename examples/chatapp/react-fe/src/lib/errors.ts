import type { CombinedError } from 'urql'

const FRIENDLY: Record<string, string> = {
  UNAUTHENTICATED: 'Your session expired. Please sign in again.',
  INVALID: 'That input was not accepted.',
  LIMIT: 'You are doing that too fast. Wait a moment and retry.',
  NOT_FOUND: 'That was not found.',
  PRECONDITION: 'That action is not allowed right now.',
  UNAVAILABLE: 'The service is temporarily unavailable.',
  CONFIG: 'The server is misconfigured.',
  BACKEND: 'The server hit an unexpected error.',
}

export function errorCode(error: CombinedError | undefined): string | null {
  const ext = error?.graphQLErrors?.[0]?.extensions
  return (ext?.code as string | undefined) ?? null
}

export function errorMessage(error: CombinedError | undefined): string {
  if (!error) return ''
  if (error.networkError) {
    return 'Cannot reach the server. Check that a backend is running.'
  }
  const gql = error.graphQLErrors?.[0]
  if (gql) {
    const code = gql.extensions?.code as string | undefined
    if (code && FRIENDLY[code]) return FRIENDLY[code]
    return gql.message
  }
  return error.message.replace(/^\[\w+\]\s*/, '')
}

import { useCallback, useSyncExternalStore } from 'react'
import { useMutation, useQuery } from 'urql'
import {
  LoginMutation,
  LogoutAllMutation,
  LogoutMutation,
  MeQuery,
  SignupMutation,
} from '../graphql/operations'
import { getToken, onTokenChange, setToken } from '../lib/token'
// `useFragment` from the client preset is a pure type-cast, not a React hook;
// aliasing it avoids the rules-of-hooks lint on conditional calls.
import { useFragment as readFragment, type FragmentType } from '../gql'
import { UserFields } from '../graphql/operations'

export type SessionUser = FragmentType<typeof UserFields>

export function useToken(): string | null {
  return useSyncExternalStore(onTokenChange, getToken, getToken)
}

export function useSession() {
  const token = useToken()
  const [{ data, fetching }, refetchMe] = useQuery({
    query: MeQuery,
    pause: !token,
  })

  const me = data?.me ?? null
  return {
    token,
    me,
    user: me ? readFragment(UserFields, me) : null,
    loadingMe: Boolean(token) && fetching,
    refetchMe,
  }
}

export function useAuthActions() {
  const [signupState, signupMut] = useMutation(SignupMutation)
  const [loginState, loginMut] = useMutation(LoginMutation)
  const [, logoutMut] = useMutation(LogoutMutation)
  const [, logoutAllMut] = useMutation(LogoutAllMutation)

  const signup = useCallback(
    async (username: string, displayName: string, password: string) => {
      const res = await signupMut({ username, displayName, password })
      if (res.data?.signup.token) setToken(res.data.signup.token)
      return res
    },
    [signupMut],
  )

  const login = useCallback(
    async (username: string, password: string) => {
      const res = await loginMut({ username, password })
      if (res.data?.login.token) setToken(res.data.login.token)
      return res
    },
    [loginMut],
  )

  const logout = useCallback(async () => {
    await logoutMut({})
    setToken(null)
  }, [logoutMut])

  const logoutAll = useCallback(async () => {
    await logoutAllMut({})
    setToken(null)
  }, [logoutAllMut])

  return {
    signup,
    login,
    logout,
    logoutAll,
    submitting: signupState.fetching || loginState.fetching,
  }
}

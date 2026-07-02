import { useState, type FormEvent } from 'react'
import { ChatCircleDots } from '@phosphor-icons/react'
import { Button } from '../ui/Button'
import { useAuthActions } from '../../hooks/useSession'
import { errorMessage } from '../../lib/errors'

type Mode = 'login' | 'signup'

export function AuthScreen() {
  const { login, signup, submitting } = useAuthActions()
  const [mode, setMode] = useState<Mode>('login')
  const [username, setUsername] = useState('')
  const [displayName, setDisplayName] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)

  const usernameOk = username.trim().length >= 3
  const passwordOk = password.length >= 6
  const displayOk = mode === 'login' || displayName.trim().length >= 1
  const canSubmit = usernameOk && passwordOk && displayOk

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    setError(null)
    const res =
      mode === 'login'
        ? await login(username.trim(), password)
        : await signup(username.trim(), displayName.trim(), password)
    if (res.error) setError(errorMessage(res.error))
  }

  return (
    <div className="auth-shell">
      <div className="auth-aside" aria-hidden>
        <div className="auth-aside-inner">
          <ChatCircleDots size={56} weight="duotone" />
          <h1>Forge Chat</h1>
          <p>
            One React client, three interchangeable GraphQL backends. Live messages,
            presence, receipts, and attachments over a single typed schema.
          </p>
        </div>
      </div>

      <div className="auth-main">
        <form className="auth-card" onSubmit={onSubmit} noValidate>
          <div className="auth-brand">
            <ChatCircleDots size={28} weight="fill" />
            <span>Forge Chat</span>
          </div>

          <div className="auth-tabs" role="tablist" aria-label="Authentication mode">
            <button
              type="button"
              role="tab"
              aria-selected={mode === 'login'}
              className="auth-tab"
              data-active={mode === 'login' || undefined}
              onClick={() => {
                setMode('login')
                setError(null)
              }}
            >
              Sign in
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={mode === 'signup'}
              className="auth-tab"
              data-active={mode === 'signup' || undefined}
              onClick={() => {
                setMode('signup')
                setError(null)
              }}
            >
              Create account
            </button>
          </div>

          <label className="field">
            <span className="field-label">Username</span>
            <input
              className="input"
              autoComplete="username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="ada"
              aria-invalid={username.length > 0 && !usernameOk}
            />
            {username.length > 0 && !usernameOk && (
              <span className="field-error">At least 3 characters.</span>
            )}
          </label>

          {mode === 'signup' && (
            <label className="field">
              <span className="field-label">Display name</span>
              <input
                className="input"
                autoComplete="name"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                placeholder="Ada Lovelace"
              />
            </label>
          )}

          <label className="field">
            <span className="field-label">Password</span>
            <input
              className="input"
              type="password"
              autoComplete={mode === 'login' ? 'current-password' : 'new-password'}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="At least 6 characters"
              aria-invalid={password.length > 0 && !passwordOk}
            />
            {mode === 'signup' && password.length > 0 && !passwordOk && (
              <span className="field-error">At least 6 characters.</span>
            )}
          </label>

          {error && (
            <p className="form-banner" role="alert">
              {error}
            </p>
          )}

          <Button type="submit" loading={submitting} disabled={!canSubmit}>
            {mode === 'login' ? 'Sign in' : 'Create account'}
          </Button>

          <p className="auth-switch">
            {mode === 'login' ? (
              <>
                New here?{' '}
                <button type="button" onClick={() => setMode('signup')}>
                  Create an account
                </button>
              </>
            ) : (
              <>
                Already registered?{' '}
                <button type="button" onClick={() => setMode('login')}>
                  Sign in
                </button>
              </>
            )}
          </p>
        </form>
      </div>
    </div>
  )
}

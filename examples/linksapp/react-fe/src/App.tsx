import {
  Copy,
  Link2,
  LogOut,
  Plus,
  QrCode,
  RefreshCw,
  Server,
  Trash2,
} from 'lucide-react'
import { FormEvent, useEffect, useMemo, useState } from 'react'

import './app.css'

type BackendKey = 'rust' | 'node' | 'python'

const API_TARGETS: Record<BackendKey, string> = {
  rust: 'http://127.0.0.1:9091',
  node: 'http://127.0.0.1:9092',
  python: 'http://127.0.0.1:9093',
}

interface PublicUser {
  id: string
  email: string
}

interface AuthResponse {
  token: string
  user: PublicUser
}

interface Link {
  slug: string
  url: string
  createdAt: string
  expiresAt: string | null
  clicks: number
}

interface ReportLine {
  primitive: string
  provider: string
  durable: boolean
  caveats: string
}

interface Meta {
  backend: string
  forge: ReportLine[]
  features: { customSlugs: boolean }
  clicksQueueDepth: { visible: number; inFlight: number; delayed: number }
}

type AuthMode = 'signup' | 'login'

const TTL_OPTIONS: { label: string; value: number | null }[] = [
  { label: 'Never', value: null },
  { label: '10 minutes', value: 600 },
  { label: '1 hour', value: 3600 },
  { label: '1 day', value: 86400 },
]

function initialApiBase(): string {
  const params = new URLSearchParams(window.location.search)
  const fromQuery = params.get('api')
  if (fromQuery) {
    localStorage.setItem('links-api-base', fromQuery)
    return fromQuery
  }
  return localStorage.getItem('links-api-base') || API_TARGETS.rust
}

function tokenKey(apiBase: string): string {
  return `links-token:${apiBase}`
}

function targetFor(apiBase: string): BackendKey | null {
  return (Object.entries(API_TARGETS).find(([, value]) => value === apiBase)?.[0] ||
    null) as BackendKey | null
}

async function request<T>(
  apiBase: string,
  path: string,
  token: string | null,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Accept', 'application/json')
  if (init.body && !headers.has('Content-Type')) headers.set('Content-Type', 'application/json')
  if (token) headers.set('Authorization', `Bearer ${token}`)
  const res = await fetch(`${apiBase}${path}`, { ...init, headers })
  const text = await res.text()
  const parsed = text ? JSON.parse(text) : null
  if (!res.ok) {
    throw new Error(parsed?.error || `request failed with ${res.status}`)
  }
  return parsed as T
}

export default function App() {
  const [apiBase, setApiBase] = useState(initialApiBase)
  const [token, setToken] = useState<string | null>(() => localStorage.getItem(tokenKey(initialApiBase())))
  const [user, setUser] = useState<PublicUser | null>(null)
  const [links, setLinks] = useState<Link[]>([])
  const [meta, setMeta] = useState<Meta | null>(null)
  const [mode, setMode] = useState<AuthMode>('signup')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [newUrl, setNewUrl] = useState('')
  const [newSlug, setNewSlug] = useState('')
  const [ttlSeconds, setTtlSeconds] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const activeTarget = useMemo(() => targetFor(apiBase), [apiBase])

  async function loadMeta(base = apiBase) {
    const next = await request<Meta>(base, '/api/meta', null)
    setMeta(next)
  }

  async function loadSession(currentToken = token, base = apiBase) {
    if (!currentToken) {
      setUser(null)
      setLinks([])
      return
    }
    const me = await request<{ user: PublicUser }>(base, '/api/me', currentToken)
    const list = await request<{ links: Link[] }>(base, '/api/links', currentToken)
    setUser(me.user)
    setLinks(list.links)
  }

  useEffect(() => {
    setError(null)
    setToken(localStorage.getItem(tokenKey(apiBase)))
    localStorage.setItem('links-api-base', apiBase)
    const params = new URLSearchParams(window.location.search)
    params.set('api', apiBase)
    window.history.replaceState(null, '', `${window.location.pathname}?${params}`)
    loadMeta(apiBase).catch((err: Error) => setError(err.message))
  }, [apiBase])

  useEffect(() => {
    loadSession(token).catch((err: Error) => {
      localStorage.removeItem(tokenKey(apiBase))
      setToken(null)
      setUser(null)
      setLinks([])
      setError(err.message)
    })
  }, [token, apiBase])

  async function authenticate(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      const auth = await request<AuthResponse>(apiBase, `/api/${mode}`, null, {
        method: 'POST',
        body: JSON.stringify({ email, password }),
      })
      localStorage.setItem(tokenKey(apiBase), auth.token)
      setToken(auth.token)
      setUser(auth.user)
      setPassword('')
      await loadSession(auth.token)
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setBusy(false)
    }
  }

  async function logout() {
    if (token) {
      await request(apiBase, '/api/logout', token, { method: 'POST' }).catch(() => null)
    }
    localStorage.removeItem(tokenKey(apiBase))
    setToken(null)
    setUser(null)
    setLinks([])
  }

  async function createLink(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const url = newUrl.trim()
    if (!url) return
    setBusy(true)
    setError(null)
    try {
      const body: Record<string, unknown> = { url }
      if (meta?.features.customSlugs && newSlug.trim()) body.slug = newSlug.trim()
      if (ttlSeconds !== null) body.ttlSeconds = ttlSeconds
      const link = await request<Link>(apiBase, '/api/links', token, {
        method: 'POST',
        body: JSON.stringify(body),
      })
      setLinks((current) => [link, ...current])
      setNewUrl('')
      setNewSlug('')
      setTtlSeconds(null)
      await loadMeta()
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setBusy(false)
    }
  }

  async function deleteLink(slug: string) {
    setError(null)
    try {
      await request(apiBase, `/api/links/${slug}`, token, { method: 'DELETE' })
      setLinks((current) => current.filter((link) => link.slug !== slug))
      await loadMeta()
    } catch (err) {
      setError((err as Error).message)
    }
  }

  return (
    <main className="app-shell">
      <section className="top-band">
        <div>
          <div className="brand-row">
            <Link2 size={30} aria-hidden="true" />
            <h1>Forge Links</h1>
          </div>
          <p>{user ? user.email : 'REST parity across Rust, Node, and Python'}</p>
        </div>

        <div className="backend-panel" aria-label="Backend target">
          <div className="panel-label">
            <Server size={16} aria-hidden="true" />
            <span>{meta?.backend || activeTarget || 'backend'}</span>
          </div>
          <div className="target-switch">
            {(Object.keys(API_TARGETS) as BackendKey[]).map((key) => (
              <button
                key={key}
                type="button"
                className={API_TARGETS[key] === apiBase ? 'active' : ''}
                onClick={() => setApiBase(API_TARGETS[key])}
              >
                {key}
              </button>
            ))}
          </div>
        </div>
      </section>

      {error && <div className="error-banner">{error}</div>}

      {!user ? (
        <section className="auth-panel">
          <div className="mode-tabs" role="tablist" aria-label="Auth mode">
            <button
              type="button"
              className={mode === 'signup' ? 'active' : ''}
              onClick={() => setMode('signup')}
            >
              Sign up
            </button>
            <button
              type="button"
              className={mode === 'login' ? 'active' : ''}
              onClick={() => setMode('login')}
            >
              Log in
            </button>
          </div>
          <form onSubmit={authenticate} className="auth-form">
            <label>
              Email
              <input
                type="email"
                placeholder="ada@example.com"
                value={email}
                onChange={(event) => setEmail(event.target.value)}
                autoComplete="email"
                required
              />
            </label>
            <label>
              Password
              <input
                type="password"
                placeholder="At least 8 characters"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete={mode === 'signup' ? 'new-password' : 'current-password'}
                minLength={8}
                required
              />
            </label>
            <button className="primary-btn" disabled={busy}>
              {mode === 'signup' ? 'Create account' : 'Log in'}
            </button>
          </form>
        </section>
      ) : (
        <section className="links-layout">
          <div className="links-toolbar">
            <div>
              <h2>Your links</h2>
              <span>{links.length} link{links.length !== 1 ? 's' : ''}</span>
            </div>
            <div className="toolbar-actions">
              <button type="button" className="icon-btn" onClick={() => loadSession()} title="Refresh">
                <RefreshCw size={18} aria-hidden="true" />
              </button>
              <button type="button" className="icon-btn" onClick={logout} title="Log out">
                <LogOut size={18} aria-hidden="true" />
              </button>
            </div>
          </div>

          <form className="new-link" onSubmit={createLink}>
            <input
              type="url"
              value={newUrl}
              onChange={(event) => setNewUrl(event.target.value)}
              placeholder="https://example.com/long-url"
              required
              data-testid="url-input"
            />
            {meta?.features.customSlugs && (
              <input
                value={newSlug}
                onChange={(event) => setNewSlug(event.target.value)}
                placeholder="custom-slug (optional)"
                maxLength={32}
                data-testid="slug-input"
              />
            )}
            <select
              value={ttlSeconds === null ? '' : String(ttlSeconds)}
              onChange={(event) => {
                const val = event.target.value
                setTtlSeconds(val === '' ? null : Number(val))
              }}
              aria-label="Expiry"
              data-testid="ttl-select"
            >
              {TTL_OPTIONS.map((opt) => (
                <option key={opt.label} value={opt.value === null ? '' : String(opt.value)}>
                  {opt.label}
                </option>
              ))}
            </select>
            <button className="add-btn" disabled={busy || !newUrl.trim()}>
              <Plus size={18} aria-hidden="true" />
              Shorten
            </button>
          </form>

          <div className="links-list" aria-label="Links list" data-testid="links-list">
            {links.length === 0 ? (
              <div className="empty-state">No links yet. Shorten one above.</div>
            ) : (
              links.map((link) => (
                <LinkRow
                  key={link.slug}
                  link={link}
                  apiBase={apiBase}
                  onDelete={() => deleteLink(link.slug)}
                />
              ))
            )}
          </div>
        </section>
      )}

      <section className="ops-strip">
        <span>Clicks queue: {meta?.clicksQueueDepth.visible ?? 0} visible</span>
        <span>{meta?.forge.length ?? 0} Forge primitives reporting</span>
      </section>
    </main>
  )
}

function LinkRow({
  link,
  apiBase,
  onDelete,
}: {
  link: Link
  apiBase: string
  onDelete: () => void
}) {
  const [clicks, setClicks] = useState(link.clicks)
  const shortUrl = `${apiBase}/${link.slug}`

  useEffect(() => {
    setClicks(link.clicks)
  }, [link.clicks])

  useEffect(() => {
    const es = new EventSource(`${apiBase}/api/links/${link.slug}/live`)
    es.onmessage = (event) => {
      try {
        const payload = JSON.parse(event.data) as { slug: string; clicks: number }
        if (payload.slug === link.slug) setClicks(payload.clicks)
      } catch {
        // malformed SSE frame; ignore
      }
    }
    return () => es.close()
  }, [apiBase, link.slug])

  function copyShortUrl() {
    navigator.clipboard.writeText(shortUrl).catch(() => null)
  }

  const expiryHint = link.expiresAt
    ? `Expires ${new Date(link.expiresAt).toLocaleString()}`
    : null

  return (
    <article className="link-row" data-testid="link-row">
      <div className="link-row-main">
        <div className="link-row-short">
          <a
            href={shortUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="short-url"
            data-testid="short-url"
          >
            {shortUrl}
          </a>
          <button type="button" className="icon-btn copy-btn" onClick={copyShortUrl} title="Copy short URL">
            <Copy size={15} aria-hidden="true" />
          </button>
        </div>
        <div className="link-row-dest" title={link.url}>
          {link.url.length > 80 ? `${link.url.slice(0, 77)}…` : link.url}
        </div>
        <div className="link-row-meta">
          <span className="click-count" data-testid="click-count">{clicks} click{clicks !== 1 ? 's' : ''}</span>
          {expiryHint && <span className="expiry-hint">{expiryHint}</span>}
        </div>
      </div>

      <div className="link-row-aside">
        <img
          className="qr-thumb"
          src={`${apiBase}/api/links/${link.slug}/qr.svg`}
          alt={`QR code for ${shortUrl}`}
          width={64}
          height={64}
          data-testid="qr-img"
        />
        <div className="link-row-actions">
          <button
            type="button"
            className="icon-btn"
            title="QR code"
            onClick={() => window.open(`${apiBase}/api/links/${link.slug}/qr.svg`, '_blank')}
          >
            <QrCode size={15} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="delete-btn"
            onClick={onDelete}
            aria-label="Delete link"
            data-testid="delete-link"
          >
            <Trash2 size={15} />
          </button>
        </div>
      </div>
    </article>
  )
}

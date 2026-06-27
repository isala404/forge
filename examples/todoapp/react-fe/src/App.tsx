import {
  CheckCircle2,
  Circle,
  ListTodo,
  LogOut,
  Plus,
  RefreshCw,
  Server,
  Trash2,
} from 'lucide-react'
import { FormEvent, useEffect, useMemo, useState } from 'react'

import './app.css'

type BackendKey = 'rust' | 'node' | 'python'

const API_TARGETS: Record<BackendKey, string> = {
  rust: 'http://127.0.0.1:9081',
  node: 'http://127.0.0.1:9082',
  python: 'http://127.0.0.1:9083',
}

interface PublicUser {
  id: string
  email: string
}

interface AuthResponse {
  token: string
  user: PublicUser
}

interface Todo {
  id: string
  title: string
  completed: boolean
  createdAt: string
  updatedAt: string
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
  auditDepth: {
    visible: number
    inFlight: number
    delayed: number
  }
}

type AuthMode = 'signup' | 'login'

function initialApiBase(): string {
  const params = new URLSearchParams(window.location.search)
  const fromQuery = params.get('api')
  if (fromQuery) {
    localStorage.setItem('todo-api-base', fromQuery)
    return fromQuery
  }
  return localStorage.getItem('todo-api-base') || API_TARGETS.rust
}

function tokenKey(apiBase: string): string {
  return `todo-token:${apiBase}`
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
  const [todos, setTodos] = useState<Todo[]>([])
  const [meta, setMeta] = useState<Meta | null>(null)
  const [mode, setMode] = useState<AuthMode>('signup')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [newTitle, setNewTitle] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const activeTarget = useMemo(() => targetFor(apiBase), [apiBase])
  const completed = todos.filter((todo) => todo.completed).length

  async function loadMeta(base = apiBase) {
    const next = await request<Meta>(base, '/api/meta', null)
    setMeta(next)
  }

  async function loadSession(currentToken = token, base = apiBase) {
    if (!currentToken) {
      setUser(null)
      setTodos([])
      return
    }
    const me = await request<{ user: PublicUser }>(base, '/api/me', currentToken)
    const list = await request<{ todos: Todo[] }>(base, '/api/todos', currentToken)
    setUser(me.user)
    setTodos(list.todos)
  }

  useEffect(() => {
    setError(null)
    setToken(localStorage.getItem(tokenKey(apiBase)))
    localStorage.setItem('todo-api-base', apiBase)
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
      setTodos([])
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
    setTodos([])
  }

  async function addTodo(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const title = newTitle.trim()
    if (!title) return
    setBusy(true)
    setError(null)
    try {
      const todo = await request<Todo>(apiBase, '/api/todos', token, {
        method: 'POST',
        body: JSON.stringify({ title }),
      })
      setTodos((current) => [todo, ...current])
      setNewTitle('')
      await loadMeta()
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setBusy(false)
    }
  }

  async function updateTodo(id: string, patch: Partial<Pick<Todo, 'title' | 'completed'>>) {
    setError(null)
    try {
      const todo = await request<Todo>(apiBase, `/api/todos/${id}`, token, {
        method: 'PATCH',
        body: JSON.stringify(patch),
      })
      setTodos((current) => current.map((item) => (item.id === id ? todo : item)))
      await loadMeta()
    } catch (err) {
      setError((err as Error).message)
    }
  }

  async function deleteTodo(id: string) {
    setError(null)
    try {
      await request(apiBase, `/api/todos/${id}`, token, { method: 'DELETE' })
      setTodos((current) => current.filter((item) => item.id !== id))
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
            <ListTodo size={30} aria-hidden="true" />
            <h1>Forge Todos</h1>
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
        <section className="todo-layout">
          <div className="todo-toolbar">
            <div>
              <h2>Tasks</h2>
              <span>
                {completed} of {todos.length} complete
              </span>
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

          <form className="new-todo" onSubmit={addTodo}>
            <input
              value={newTitle}
              onChange={(event) => setNewTitle(event.target.value)}
              placeholder="New todo"
              maxLength={160}
            />
            <button className="add-btn" disabled={busy || !newTitle.trim()}>
              <Plus size={18} aria-hidden="true" />
              Add
            </button>
          </form>

          <div className="todo-list" aria-label="Todo list">
            {todos.length === 0 ? (
              <div className="empty-state">Nothing queued up.</div>
            ) : (
              todos.map((todo) => (
                <TodoRow
                  key={todo.id}
                  todo={todo}
                  onToggle={() => updateTodo(todo.id, { completed: !todo.completed })}
                  onTitle={(title) => updateTodo(todo.id, { title })}
                  onDelete={() => deleteTodo(todo.id)}
                />
              ))
            )}
          </div>
        </section>
      )}

      <section className="ops-strip">
        <span>Audit queue: {meta?.auditDepth.visible ?? 0} visible</span>
        <span>{meta?.forge.length ?? 0} Forge primitives reporting</span>
      </section>
    </main>
  )
}

function TodoRow({
  todo,
  onToggle,
  onTitle,
  onDelete,
}: {
  todo: Todo
  onToggle: () => void
  onTitle: (title: string) => void
  onDelete: () => void
}) {
  const [draft, setDraft] = useState(todo.title)

  useEffect(() => setDraft(todo.title), [todo.title])

  return (
    <article className={`todo-row ${todo.completed ? 'done' : ''}`}>
      <button
        type="button"
        className="check-btn"
        onClick={onToggle}
        aria-label={todo.completed ? 'Mark incomplete' : 'Mark complete'}
      >
        {todo.completed ? <CheckCircle2 size={22} /> : <Circle size={22} />}
      </button>
      <input
        aria-label="Todo title"
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          const title = draft.trim()
          if (title && title !== todo.title) onTitle(title)
          else setDraft(todo.title)
        }}
        onKeyDown={(event) => {
          if (event.key === 'Enter') event.currentTarget.blur()
        }}
      />
      <button type="button" className="delete-btn" onClick={onDelete} aria-label="Delete todo">
        <Trash2 size={18} />
      </button>
    </article>
  )
}

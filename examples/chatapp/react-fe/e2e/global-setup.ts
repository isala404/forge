import type { FullConfig } from '@playwright/test'

const READY_TIMEOUT_MS = Number(process.env.PLAYWRIGHT_STACK_READY_TIMEOUT_MS ?? 120_000)
const POLL_MS = 1_000
const FETCH_TIMEOUT_MS = 2_500

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

async function fetchWithTimeout(url: string, init: RequestInit = {}): Promise<Response> {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS)
  try {
    return await fetch(url, { ...init, signal: controller.signal })
  } finally {
    clearTimeout(timer)
  }
}

async function waitFor(label: string, probe: () => Promise<void>): Promise<void> {
  const deadline = Date.now() + READY_TIMEOUT_MS
  let lastError: unknown

  while (Date.now() < deadline) {
    try {
      await probe()
      console.log(`ready: ${label}`)
      return
    } catch (error) {
      lastError = error
      await sleep(POLL_MS)
    }
  }

  throw new Error(`${label} was not ready within ${READY_TIMEOUT_MS}ms: ${describeError(lastError)}`)
}

async function waitForFrontend(name: string, url: string): Promise<void> {
  await waitFor(`${name} frontend`, async () => {
    const res = await fetchWithTimeout(url)
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
  })
}

async function waitForBackend(name: string, url: string): Promise<void> {
  await waitFor(`${name} backend`, async () => {
    const res = await fetchWithTimeout(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ query: 'query Ready { __typename }' }),
    })
    if (!res.ok) throw new Error(`HTTP ${res.status}`)

    const json = (await res.json()) as { data?: unknown; errors?: unknown }
    if (json.errors || !json.data) {
      throw new Error(`GraphQL readiness failed: ${JSON.stringify(json.errors ?? json)}`)
    }
  })
}

type StackTarget = {
  name: string
  fe: string
  be: string
}

function targetsFromConfig(config: FullConfig): StackTarget[] {
  const targets = config.projects.map(project => {
    const fe = project.use.baseURL
    const be = project.metadata?.backend

    if (typeof fe !== 'string') throw new Error(`project ${project.name} is missing use.baseURL`)
    if (typeof be !== 'string') throw new Error(`project ${project.name} is missing metadata.backend`)

    return { name: project.name, fe, be }
  })

  if (targets.length === 0) throw new Error('no Playwright projects configured')
  return targets
}

export default async function globalSetup(config: FullConfig): Promise<void> {
  for (const target of targetsFromConfig(config)) {
    await waitForBackend(target.name, target.be)
    await waitForFrontend(target.name, target.fe)
  }
}

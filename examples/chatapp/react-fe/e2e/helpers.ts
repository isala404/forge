import { expect, type Page, type TestInfo } from '@playwright/test'

export function backendOf(testInfo: TestInfo): string {
  const be = testInfo.project.metadata?.backend
  if (typeof be !== 'string') throw new Error('project metadata.backend not set')
  return be
}

// Unique per call so reruns never collide on the username unique constraint.
export function uniqueUser(prefix: string): string {
  const rand = Math.floor(Math.random() * 1e9).toString(36)
  return `${prefix}_${Date.now().toString(36)}_${rand}`
}

// Create an account straight through the API — test setup, not the thing under test.
export async function signup(
  backend: string,
  username: string,
  displayName: string,
  password = 'password123',
): Promise<void> {
  const res = await fetch(backend, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      query: `mutation($u:String!,$d:String!,$p:String!){ signup(username:$u,displayName:$d,password:$p){ token } }`,
      variables: { u: username, d: displayName, p: password },
    }),
  })
  const json = (await res.json()) as { data?: { signup?: { token?: string } }; errors?: unknown }
  if (!json.data?.signup?.token) {
    throw new Error(`signup failed for ${username}: ${JSON.stringify(json.errors ?? json)}`)
  }
}

// Sign in through the UI and wait for the authenticated shell. Pressing Enter in the
// password field submits the form, avoiding the two "Sign in" buttons (tab + submit).
export async function loginViaUi(page: Page, username: string, password = 'password123'): Promise<void> {
  await page.goto('/')
  await page.getByPlaceholder('ada').fill(username)
  const pw = page.getByPlaceholder('At least 6 characters')
  await pw.fill(password)
  await pw.press('Enter')
  await expect(page.getByPlaceholder('Search chats')).toBeVisible()
}

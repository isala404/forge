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

// Minimal GraphQL POST for test setup. Throws on transport or GraphQL errors so a
// broken precondition fails the test loudly rather than silently.
async function gql<T>(
  backend: string,
  query: string,
  variables: Record<string, unknown>,
  token?: string,
): Promise<T> {
  const headers: Record<string, string> = { 'content-type': 'application/json' }
  if (token) headers.authorization = `Bearer ${token}`
  const res = await fetch(backend, {
    method: 'POST',
    headers,
    body: JSON.stringify({ query, variables }),
  })
  const json = (await res.json()) as { data?: T; errors?: unknown }
  if (json.errors || !json.data) {
    throw new Error(`graphql error: ${JSON.stringify(json.errors ?? json)}`)
  }
  return json.data
}

// Sign in through the API and return the bearer token (test setup, not under test).
export async function login(backend: string, username: string, password = 'password123'): Promise<string> {
  const data = await gql<{ login: { token: string } }>(
    backend,
    `mutation($u:String!,$p:String!){ login(username:$u,password:$p){ token } }`,
    { u: username, p: password },
  )
  return data.login.token
}

// Create a direct chat with one other user through the API and return its id.
export async function createDirectChat(backend: string, token: string, otherUsername: string): Promise<string> {
  const data = await gql<{ createChat: { id: string } }>(
    backend,
    `mutation($m:[String!]!){ createChat(kind:DIRECT, title:null, memberUsernames:$m){ id } }`,
    { m: [otherUsername] },
    token,
  )
  return data.createChat.id
}

// Send a message into a chat through the API (used to drive a live event at a peer).
export async function sendMessageApi(backend: string, token: string, chatId: string, body: string): Promise<void> {
  await gql(
    backend,
    `mutation($c:ID!,$b:String!){ sendMessage(chatId:$c, body:$b){ id } }`,
    { c: chatId, b: body },
    token,
  )
}

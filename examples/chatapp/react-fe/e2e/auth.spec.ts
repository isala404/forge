import { test, expect } from '@playwright/test'
import { backendOf, signup, uniqueUser } from './helpers'

// Guards the rust-be regression where GET /graphql was routed only to the websocket
// upgrade handler, so the SPA's GET-method boot queries (me, chats) returned 400 and
// login appeared to do nothing: the app stayed on the sign-in screen. The fix makes
// GET serve plain queries too, matching node and python.
test('signing in lands in the authenticated app shell', async ({ page }, testInfo) => {
  const username = uniqueUser('auth')
  await signup(backendOf(testInfo), username, 'Auth Probe')

  await page.goto('/')
  await page.getByPlaceholder('ada').fill(username)
  const pw = page.getByPlaceholder('At least 6 characters')
  await pw.fill('password123')
  await pw.press('Enter')

  // The chat search box only exists once authenticated; the password field is gone.
  await expect(page.getByPlaceholder('Search chats')).toBeVisible()
  await expect(page.getByPlaceholder('At least 6 characters')).toHaveCount(0)
})

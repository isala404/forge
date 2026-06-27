import { test, expect, type Page } from '@playwright/test'
import {
  backendOf,
  createDirectChat,
  login,
  loginViaUi,
  sendMessageApi,
  signup,
  uniqueUser,
} from './helpers'

// Guards the post-login live-reactivity bug: the urql wiring used to `dispose()` the
// graphql-ws client whenever the auth token changed. `dispose()` is terminal: the
// client can never be reused, so after the very first UI login no subscription could
// ever connect, and live messages only appeared after a full page reload re-created
// the client. The fix uses `terminate()` (drop the socket, keep the client). Here a
// freshly-logged-in Alice must receive Bob's message live, with no reload.
test('a freshly logged-in user receives a peer message live without reloading', async ({ page }, testInfo) => {
  const backend = backendOf(testInfo)
  const alice = uniqueUser('alice')
  const bob = uniqueUser('bob')
  await signup(backend, alice, 'Alice E2E')
  await signup(backend, bob, 'Bob E2E')

  // Set up the chat out of band so we hold its id for Bob's API-driven send.
  const aliceToken = await login(backend, alice)
  const chatId = await createDirectChat(backend, aliceToken, bob)

  // Watch every socket this page opens for the moment Alice's messageAdded
  // subscription is sent, which means the live channel is established, so Bob's
  // send afterwards can't be lost to the at-most-once pubsub. Registered before the
  // first navigation so the very first socket is observed.
  const messageSubscribed = waitForFrame(page, 'messageAdded')

  await loginViaUi(page, alice)

  // Open the conversation; this is what mounts the messageAdded subscription.
  await page.locator('.chat-row', { hasText: 'Bob E2E' }).click()
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await messageSubscribed

  const text = `live-${Date.now().toString(36)}`
  const bobToken = await login(backend, bob)
  await sendMessageApi(backend, bobToken, chatId, text)

  // Arrives over the live subscription on the never-reloaded page.
  await expect(page.locator('.bubble-body', { hasText: text })).toBeVisible()
})

// Resolve once any WebSocket on the page sends a text frame containing `marker`.
function waitForFrame(page: Page, marker: string): Promise<void> {
  return new Promise<void>((resolve) => {
    page.on('websocket', (ws) => {
      ws.on('framesent', (frame) => {
        if (typeof frame.payload === 'string' && frame.payload.includes(marker)) {
          resolve()
        }
      })
    })
  })
}

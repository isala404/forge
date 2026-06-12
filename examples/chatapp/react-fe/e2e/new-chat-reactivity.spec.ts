import { test, expect } from '@playwright/test'
import { backendOf, loginViaUi, signup, uniqueUser } from './helpers'

// Guards the reactivity bug where creating a brand-new chat and immediately sending the
// first message left both the conversation ("No messages yet") and the sidebar ("No
// conversations yet") empty until a manual reload. The sender's own message was only
// surfaced via the at-most-once `messageAdded` subscription, which a just-created chat
// often hasn't finished subscribing to before the first message's fanout publishes, so
// the echo is lost (most visible on the slower python backend). The fix writes the sent
// message and the new chat into the graphcache so the view updates from the mutation
// result, independent of the subscription.
test('new chat: first message and the chat appear without a reload', async ({ page }, testInfo) => {
  const backend = backendOf(testInfo)
  const alice = uniqueUser('alice')
  const bob = uniqueUser('bob')
  await signup(backend, alice, 'Alice E2E')
  await signup(backend, bob, 'Bob E2E')

  await loginViaUi(page, alice)

  await page.getByRole('button', { name: 'New chat' }).click()
  const dialog = page.getByRole('dialog', { name: 'New conversation' })
  await dialog.getByPlaceholder('ada').fill(bob)
  await dialog.getByRole('button', { name: 'Add member' }).click()
  await dialog.getByRole('button', { name: 'Create' }).click()

  const composer = page.getByRole('textbox', { name: 'Message' })
  await expect(composer).toBeVisible()

  const text = `first-message-${Date.now().toString(36)}`
  await composer.fill(text)
  await composer.press('Enter')

  // The message bubble renders in the conversation (scoped to `.bubble-body` so we are
  // not just seeing the sidebar's last-message preview), and the chat shows in the
  // sidebar — both without any page reload.
  await expect(page.locator('.bubble-body', { hasText: text })).toBeVisible()
  await expect(page.locator('.chat-row-name', { hasText: 'Bob E2E' })).toBeVisible()
})

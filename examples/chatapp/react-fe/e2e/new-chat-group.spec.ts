import { test, expect } from '@playwright/test'
import { backendOf, loginViaUi, signup, uniqueUser } from './helpers'

// The New-chat modal infers the kind from how many people you add: one other person
// is a direct chat, two or more is a group. The group-name field only appears once a
// second member is added, and it is optional; a blank name leaves the group titleless
// and the UI derives a display name from its members. This guards that behaviour and
// the titleless-group create path across all three backends.
test('adding a second member turns the new chat into a group with an optional name', async ({ page }, testInfo) => {
  const backend = backendOf(testInfo)
  const alice = uniqueUser('alice')
  const bob = uniqueUser('bob')
  const carol = uniqueUser('carol')
  await signup(backend, alice, 'Alice E2E')
  await signup(backend, bob, 'Bob E2E')
  await signup(backend, carol, 'Carol E2E')

  await loginViaUi(page, alice)

  await page.getByRole('button', { name: 'New chat' }).click()
  const dialog = page.getByRole('dialog', { name: 'New conversation' })
  const input = dialog.getByPlaceholder('Add a username, press Enter')
  const groupName = dialog.getByPlaceholder('Weekend plans')

  await input.fill(bob)
  await dialog.getByRole('button', { name: 'Add member' }).click()
  await expect(groupName).toHaveCount(0)

  await input.fill(carol)
  await dialog.getByRole('button', { name: 'Add member' }).click()
  await expect(groupName).toBeVisible()

  // Leave the name blank and create; a titleless group is allowed.
  await dialog.getByRole('button', { name: 'Create' }).click()

  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  const row = page.locator('.chat-row', { hasText: 'Bob E2E' })
  await expect(row).toContainText('Carol E2E')
  await expect(row.locator('.chat-row-tag')).toBeVisible()
})

import { test, expect } from '@playwright/test'
import { backendOf, loginViaUi, signup, uniqueUser } from './helpers'

// Guards the attachment-upload path end to end. Presigned URLs are relative
// (`/api/files`), so the browser PUTs them to the SPA origin; nginx must proxy
// that to the backend, which verifies the signature and stores the bytes. The bug
// this protects against was the SPA origin answering the PUT itself: a static-only
// nginx returned 405, and a body over its default 1 MiB limit returned 413; the
// upload never reached the backend at all. We assert the PUT returns 200 and the
// attachment renders, using a >1 MiB file so the old 413 path would also fail.
test('attachment uploads through the blob proxy and renders in the conversation', async ({ page }, testInfo) => {
  const backend = backendOf(testInfo)
  const alice = uniqueUser('alice')
  const bob = uniqueUser('bob')
  await signup(backend, alice, 'Alice E2E')
  await signup(backend, bob, 'Bob E2E')

  await loginViaUi(page, alice)

  // Open a direct chat so the composer (and its file input) is on screen.
  await page.getByRole('button', { name: 'New chat' }).click()
  const dialog = page.getByRole('dialog', { name: 'New conversation' })
  await dialog.getByPlaceholder('Add a username, press Enter').fill(bob)
  await dialog.getByRole('button', { name: 'Add member' }).click()
  await dialog.getByRole('button', { name: 'Create' }).click()
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()

  // Watch the presigned PUT specifically. Over 1 MiB, so the pre-fix nginx (static
  // SPA, default body cap) would have answered 405/413 before the backend saw it.
  const putDone = page.waitForResponse(
    (r) => r.url().includes('/api/files') && r.request().method() === 'PUT',
  )

  const big = Buffer.alloc(2 * 1024 * 1024, 7)
  await page.locator('input[type="file"]').setInputFiles({
    name: 'report.bin',
    mimeType: 'application/octet-stream',
    buffer: big,
  })

  const putRes = await putDone
  expect(putRes.status(), 'presigned PUT must reach the backend, not the static SPA').toBe(200)

  // The composer shows the staged attachment once the upload succeeds (onPickFile
  // only sets the pending state on a 200), then sending renders it as a bubble.
  await expect(page.locator('.composer-attachment')).toBeVisible()
  await page.getByRole('textbox', { name: 'Message' }).fill('here is the file')
  await page.getByRole('textbox', { name: 'Message' }).press('Enter')
  await expect(page.locator('.bubble-media')).toBeVisible()
})

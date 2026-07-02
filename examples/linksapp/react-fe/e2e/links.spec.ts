import { expect, test, type TestInfo } from '@playwright/test'

function apiBase(testInfo: TestInfo): string {
  return testInfo.project.metadata.apiBase as string
}

test('signup and link lifecycle works through the selected REST backend', async ({
  page,
}, testInfo) => {
  const backend = testInfo.project.name
  const email = `links-${backend}-${Date.now()}@example.com`
  const api = apiBase(testInfo)

  await page.goto(`/?api=${encodeURIComponent(api)}`)
  await expect(page.getByRole('heading', { name: 'Forge Links' })).toBeVisible()
  await expect(page.getByRole('button', { name: backend })).toHaveClass(/active/)

  await page.getByLabel('Email').fill(email)
  await page.getByLabel('Password').fill('password123')
  await page.getByRole('button', { name: 'Create account' }).click()

  await expect(page.getByRole('heading', { name: 'Your links' })).toBeVisible()
  await expect(page.getByText(email)).toBeVisible()

  await page.getByTestId('url-input').fill('https://example.com/some-long-path')
  await page.getByRole('button', { name: 'Shorten' }).click()

  const row = page.getByTestId('link-row').first()
  await expect(row).toBeVisible()

  const shortUrlEl = row.getByTestId('short-url')
  await expect(shortUrlEl).toBeVisible()
  const shortUrlText = await shortUrlEl.textContent()
  expect(shortUrlText).toMatch(new RegExp(`^${api}/`))

  const qrImg = row.getByTestId('qr-img')
  await expect(qrImg).toBeVisible()
  const qrSrc = await qrImg.getAttribute('src')
  expect(qrSrc).toContain('/api/links/')
  expect(qrSrc).toContain('/qr.svg')

  await row.getByTestId('delete-link').click()

  await expect(page.getByTestId('link-row')).toHaveCount(0)
  await expect(page.getByText('No links yet.')).toBeVisible()
})

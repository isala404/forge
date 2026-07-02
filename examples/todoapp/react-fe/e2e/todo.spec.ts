import { expect, test, type TestInfo } from '@playwright/test'

function apiBase(testInfo: TestInfo): string {
  return testInfo.project.metadata.apiBase as string
}

test('signup and todo lifecycle works through the selected REST backend', async ({
  page,
}, testInfo) => {
  const backend = testInfo.project.name
  const email = `todo-${backend}-${Date.now()}@example.com`
  const api = apiBase(testInfo)

  await page.goto(`/?api=${encodeURIComponent(api)}`)
  await expect(page.getByRole('heading', { name: 'Forge Todos' })).toBeVisible()
  await expect(page.getByRole('button', { name: backend })).toHaveClass(/active/)

  await page.getByLabel('Email').fill(email)
  await page.getByLabel('Password').fill('password123')
  await page.getByRole('button', { name: 'Create account' }).click()

  await expect(page.getByRole('heading', { name: 'Tasks' })).toBeVisible()
  await expect(page.getByText(email)).toBeVisible()

  await page.getByPlaceholder('New todo').fill(`Ship REST todo on ${backend}`)
  await page.getByRole('button', { name: 'Add' }).click()

  const title = page.getByLabel('Todo title').first()
  await expect(title).toHaveValue(`Ship REST todo on ${backend}`)

  await page.getByRole('button', { name: 'Mark complete' }).click()
  await expect(page.getByText('1 of 1 complete')).toBeVisible()

  await title.fill(`Polish REST todo on ${backend}`)
  const patched = page.waitForResponse(
    (response) => response.url().includes('/api/todos/') && response.request().method() === 'PATCH',
  )
  await title.press('Enter')
  await expect((await patched).ok()).toBeTruthy()
  await expect(title).toHaveValue(`Polish REST todo on ${backend}`)

  await page.reload()
  await expect(page.getByRole('heading', { name: 'Tasks' })).toBeVisible()
  await expect(page.getByLabel('Todo title').first()).toHaveValue(`Polish REST todo on ${backend}`)
  await expect(page.getByText('1 of 1 complete')).toBeVisible()

  await page.getByRole('button', { name: 'Delete todo' }).click()
  await expect(page.getByLabel('Todo title')).toHaveCount(0)
  await expect(page.getByText('Nothing queued up.')).toBeVisible()
})

import { defineConfig } from '@playwright/test'

const TARGETS = {
  rust: 'http://127.0.0.1:9081',
  node: 'http://127.0.0.1:9082',
  python: 'http://127.0.0.1:9083',
} as const

export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  expect: { timeout: 8_000 },
  fullyParallel: false,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://127.0.0.1:5174',
    headless: true,
    trace: 'retain-on-failure',
  },
  projects: Object.entries(TARGETS).map(([name, apiBase]) => ({
    name,
    metadata: { apiBase },
  })),
})

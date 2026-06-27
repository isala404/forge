import { defineConfig } from '@playwright/test'

const TARGETS = {
  rust: 'http://127.0.0.1:9091',
  node: 'http://127.0.0.1:9092',
  python: 'http://127.0.0.1:9093',
} as const

export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  expect: { timeout: 8_000 },
  fullyParallel: false,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://127.0.0.1:5175',
    headless: true,
    trace: 'retain-on-failure',
  },
  projects: Object.entries(TARGETS).map(([name, apiBase]) => ({
    name,
    metadata: { apiBase },
  })),
})

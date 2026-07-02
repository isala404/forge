import { defineConfig } from '@playwright/test'
import { TARGETS } from './e2e/targets'

// E2E against the running docker stacks (examples/chatapp/docker-compose.yml). The
// same SPA is built three times, one per backend, so each project points the browser
// at one SPA (`baseURL`) and carries that backend's GraphQL URL in `metadata.backend`
// for direct API setup (creating test users). Bring the stacks up first:
//   docker compose up --build        (from examples/chatapp/)
export default defineConfig({
  testDir: './e2e',
  globalSetup: './e2e/global-setup.ts',
  timeout: 30_000,
  expect: { timeout: 8_000 },
  fullyParallel: false,
  retries: 0,
  reporter: [['list']],
  use: { headless: true, trace: 'retain-on-failure' },
  projects: Object.entries(TARGETS).map(([name, urls]) => ({
    name,
    use: { baseURL: urls.fe },
    metadata: { backend: urls.be },
  })),
})

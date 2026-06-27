import { defineConfig } from '@playwright/test'

// OrbStack e2e config. Same as playwright.config.ts, but the test-setup API calls (`be`)
// go to the backends' orb.local hostnames instead of the non-forwarding 127.0.0.1:808x.
// The SPA (`fe`) ports 8091-8093 forward to the host normally. Bring the stack up with
// `docker compose --env-file ../.env.orb up` so the in-browser app hits the same orb.local backends.
const TARGETS = {
  rust: { fe: 'http://localhost:8091', be: 'http://chatapp-rs-app-1.orb.local:8081/graphql' },
  node: { fe: 'http://localhost:8092', be: 'http://chatapp-node-app-1.orb.local:8082/graphql' },
  python: { fe: 'http://localhost:8093', be: 'http://chatapp-pybe-backend-1.orb.local:8083/graphql' },
} as const

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

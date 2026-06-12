import { defineConfig } from '@playwright/test'

// E2E against the running docker stacks (examples/chatapp/docker-compose.yml). The
// same SPA is built three times, one per backend, so each project points the browser
// at one SPA (`baseURL`) and carries that backend's GraphQL URL in `metadata.backend`
// for direct API setup (creating test users). Bring the stacks up first:
//   docker compose up --build        (from examples/chatapp/)
const TARGETS = {
  rust: { fe: 'http://localhost:8091', be: 'http://localhost:8081/graphql' },
  node: { fe: 'http://localhost:8092', be: 'http://localhost:8082/graphql' },
  python: { fe: 'http://localhost:8093', be: 'http://localhost:8083/graphql' },
} as const

export default defineConfig({
  testDir: './e2e',
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

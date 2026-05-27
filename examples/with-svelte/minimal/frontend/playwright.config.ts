import { defineConfig, devices } from "@playwright/test";

const TEST_URL = process.env.FORGE_TEST_URL;

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  timeout: 90_000,
  workers: process.env.CI ? 1 : undefined,
  reporter: "html",
  globalSetup: "./tests/global-setup.ts",
  use: {
    baseURL: TEST_URL || "http://localhost:9080",
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  ...(TEST_URL
    ? {}
    : {
        webServer: {
          command: "bun run dev --port 9080",
          url: "http://localhost:9080",
          reuseExistingServer: true,
          timeout: 120 * 1000,
        },
      }),
});

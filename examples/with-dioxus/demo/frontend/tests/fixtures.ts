import { test as base, expect, type Page } from "@playwright/test";

export { expect };

export const API_URL =
  process.env.FORGE_TEST_URL ||
  process.env.VITE_API_URL ||
  "http://localhost:9081";
export const ACTION_TIMEOUT = process.env.CI ? 30_000 : 30_000;

export function uniqueId(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

export function trackConsoleErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      const text = msg.text();
      if (
        !text.includes("net::ERR") &&
        !text.includes("favicon") &&
        !text.includes("EventSource") &&
        !text.includes("_dioxus")
      ) {
        errors.push(text);
      }
    }
  });
  return errors;
}

type ForgeFixtures = {
  rpc: (fn: string, args?: unknown) => Promise<unknown>;
  gotoReady: (path?: string) => Promise<void>;
  // Capture a full-page screenshot at the end of every test. Saved to
  // `${testInfo.outputDir}/<slug>.png` so each route's final UI state lands
  // alongside Playwright's other artifacts (trace.zip, video.webm) when a
  // failure happens. The artifact bundle is uploaded by CI without any extra
  // wiring.
  routeScreenshot: void;
};

export const test = base.extend<ForgeFixtures>({
  routeScreenshot: [
    async ({ page }, use, testInfo) => {
      await use();
      if (page.isClosed()) return;
      const slug = testInfo.title.replace(/[^a-z0-9]+/gi, "-").toLowerCase();
      try {
        await page.screenshot({
          path: testInfo.outputPath(`${slug || "route"}.png`),
          fullPage: true,
        });
      } catch {
        // Test already navigated away or context closed; the assertion-level
        // failure (if any) is what we want surfaced, not the screenshot error.
      }
    },
    { auto: true },
  ],
  // eslint-disable-next-line no-empty-pattern
  rpc: async ({}, use) => {
    await use(async (fn: string, args: unknown = null) => {
      for (let attempt = 1; attempt <= 3; attempt++) {
        try {
          const res = await fetch(`${API_URL}/_api/rpc/${fn}`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ args }),
          });
          if (!res.ok) {
            const body = await res.text();
            throw new Error(`RPC ${fn} failed (${res.status}): ${body}`);
          }
          return (await res.json()).data;
        } catch (err) {
          // Retry connection errors (backend restarting), not HTTP errors
          const isConnectionError =
            err instanceof TypeError &&
            (err as TypeError).message === "fetch failed";
          if (!isConnectionError || attempt === 3) throw err;
          await new Promise((r) => setTimeout(r, attempt * 1000));
        }
      }
    });
  },

  gotoReady: async ({ page }, use) => {
    await use(async (path = "/") => {
      // Wait for the subscription registration response, not just the SSE
      // connection. This signals that reactivity is fully wired up.
      // WASM apps need extra time: download → instantiate → init → SSE → subscribe.
      // Global setup pre-compiles the WASM, but the browser still has to download
      // and instantiate the binary on each page load.
      const subscribed = page.waitForResponse(
        (res) => res.url().includes("/_api/subscribe") && res.status() === 200,
        { timeout: ACTION_TIMEOUT * 3 },
      );
      await page.goto(path);
      await subscribed;
    });
  },
});

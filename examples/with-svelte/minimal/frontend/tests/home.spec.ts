import { test, expect, trackConsoleErrors } from "./fixtures";

test("application boots without console errors", async ({ page }) => {
  const errors = trackConsoleErrors(page);

  await page.goto("/");

  await expect(page.locator("body")).toBeVisible();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByRole("link", { name: "About" })).toBeVisible();
  expect(errors).toHaveLength(0);
});

test("home page points the Backend link at the configured API URL", async ({
  page,
}) => {
  await page.goto("/");

  // `forge test` clears PUBLIC_API_URL so the embedded frontend serves
  // same-origin with relative URLs. Regression guard: the page used to read a
  // non-existent VITE_API_URL var and silently fall back to a hardcoded
  // localhost:8080 — which would make this an absolute URL instead.
  const backendLink = page.locator("p.subtitle a");
  await expect(backendLink).toHaveAttribute("href", "/health");
});

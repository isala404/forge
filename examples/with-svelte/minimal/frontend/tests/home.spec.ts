import { test, expect, trackConsoleErrors, API_URL } from "./fixtures";

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

  // Regression guard: the page used to read a non-existent VITE_API_URL var
  // and silently fall back to localhost:8080 instead of PUBLIC_API_URL.
  const backendLink = page.locator("p.subtitle a");
  await expect(backendLink).toHaveText(API_URL);
  await expect(backendLink).toHaveAttribute("href", `${API_URL}/health`);
});

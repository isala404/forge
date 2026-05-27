import {
  test,
  expect,
  ACTION_TIMEOUT,
  uniqueId,
  trackConsoleErrors,
} from "./fixtures";
import type { Page } from "@playwright/test";

const INPUT = 'input[placeholder="What needs to be done?"]';
const EMAIL = 'input[type="email"]';
const PASSWORD = 'input[type="password"]';
const NAME = 'input[placeholder="Name"]';

async function signUp(
  page: Page,
  email: string,
  name: string,
  password: string,
) {
  await page.getByRole("button", { name: "Sign up" }).first().click();
  await page.fill(NAME, name);
  await page.fill(EMAIL, email);
  await page.fill(PASSWORD, password);
  await page.getByRole("button", { name: "Sign up" }).last().click();
  await expect(page.locator(INPUT)).toBeVisible({ timeout: ACTION_TIMEOUT });
}

// The app only subscribes to the todos query once authenticated, so reactivity
// readiness can't be detected until after sign-up. Arm the subscribe wait
// before submitting, then await it once the authed view renders. (×3 timeout
// for the WASM download → instantiate → init → SSE → subscribe path.)
async function signUpReady(
  page: Page,
  email: string,
  name: string,
  password: string,
) {
  const subscribed = page.waitForResponse(
    (res) => res.url().includes("/_api/subscribe") && res.status() === 200,
    { timeout: ACTION_TIMEOUT * 3 },
  );
  await signUp(page, email, name, password);
  await subscribed;
}

test("authenticated user can create, toggle, and delete their todos", async ({
  page,
}) => {
  const errors = trackConsoleErrors(page);
  const email = `${uniqueId("user")}@test.local`;

  await page.goto("/");
  await signUpReady(page, email, "Solo", "password123");

  const title = uniqueId("release");
  await page.fill(INPUT, title);
  await page.click(".input-row button");

  const todoItem = page.locator("li", { hasText: title });
  await expect(todoItem).toBeVisible({ timeout: ACTION_TIMEOUT });
  await expect(page.locator(".count")).toHaveText("1 remaining", {
    timeout: ACTION_TIMEOUT,
  });

  await todoItem.locator("button.toggle").click();
  await expect(todoItem).toHaveClass(/completed/, { timeout: ACTION_TIMEOUT });

  await todoItem.locator("button.delete").click();
  await expect(todoItem).not.toBeVisible({ timeout: ACTION_TIMEOUT });
  expect(errors).toHaveLength(0);
});

test("two users cannot see each other's todos", async ({ browser }) => {
  const aliceEmail = `${uniqueId("alice")}@test.local`;
  const bobEmail = `${uniqueId("bob")}@test.local`;
  const aliceTitle = uniqueId("alice-task");
  const bobTitle = uniqueId("bob-task");

  const aliceCtx = await browser.newContext();
  const alice = await aliceCtx.newPage();
  await alice.goto("/");
  await signUpReady(alice, aliceEmail, "Alice", "password123");
  await alice.fill(INPUT, aliceTitle);
  await alice.click(".input-row button");
  await expect(alice.locator("li", { hasText: aliceTitle })).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });

  const bobCtx = await browser.newContext();
  const bob = await bobCtx.newPage();
  await bob.goto("/");
  await signUpReady(bob, bobEmail, "Bob", "password123");
  await bob.fill(INPUT, bobTitle);
  await bob.click(".input-row button");
  await expect(bob.locator("li", { hasText: bobTitle })).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });

  await expect(bob.locator("li", { hasText: aliceTitle })).toHaveCount(0);
  await expect(alice.locator("li", { hasText: bobTitle })).toHaveCount(0);

  await aliceCtx.close();
  await bobCtx.close();
});

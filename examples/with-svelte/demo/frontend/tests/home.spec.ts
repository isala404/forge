import {
  test,
  expect,
  API_URL,
  ACTION_TIMEOUT,
  uniqueId,
  trackConsoleErrors,
} from "./fixtures";
import type { Page } from "@playwright/test";

// User CRUD requires the `admin` role, `confirm_verification` and
// `trigger_demo_webhook` require an authenticated session. The seeded
// `demo@example.com` user is an admin (see migrations/0001_initial.sql).
// Prefill is dev-only, so production-built bundles need explicit credentials.
async function loginAsAdmin(page: Page) {
  const auth = page.locator("section", {
    has: page.getByText("refresh tokens"),
  });
  await auth.getByPlaceholder("Email").fill("demo@example.com");
  await auth.getByPlaceholder(/Password/).fill("password123");
  // Logging in rotates the token; the client tears down the anonymous SSE
  // stream and re-subscribes every query over a fresh authenticated one. Wait
  // for that re-subscription so reactive reads (and job/webhook push updates)
  // reflect the authenticated session before the test interacts.
  const resubscribed = page.waitForResponse(
    (res) => res.url().includes("/_api/subscribe") && res.status() === 200,
    { timeout: ACTION_TIMEOUT * 3 },
  );
  await auth.locator('button[type="submit"]').click();
  await expect(auth.getByText("Logged in as")).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });
  await resubscribed;
}

async function signDemoWebhook(body: string): Promise<string> {
  const encoder = new TextEncoder();
  const keyData = await crypto.subtle.importKey(
    "raw",
    encoder.encode("demo-secret"),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const sig = await crypto.subtle.sign("HMAC", keyData, encoder.encode(body));
  return [...new Uint8Array(sig)]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

test("users CRUD stays reactive through create, edit, and delete", async ({
  page,
  gotoReady,
}) => {
  const errors = trackConsoleErrors(page);
  const name = uniqueId("Create");
  const email = `${name.toLowerCase()}@test.com`;
  const updatedName = uniqueId("Edited");

  await gotoReady();
  await loginAsAdmin(page);

  const section = page.locator("section", {
    has: page.getByRole("heading", { name: /users/i }),
  });

  await section.getByPlaceholder("Name").fill(name);
  await section.getByPlaceholder("Email").fill(email);
  await section.getByRole("button", { name: "Create" }).click();

  const row = page.locator("tr", { hasText: name });
  await expect(row).toBeVisible({ timeout: ACTION_TIMEOUT });
  await expect(row.getByText(email)).toBeVisible({ timeout: ACTION_TIMEOUT });

  await row.getByRole("button", { name: "Edit" }).click();
  const editRow = page.locator("tr.editing");
  await expect(editRow).toBeVisible();
  const nameInput = editRow.locator('input[type="text"]');
  await nameInput.clear();
  await nameInput.fill(updatedName);
  await editRow.getByRole("button", { name: "Save" }).click();

  const updatedRow = page.locator("tr", { hasText: updatedName });
  await expect(updatedRow).toBeVisible({ timeout: ACTION_TIMEOUT });

  await updatedRow.getByRole("button", { name: "Delete" }).click();
  await updatedRow.getByRole("button", { name: "Confirm" }).click();
  await expect(updatedRow).not.toBeVisible({ timeout: ACTION_TIMEOUT });
  expect(errors).toHaveLength(0);
});

test("export job and verification workflow complete from the UI", async ({
  page,
  gotoReady,
}) => {
  await gotoReady();
  // `confirm_verification` requires an authenticated session.
  await loginAsAdmin(page);

  const exportSection = page.locator("section", {
    has: page.getByText("Export Job"),
  });
  // Export job has ~8s of artificial delays (10 × 800ms) plus DB/SSE overhead
  const JOB_TIMEOUT = 30_000;

  await exportSection.getByRole("button", { name: "Start Export" }).click();
  await expect(exportSection.getByText(/Export complete/i)).toBeVisible({
    timeout: JOB_TIMEOUT,
  });
  await expect(exportSection.getByText(/100%/)).toBeVisible();

  const verificationSection = page.locator("section", {
    has: page.getByText("Verification"),
  });
  await verificationSection
    .getByRole("button", { name: "Start Workflow" })
    .click();

  // Workflow pauses at "await_confirmation" step, waiting for user to click confirm
  const confirmBtn = verificationSection.getByRole("button", {
    name: "Confirm Verification",
  });
  await expect(confirmBtn).toBeVisible({ timeout: JOB_TIMEOUT });
  await confirmBtn.click();

  // After confirmation, remaining steps complete (includes wait_period durable sleep)
  await expect(verificationSection.locator(".step.completed")).toHaveCount(6, {
    timeout: JOB_TIMEOUT,
  });
});

test("auth flow logs in, refreshes, and logs out cleanly", async ({
  page,
  gotoReady,
}) => {
  await gotoReady();

  const section = page.locator("section", {
    has: page.getByText("refresh tokens"),
  });

  // Prefill is dev-only; the production bundle ships empty fields.
  await section.getByPlaceholder("Email").fill("demo@example.com");
  await section.getByPlaceholder(/Password/).fill("password123");
  await section.locator('button[type="submit"]').click();
  await expect(section.getByText("Logged in as")).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });

  await section.getByRole("button", { name: "Refresh Token" }).click();
  await expect(section.getByText(/Token refreshed 1 time/)).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });

  await section.getByRole("button", { name: "Logout" }).click();
  await expect(section.getByPlaceholder("Email")).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });
  await expect(section.getByText("Logged in as")).not.toBeVisible();
});

test("webhook endpoint rejects bad signatures and surfaces accepted events", async ({
  page,
  gotoReady,
  request,
}) => {
  const ts = () => Math.floor(Date.now() / 1000).toString();
  const badResponse = await request.post(`${API_URL}/_api/webhooks/demo`, {
    headers: {
      "Content-Type": "application/json",
      "X-Webhook-Signature": "invalid",
      "X-Webhook-Timestamp": ts(),
      "X-Idempotency-Key": `bad-${Date.now()}`,
    },
    data: { action: "test" },
  });
  expect(badResponse.status()).toBe(401);

  const key = `event-${Date.now()}`;
  const body = JSON.stringify({ action: "test" });
  const signature = await signDemoWebhook(body);
  const accepted = await request.post(`${API_URL}/_api/webhooks/demo`, {
    headers: {
      "Content-Type": "application/json",
      "X-Webhook-Signature": signature,
      "X-Webhook-Timestamp": ts(),
      "X-Idempotency-Key": key,
    },
    data: JSON.parse(body),
  });
  expect(accepted.ok()).toBeTruthy();

  await gotoReady();
  // `trigger_demo_webhook` (the "Send" button) requires an authenticated session.
  await loginAsAdmin(page);
  const section = page.locator("section", {
    has: page.getByText("Webhook"),
  });
  await expect(section.getByText(key)).toBeVisible({ timeout: ACTION_TIMEOUT });

  // Exercises triggerWebhook() end to end via a real browser click, including
  // the replay-protection timestamp header.
  const errors = trackConsoleErrors(page);
  await section.getByRole("button", { name: "Send" }).click();
  await expect(section.getByText(/Webhook processed/i)).toBeVisible({
    timeout: ACTION_TIMEOUT,
  });
  await expect(section.locator("p.hint.warning")).toHaveCount(0);
  expect(errors).toHaveLength(0);
});

import { test, expect, API_URL, ACTION_TIMEOUT } from "./fixtures";
import { randomUUID } from "crypto";
import type { Page, Request } from "@playwright/test";

// Mirrors examples/with-svelte/demo/frontend/tests/signals.spec.ts but adapted
// for the Dioxus SDK. The Dioxus tracker posts every signal kind to the
// unified `/_api/signal` endpoint with a `{type, payload}` discriminator. Two
// SDK quirks worth noting:
//   - capture_error always wires `stack: None` on the wire; the test bridge
//     folds Error.stack into context.stack so we still assert on it via context
//   - identify() is enqueued as a track event named "identify" with
//     `{user_id, traits}` in properties, posted in the regular event batch
// window.forgeSignals is installed by SignalsBridge in src/signals_bridge.rs.

declare global {
  interface Window {
    forgeSignals: {
      track: (event: string, properties?: Record<string, unknown>) => void;
      identify: (
        userId: string,
        traits?: Record<string, unknown>,
      ) => Promise<void>;
      breadcrumb: (message: string, data?: Record<string, unknown>) => void;
      captureError: (
        err: unknown,
        ctx?: Record<string, unknown>,
      ) => Promise<void>;
      page: () => Promise<void>;
      nextCorrelationId: () => string;
      getSessionId: () => string | null;
    };
  }
}

type ViewPayload = {
  url: string;
  referrer?: string;
  title?: string;
  utm_source?: string;
  utm_medium?: string;
  utm_campaign?: string;
  utm_term?: string;
  utm_content?: string;
  correlation_id?: string;
};

type ClientEvent = {
  event: string;
  properties?: Record<string, unknown>;
  correlation_id?: string;
  timestamp?: string;
};

type EventPayload = {
  events: ClientEvent[];
  context?: { page_url?: string; referrer?: string; session_id?: string };
};

type DiagnosticError = {
  message: string;
  stack?: string;
  context?: Record<string, unknown>;
  correlation_id?: string;
  breadcrumbs?: Array<{
    message: string;
    data?: Record<string, unknown>;
    timestamp?: string;
  }>;
  page_url?: string;
};

type ReportPayload = { errors: DiagnosticError[] };

type SignalEnvelope =
  | { type: "view"; payload: ViewPayload }
  | { type: "event"; payload: EventPayload }
  | { type: "report"; payload: ReportPayload };

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

async function waitForSignal<T extends SignalEnvelope["type"]>(
  page: Page,
  type: T,
  predicate: (
    payload: Extract<SignalEnvelope, { type: T }>["payload"],
  ) => boolean = () => true,
  timeout = ACTION_TIMEOUT * 3,
): Promise<{
  request: Request;
  payload: Extract<SignalEnvelope, { type: T }>["payload"];
}> {
  const request = await page.waitForRequest(
    (req) => {
      if (!req.url().includes("/_api/signal")) return false;
      if (req.method() !== "POST") return false;
      try {
        const body = req.postDataJSON() as SignalEnvelope;
        if (body.type !== type) return false;
        return predicate(
          body.payload as Extract<SignalEnvelope, { type: T }>["payload"],
        );
      } catch {
        return false;
      }
    },
    { timeout },
  );
  const body = request.postDataJSON() as SignalEnvelope;
  return {
    request,
    payload: body.payload as Extract<SignalEnvelope, { type: T }>["payload"],
  };
}

test.describe("signals: client SDK end-to-end", () => {
  test.beforeEach(async ({ page, gotoReady }) => {
    await gotoReady("/");
    await page.waitForFunction(() => !!window.forgeSignals, undefined, {
      timeout: ACTION_TIMEOUT * 2,
    });
  });

  test("view is auto-captured on initial navigation", async ({
    page,
    gotoReady,
  }) => {
    const viewPromise = waitForSignal(page, "view");
    await gotoReady("/?utm_source=playwright&utm_medium=test");
    const { payload } = await viewPromise;
    expect(payload.url).toBeTruthy();
    expect(payload.utm_source).toBe("playwright");
    expect(payload.utm_medium).toBe("test");
  });

  test("event: track() dispatches end-to-end", async ({ page }) => {
    const eventPromise = waitForSignal(page, "event", (p) =>
      p.events.some((e) => e.event === "signals_spec_track"),
    );
    await page.evaluate(() => {
      window.forgeSignals.track("signals_spec_track", {
        number: 42,
        nested: { a: 1, b: [true, false] },
      });
    });
    const { payload } = await eventPromise;
    const evt = payload.events.find((e) => e.event === "signals_spec_track");
    expect(evt).toBeTruthy();
    expect(evt!.properties).toMatchObject({
      number: 42,
      nested: { a: 1, b: [true, false] },
    });
  });

  test("identify: SDK emits an identify event with user_id + traits", async ({
    page,
  }) => {
    const userId = randomUUID();
    const eventPromise = waitForSignal(page, "event", (p) =>
      p.events.some(
        (e) =>
          e.event === "identify" &&
          (e.properties as { user_id?: string } | undefined)?.user_id ===
            userId,
      ),
    );
    await page.evaluate(async (uid) => {
      await window.forgeSignals.identify(uid, {
        plan: "enterprise",
        email: "signals@example.com",
      });
    }, userId);
    const { payload } = await eventPromise;
    const identifyEvent = payload.events.find((e) => e.event === "identify");
    expect(identifyEvent).toBeTruthy();
    expect(identifyEvent!.properties).toMatchObject({
      user_id: userId,
      traits: { plan: "enterprise", email: "signals@example.com" },
    });
  });

  test("report: captureError sends context, breadcrumbs, page_url", async ({
    page,
  }) => {
    // The Dioxus SDK wires stack: None on the wire; the test bridge folds
    // Error.stack into context.stack so we assert via context.
    const marker = `signals-spec-${randomUUID()}`;
    const reportPromise = waitForSignal(page, "report", (p) =>
      p.errors.some((e) => e.message.includes(marker)),
    );
    await page.evaluate(async (m) => {
      window.forgeSignals.breadcrumb("step-one", { step: 1 });
      window.forgeSignals.breadcrumb("step-two", { step: 2 });
      await window.forgeSignals.captureError(new Error(`${m} manual capture`), {
        feature: "tests",
      });
    }, marker);
    const { payload } = await reportPromise;
    const err = payload.errors.find((e) => e.message.includes(marker))!;
    expect(err.context).toMatchObject({ feature: "tests" });
    expect(err.page_url).toBeTruthy();
    const crumbMessages = (err.breadcrumbs ?? []).map((c) => c.message);
    expect(crumbMessages).toEqual(
      expect.arrayContaining(["step-one", "step-two"]),
    );
  });

  test("correlation_id: SDK attaches x-correlation-id on RPC calls", async ({
    page,
  }) => {
    const rpcReq = page.waitForRequest(
      (req) => req.url().includes("/_api/rpc/") && req.method() === "POST",
      { timeout: ACTION_TIMEOUT * 2 },
    );
    await page.getByRole("button", { name: /Fetch Stats/i }).click();
    const req = await rpcReq;
    expect(req.headers()["x-correlation-id"]).toMatch(/\S/);
  });
});

test.describe("signals: unified endpoint contract", () => {
  test("POST /signal {type:view} returns ok + session_id", async ({
    request,
  }) => {
    const res = await request.post(`${API_URL}/_api/signal`, {
      headers: { "Content-Type": "application/json" },
      data: {
        type: "view",
        payload: {
          url: "https://demo.example/landing",
          referrer: "https://external.example",
          title: "Landing",
          utm_source: "newsletter",
          utm_medium: "email",
          utm_campaign: "spring",
          correlation_id: "corr-view-1",
        },
      },
    });
    const body = (await res.json()) as { ok: boolean; session_id?: string };
    expect(res.ok()).toBeTruthy();
    expect(body.ok).toBe(true);
    expect(body.session_id).toMatch(UUID_RE);
  });

  test("POST /signal {type:event} accepts a batch", async ({ request }) => {
    const res = await request.post(`${API_URL}/_api/signal`, {
      headers: { "Content-Type": "application/json" },
      data: {
        type: "event",
        payload: {
          events: [
            {
              event: "purchase",
              properties: { amount: 99.99 },
              correlation_id: "corr-event-1",
            },
            { event: "scroll", properties: { depth: 75 } },
          ],
          context: { page_url: "https://demo.example/checkout" },
        },
      },
    });
    const body = (await res.json()) as { ok: boolean; session_id?: string };
    expect(body.ok).toBe(true);
    expect(body.session_id).toMatch(UUID_RE);
  });

  test("POST /signal {type:report} accepts errors with breadcrumbs", async ({
    request,
  }) => {
    const res = await request.post(`${API_URL}/_api/signal`, {
      headers: { "Content-Type": "application/json" },
      data: {
        type: "report",
        payload: {
          errors: [
            {
              message: "TypeError: x",
              stack: "TypeError: x\n  at foo",
              context: { component: "Checkout" },
              correlation_id: "corr-report-1",
              page_url: "https://demo.example/checkout",
              breadcrumbs: [
                {
                  message: "clicked pay",
                  data: { method: "card" },
                  timestamp: new Date().toISOString(),
                },
              ],
            },
          ],
        },
      },
    });
    const body = (await res.json()) as { ok: boolean };
    expect(body.ok).toBe(true);
  });

  test("POST /signal {type:event} rejects batches over 50", async ({
    request,
  }) => {
    const events = Array.from({ length: 51 }, (_, i) => ({ event: `e_${i}` }));
    const res = await request.post(`${API_URL}/_api/signal`, {
      headers: { "Content-Type": "application/json" },
      data: { type: "event", payload: { events } },
    });
    const body = (await res.json()) as { ok: boolean };
    expect(body.ok).toBe(false);
  });

  test("DNT:1 short-circuits view but report still lands", async ({
    request,
  }) => {
    const viewRes = await request.post(`${API_URL}/_api/signal`, {
      headers: { "Content-Type": "application/json", DNT: "1" },
      data: { type: "view", payload: { url: "/dnt-test" } },
    });
    const viewBody = (await viewRes.json()) as {
      ok: boolean;
      session_id?: string | null;
    };
    expect(viewBody.ok).toBe(true);
    expect(viewBody.session_id).toBeFalsy();

    const reportRes = await request.post(`${API_URL}/_api/signal`, {
      headers: { "Content-Type": "application/json", DNT: "1" },
      data: {
        type: "report",
        payload: { errors: [{ message: "dnt-user-crash" }] },
      },
    });
    const reportBody = (await reportRes.json()) as { ok: boolean };
    expect(reportBody.ok).toBe(true);
  });
});

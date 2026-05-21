
import type { ForgeClient } from "./client.js";

export interface SignalsConfig {
  /** Enable signals collection (default: true) */
  enabled?: boolean;
  /** Auto-track page views on navigation (default: true) */
  autoPageViews?: boolean;
  /** Auto-capture frontend errors (default: true) */
  autoCaptureErrors?: boolean;
  /** Auto-capture Web Vitals (LCP, CLS, INP, FCP, TTFB, navigation) (default: true) */
  autoWebVitals?: boolean;
  /** Auto-capture online/offline transitions (default: true) */
  autoNetworkEvents?: boolean;
  /** Flush interval in ms (default: 5000) */
  flushInterval?: number;
  /** Max events per batch (default: 20) */
  maxBatchSize?: number;
  /** Respect DNT / Sec-GPC headers and disable on opt-out (default: true) */
  respectDnt?: boolean;
  /** Persist the outbound queue to localStorage so events survive reloads (default: true) */
  persistQueue?: boolean;
}

interface SignalEvent {
  event: string;
  properties?: Record<string, unknown>;
  correlation_id?: string;
  timestamp?: string;
}

interface Breadcrumb {
  message: string;
  data?: Record<string, unknown>;
  timestamp: string;
}

const DEFAULT_FLUSH_INTERVAL = 5000;
const DEFAULT_MAX_BATCH = 20;
const MAX_BREADCRUMBS = 20;
const MAX_QUEUE_SIZE = 1000;
const PERSIST_KEY = "forge_signals_queue_v1";

function generateId(): string {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let id = "";
  const bytes = crypto.getRandomValues(new Uint8Array(21));
  for (const byte of bytes) {
    id += chars[byte % chars.length];
  }
  return id;
}

function hasOptedOut(): boolean {
  if (typeof navigator === "undefined") return false;
  const nav = navigator as Navigator & {
    doNotTrack?: string;
    globalPrivacyControl?: boolean;
    msDoNotTrack?: string;
  };
  const win = typeof window !== "undefined"
    ? (window as Window & { doNotTrack?: string })
    : undefined;
  const dnt = nav.doNotTrack ?? win?.doNotTrack ?? nav.msDoNotTrack;
  return dnt === "1" || dnt === "yes" || nav.globalPrivacyControl === true;
}

export class ForgeSignals {
  private queue: SignalEvent[] = [];
  private breadcrumbs: Breadcrumb[] = [];
  private sessionId: string | null = null;
  private lastCorrelationId: string | null = null;
  private lastPageUrl: string | null = null;
  private client: ForgeClient;
  private config: Required<SignalsConfig>;
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private destroyed = false;
  private utmParams: Record<string, string> | null = null;
  private originalPushState: typeof history.pushState | null = null;
  private originalReplaceState: typeof history.replaceState | null = null;
  private boundListeners: Array<[EventTarget, string, EventListener]> = [];

  constructor(client: ForgeClient, config?: SignalsConfig) {
    this.client = client;
    this.config = {
      enabled: config?.enabled ?? true,
      autoPageViews: config?.autoPageViews ?? true,
      autoCaptureErrors: config?.autoCaptureErrors ?? true,
      autoWebVitals: config?.autoWebVitals ?? true,
      autoNetworkEvents: config?.autoNetworkEvents ?? true,
      flushInterval: config?.flushInterval ?? DEFAULT_FLUSH_INTERVAL,
      maxBatchSize: config?.maxBatchSize ?? DEFAULT_MAX_BATCH,
      respectDnt: config?.respectDnt ?? true,
      persistQueue: config?.persistQueue ?? true,
    };

    if (!this.config.enabled) return;
    if (this.config.respectDnt && hasOptedOut()) {
      this.config.enabled = false;
      return;
    }

    this.utmParams = this.extractUtm();
    this.restoreQueue();
    this.startFlushTimer();
    // Defer auto-capture setup to avoid competing with the SSE connection
    // for DB pool connections on cold start.
    setTimeout(() => {
      if (!this.destroyed) this.setupAutoCapture();
    }, 2000);
    this.setupUnloadFlush();
    if (this.config.autoWebVitals) {
      this.setupWebVitals();
    }
    if (this.config.autoNetworkEvents) {
      this.setupNetworkEvents();
    }
  }

  /** Track a custom event. */
  track(event: string, properties?: Record<string, unknown>): void {
    if (!this.config.enabled) return;
    this.enqueue({
      event,
      properties,
      correlation_id: this.lastCorrelationId ?? undefined,
    });
  }

  /** Identify the current user (links anonymous session to user). */
  identify(userId: string, traits?: Record<string, unknown>): void {
    if (!this.config.enabled) return;
    this.enqueue({
      event: "identify",
      properties: { user_id: userId, traits: traits ?? {} },
      correlation_id: this.lastCorrelationId ?? undefined,
    });
  }

  /** Track a page view. Called automatically on navigation when `autoPageViews` is enabled. */
  async page(properties?: Record<string, unknown>): Promise<void> {
    if (!this.config.enabled) return;
    try {
      const payload: Record<string, unknown> = {
        url: location.href,
        referrer: document.referrer || undefined,
        title: document.title || undefined,
        ...this.utmParams,
        ...properties,
      };

      const response = await fetch(`${this.client.getUrl()}/_api/signal`, {
        method: "POST",
        ...this.signalFetchOptions(),
        body: JSON.stringify({ type: "view", payload }),
      });

      const result = await response.json();
      if (result.session_id && !this.sessionId) {
        this.sessionId = result.session_id;
      }
      this.utmParams = null;
    } catch {
      // Silent
    }
  }

  /** Capture a frontend error with optional context. */
  captureError(error: Error | string, context?: Record<string, unknown>): void {
    if (!this.config.enabled) return;
    const message = typeof error === "string" ? error : error.message;
    const stack = typeof error === "string" ? undefined : error.stack;

    this.reportErrors([{
      message,
      stack,
      context,
      correlation_id: this.lastCorrelationId ?? undefined,
      breadcrumbs: [...this.breadcrumbs],
      page_url: typeof location !== "undefined" ? location.href : undefined,
    }]);
  }

  /** Add a breadcrumb for error context. */
  breadcrumb(message: string, data?: Record<string, unknown>): void {
    if (!this.config.enabled) return;
    this.breadcrumbs.push({
      message,
      data,
      timestamp: new Date().toISOString(),
    });
    if (this.breadcrumbs.length > MAX_BREADCRUMBS) {
      this.breadcrumbs.shift();
    }
  }

  nextCorrelationId(): string {
    this.lastCorrelationId = generateId();
    return this.lastCorrelationId;
  }

  getSessionId(): string | null {
    return this.sessionId;
  }

  vital(name: string, value: number, extra?: { rating?: string; attribution?: Record<string, unknown> }): void {
    if (!this.config.enabled) return;
    this.enqueue({
      event: `webvital.${name}`,
      properties: {
        value,
        rating: extra?.rating,
        attribution: extra?.attribution,
        page_url: typeof location !== "undefined" ? location.href : undefined,
      },
      correlation_id: this.lastCorrelationId ?? undefined,
    });
  }

  destroy(): void {
    this.destroyed = true;
    if (this.flushTimer) clearInterval(this.flushTimer);
    this.flushBeacon();
    this.teardownAutoCapture();
  }

  private signalFetchOptions(): { headers: Record<string, string>; credentials: RequestCredentials; keepalive: boolean } {
    return {
      headers: {
        "Content-Type": "application/json",
        "x-forge-platform": "web",
        ...(this.sessionId ? { "x-session-id": this.sessionId } : {}),
      },
      credentials: "include",
      // keepalive lets pending requests survive pagehide / tab close up to the
      // browser's budget (typically 64KB) so events emitted during shutdown
      // still reach the server.
      keepalive: true,
    };
  }

  private enqueue(event: SignalEvent): void {
    event.timestamp = new Date().toISOString();
    this.queue.push(event);
    if (this.queue.length > MAX_QUEUE_SIZE) {
      this.queue.splice(0, this.queue.length - MAX_QUEUE_SIZE);
    }
    if (this.queue.length >= this.config.maxBatchSize) {
      this.flush();
    } else {
      this.persistQueue();
    }
  }

  private async flush(): Promise<void> {
    if (this.queue.length === 0) return;
    const events = this.queue.splice(0, this.config.maxBatchSize);
    try {
      const response = await fetch(`${this.client.getUrl()}/_api/signal`, {
        method: "POST",
        ...this.signalFetchOptions(),
        body: JSON.stringify({
          type: "event",
          payload: {
            events,
            context: {
              page_url: typeof location !== "undefined" ? location.href : undefined,
              session_id: this.sessionId,
            },
          },
        }),
      });
      if (!response.ok) throw new Error(`status ${response.status}`);
      const result = await response.json();
      if (result.session_id && !this.sessionId) {
        this.sessionId = result.session_id;
      }
      this.persistQueue();
    } catch {
      this.queue.unshift(...events);
      if (this.queue.length > MAX_QUEUE_SIZE) {
        this.queue.length = MAX_QUEUE_SIZE;
      }
      this.persistQueue();
    }
  }

  private flushBeacon(): void {
    if (this.queue.length === 0) return;
    const events = this.queue.splice(0);
    const body = JSON.stringify({
      type: "event",
      payload: {
        events,
        context: {
          page_url: typeof location !== "undefined" ? location.href : undefined,
          session_id: this.sessionId,
        },
      },
    });
    try {
      const sent = typeof navigator !== "undefined" && navigator.sendBeacon
        ? navigator.sendBeacon(`${this.client.getUrl()}/_api/signal`, new Blob([body], { type: "application/json" }))
        : false;
      if (!sent) {
        // Fall back to keepalive fetch if beacon isn't available / over quota
        fetch(`${this.client.getUrl()}/_api/signal`, {
          method: "POST",
          ...this.signalFetchOptions(),
          body,
        }).catch(() => {});
      }
      this.persistQueue();
    } catch {
      // Last resort, just drop
    }
  }

  private async reportErrors(errors: Array<Record<string, unknown>>): Promise<void> {
    try {
      await fetch(`${this.client.getUrl()}/_api/signal`, {
        method: "POST",
        ...this.signalFetchOptions(),
        body: JSON.stringify({ type: "report", payload: { errors } }),
      });
    } catch {
      // Silent
    }
  }

  private startFlushTimer(): void {
    this.flushTimer = setInterval(() => {
      if (this.destroyed) return;
      this.flush();
    }, this.config.flushInterval);
  }

  private addEventListener(target: EventTarget, event: string, handler: EventListener): void {
    target.addEventListener(event, handler);
    this.boundListeners.push([target, event, handler]);
  }

  private setupAutoCapture(): void {
    if (typeof window === "undefined") return;

    if (this.config.autoPageViews) {
      this.lastPageUrl = location.href;
      this.page();

      this.originalPushState = history.pushState.bind(history);
      this.originalReplaceState = history.replaceState.bind(history);

      const onNavigation = () => {
        const current = location.href;
        if (current !== this.lastPageUrl) {
          this.lastPageUrl = current;
          this.page();
        }
      };

      history.pushState = (...args: Parameters<typeof history.pushState>) => {
        this.originalPushState!(...args);
        onNavigation();
      };
      history.replaceState = (...args: Parameters<typeof history.replaceState>) => {
        this.originalReplaceState!(...args);
        onNavigation();
      };

      this.addEventListener(window, "popstate", () => onNavigation());
    }

    if (this.config.autoCaptureErrors) {
      this.addEventListener(window, "error", ((e: ErrorEvent) => {
        if (e.error) {
          this.captureError(e.error);
        } else {
          this.captureError(e.message || "Unknown error");
        }
      }) as EventListener);

      this.addEventListener(window, "unhandledrejection", ((e: PromiseRejectionEvent) => {
        const reason = e.reason;
        if (reason instanceof Error) {
          this.captureError(reason);
        } else {
          this.captureError(String(reason || "Unhandled promise rejection"));
        }
      }) as EventListener);
    }
  }

  private teardownAutoCapture(): void {
    if (this.originalPushState) {
      history.pushState = this.originalPushState;
    }
    if (this.originalReplaceState) {
      history.replaceState = this.originalReplaceState;
    }
    for (const [target, event, handler] of this.boundListeners) {
      target.removeEventListener(event, handler);
    }
    this.boundListeners = [];
  }

  private setupUnloadFlush(): void {
    if (typeof document === "undefined") return;

    // visibilitychange fires when tab hides; most reliable unload signal on
    // modern browsers. pagehide fires on actual navigation away. Safari
    // sometimes fires only one; bind both so we never miss a flush.
    this.addEventListener(document, "visibilitychange", () => {
      if (document.visibilityState === "hidden") {
        this.flushBeacon();
      }
    });
    if (typeof window !== "undefined") {
      this.addEventListener(window, "pagehide", () => {
        this.flushBeacon();
      });
    }
  }

  private setupWebVitals(): void {
    if (typeof window === "undefined" || typeof PerformanceObserver === "undefined") return;

    // Best-effort via PerformanceObserver; no external library. Values match
    // Core Web Vitals spec: LCP/INP/FCP/TTFB in ms, CLS unitless.
    const observe = (type: string, cb: (entry: PerformanceEntry) => void) => {
      try {
        const observer = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) cb(entry);
        });
        observer.observe({ type, buffered: true } as PerformanceObserverInit);
      } catch {
        // entry type unsupported in this browser
      }
    };

    let lcpValue = 0;
    observe("largest-contentful-paint", (entry) => {
      const e = entry as PerformanceEntry & { renderTime?: number; loadTime?: number };
      lcpValue = e.renderTime ?? e.loadTime ?? entry.startTime;
    });

    let clsValue = 0;
    observe("layout-shift", (entry) => {
      const e = entry as PerformanceEntry & { value: number; hadRecentInput: boolean };
      if (!e.hadRecentInput) clsValue += e.value;
    });

    observe("event", (entry) => {
      const e = entry as PerformanceEntry & { interactionId?: number; duration: number };
      if (e.interactionId && e.duration > 40) {
        this.vital("inp", e.duration, {
          rating: e.duration < 200 ? "good" : e.duration < 500 ? "needs-improvement" : "poor",
          attribution: { name: entry.name },
        });
      }
    });

    observe("paint", (entry) => {
      if (entry.name === "first-contentful-paint") {
        this.vital("fcp", entry.startTime, {
          rating: entry.startTime < 1800 ? "good" : entry.startTime < 3000 ? "needs-improvement" : "poor",
        });
      }
    });

    observe("longtask", (entry) => {
      this.vital("long_task", entry.duration, {
        attribution: { name: entry.name, startTime: entry.startTime },
      });
    });

    const onLoad = () => {
      try {
        const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
        if (nav) {
          const ttfb = nav.responseStart;
          if (ttfb > 0) {
            this.vital("ttfb", ttfb, {
              rating: ttfb < 800 ? "good" : ttfb < 1800 ? "needs-improvement" : "poor",
            });
          }
          this.vital("navigation", nav.loadEventEnd - nav.startTime, {
            attribution: {
              dom_content_loaded: nav.domContentLoadedEventEnd - nav.startTime,
              dom_interactive: nav.domInteractive - nav.startTime,
              transfer_size: nav.transferSize,
              type: nav.type,
            },
          });
        }
      } catch {
        // ignore
      }
    };
    if (document.readyState === "complete") {
      onLoad();
    } else {
      this.addEventListener(window, "load", onLoad as EventListener);
    }

    this.addEventListener(document, "visibilitychange", () => {
      if (document.visibilityState === "hidden") {
        if (lcpValue > 0) {
          this.vital("lcp", lcpValue, {
            rating: lcpValue < 2500 ? "good" : lcpValue < 4000 ? "needs-improvement" : "poor",
          });
          lcpValue = 0;
        }
        if (clsValue > 0) {
          this.vital("cls", clsValue, {
            rating: clsValue < 0.1 ? "good" : clsValue < 0.25 ? "needs-improvement" : "poor",
          });
          clsValue = 0;
        }
      }
    });
  }

  private setupNetworkEvents(): void {
    if (typeof window === "undefined") return;
    this.addEventListener(window, "online", () => {
      this.track("network.online");
      this.flush();
    });
    this.addEventListener(window, "offline", () => {
      this.track("network.offline");
    });
  }

  private extractUtm(): Record<string, string> | null {
    if (typeof location === "undefined") return null;
    const params = new URLSearchParams(location.search);
    const utm: Record<string, string> = {};
    for (const key of ["utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content"]) {
      const value = params.get(key);
      if (value) utm[key] = value;
    }
    return Object.keys(utm).length > 0 ? utm : null;
  }

  private restoreQueue(): void {
    if (!this.config.persistQueue || typeof localStorage === "undefined") return;
    try {
      const raw = localStorage.getItem(PERSIST_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        this.queue.push(...parsed.slice(0, MAX_QUEUE_SIZE));
      }
    } catch {
      // ignore corrupt storage
    }
  }

  private persistQueue(): void {
    if (!this.config.persistQueue || typeof localStorage === "undefined") return;
    try {
      if (this.queue.length === 0) {
        localStorage.removeItem(PERSIST_KEY);
      } else {
        localStorage.setItem(PERSIST_KEY, JSON.stringify(this.queue));
      }
    } catch {
      // Quota exceeded or storage unavailable (private mode). Silent.
    }
  }
}

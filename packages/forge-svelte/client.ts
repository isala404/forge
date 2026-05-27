
import type { ForgeError, ConnectionState } from "./types.js";

export interface ForgeClientConfig {
  url: string;
  getToken?: () => string | null | Promise<string | null>;
  /** Called on 401; return a new token to retry once, or null/throw to fail with UNAUTHORIZED. */
  refreshToken?: () => Promise<string | null>;
  onAuthError?: (error: ForgeError) => void;
  onMutationError?: (error: ForgeClientError) => void;
  /** Opt-in diagnostic channel for non-fatal events (e.g. reconnect failures). */
  onDebug?: (message: string) => void;
  timeout?: number;
}

interface RpcErrorPayload {
  code: string;
  message: string;
  retry_after_secs?: number;
  details?: Record<string, unknown>;
}

interface RpcResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: RpcErrorPayload;
}

interface SsePayload {
  type: "update" | "error" | "connected" | "gap" | "channel";
  target?: string;
  payload?: unknown;
  code?: string;
  message?: string;
  session_id?: string;
  session_secret?: string;
  channel?: string;
}

export class ForgeClientError extends Error implements ForgeError {
  code: string;
  retryAfterSecs?: number;
  details?: Record<string, unknown>;

  constructor(code: string, message: string, retryAfterSecs?: number, details?: Record<string, unknown>) {
    super(message);
    this.name = "ForgeClientError";
    this.code = code;
    this.retryAfterSecs = retryAfterSecs;
    this.details = details;
  }

  isRateLimited(): boolean { return this.code === "RATE_LIMITED"; }
  isUnauthorized(): boolean { return this.code === "UNAUTHORIZED"; }
  isValidation(): boolean { return this.code === "VALIDATION_ERROR"; }
}

interface SubscriptionMeta {
  functionName: string;
  args: unknown;
  failedAttempts: number;
}

export class ForgeClient {
  private config: ForgeClientConfig;
  private eventSource: EventSource | null = null;
  private connectionState: ConnectionState = "disconnected";
  private sessionId: string | null = null;
  private sessionSecret: string | null = null;
  private connectionListeners = new Set<(state: ConnectionState) => void>();
  private subscriptions = new Map<string, (data: unknown) => void>();
  private subscriptionMeta = new Map<string, SubscriptionMeta>();
  // Job/workflow subscriptions keyed by client_sub_id -> job/workflow id. Like
  // query subscriptions, these must be re-registered on the new SSE session
  // after a reconnect, or their server-side entries stay bound to the abandoned
  // session and progress/status pushes are silently lost.
  private jobMeta = new Map<string, string>();
  private workflowMeta = new Map<string, string>();
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 10;
  private maxSubscriptionRetries = 3;
  private reconnectDelay = 1000;
  private connectionTimeoutMs = 30000;
  private eventListeners: Array<{ event: string; handler: EventListener }> = [];
  private connectionId = 0;
  private hasConnectedBefore = false;
  private connectedTokenHash: string | null = null;
  private reconnectPromise: Promise<void> | null = null;
  private signals: import("./signals.js").ForgeSignals | null = null;

  constructor(config: ForgeClientConfig) {
    this.config = config;
  }

  /** Wire signals for correlation ID injection on RPC calls. */
  setSignals(signals: import("./signals.js").ForgeSignals): void {
    this.signals = signals;
  }

  /**
   * Stable fingerprint of a token used to detect rotation. SHA-256 over the
   * whole token avoids the trap where JWTs from the same signing config
   * share the first ~37 characters (header is constant); a prefix-based
   * hash would miss rotation entirely.
   */
  private async hashToken(token: string | null): Promise<string | null> {
    if (!token) return null;
    if (typeof crypto !== "undefined" && crypto.subtle) {
      const bytes = new TextEncoder().encode(token);
      const digest = await crypto.subtle.digest("SHA-256", bytes);
      const view = new Uint8Array(digest);
      let hex = "";
      for (const b of view) hex += b.toString(16).padStart(2, "0");
      return hex;
    }
    // Fallback for non-secure contexts: take the suffix instead of the
    // prefix, which (unlike the prefix) varies between JWTs with identical
    // headers/payloads in the same signing slot.
    return token.length > 16 ? token.slice(-16) : token;
  }

  getUrl(): string {
    return this.config.url;
  }

  notifyMutationError(error: ForgeClientError): void {
    this.config.onMutationError?.(error);
  }

  getConnectionState(): ConnectionState {
    return this.connectionState;
  }

  onConnectionStateChange(listener: (state: ConnectionState) => void): () => void {
    this.connectionListeners.add(listener);
    return () => this.connectionListeners.delete(listener);
  }

  async connect(): Promise<void> {
    if (this.eventSource?.readyState === EventSource.OPEN) return;

    const currentConnectionId = ++this.connectionId;

    this.setConnectionState("connecting");

    if (!this.hasConnectedBefore) {
      const jitter = Math.random() * 1000;
      await new Promise((r) => setTimeout(r, jitter));
      if (currentConnectionId !== this.connectionId) return;
    }

    // Resolve token to either (a) mint a short-lived single-use SSE ticket
    // (authenticated streams) or (b) connect anonymously. The bearer JWT
    // never appears in the URL — query strings leak into access logs,
    // browser history, and Referer headers.
    const token = await this.getToken();

    if (currentConnectionId !== this.connectionId) return;

    let ticket: string | null = null;
    if (token) {
      try {
        const res = await fetch(`${this.config.url}/_api/events/ticket`, {
          method: "POST",
          headers: { Authorization: `Bearer ${token}` },
          credentials: "include",
        });
        if (res.ok) {
          const body = (await res.json()) as { ticket?: string };
          ticket = body.ticket ?? null;
        }
      } catch {
        // Network failure here is non-fatal; we'll fall through to anonymous
        // connect and let the reconnect loop retry.
      }
      if (currentConnectionId !== this.connectionId) return;
    }

    const params = new URLSearchParams();
    if (ticket) params.set("ticket", ticket);

    const sseUrl = `${this.config.url}/_api/events${params.toString() ? `?${params}` : ""}`;

    return new Promise((resolve) => {
      if (currentConnectionId !== this.connectionId) {
        resolve();
        return;
      }

      try {
        this.eventSource = new EventSource(sseUrl);
      } catch {
        this.setConnectionState("disconnected");
        resolve();
        return;
      }

      let resolved = false;
      const timeoutId = setTimeout(() => {
        if (resolved || currentConnectionId !== this.connectionId) return;
        resolved = true;
        this.eventSource?.close();
        this.setConnectionState("disconnected");
        this.scheduleReconnect();
        resolve();
      }, this.connectionTimeoutMs);

      this.addEventSourceListener("connected", (e) => {
        if (resolved || currentConnectionId !== this.connectionId) return;
        resolved = true;
        clearTimeout(timeoutId);
        const data = JSON.parse((e as MessageEvent).data) as SsePayload;
        this.sessionId = data.session_id ?? null;
        this.sessionSecret = data.session_secret ?? null;
        this.setConnectionState("connected");
        this.reconnectAttempts = 0;
        this.hasConnectedBefore = true;
        // Finish wiring up this session BEFORE resolving connect():
        //   1. Set `connectedTokenHash` synchronously-before-resolve. It was
        //      previously set in a detached `.then`, so a `call()` running right
        //      after a reconnect could observe a stale hash and reconnect AGAIN,
        //      stranding just-restored subscriptions on the abandoned session
        //      (their pushes then land in a channel no client reads).
        //   2. Re-register subscriptions, so callers awaiting connect()/
        //      reconnect() (notably call() after a login) don't fire RPCs before
        //      the subscriptions exist on this session.
        void (async () => {
          try {
            this.connectedTokenHash = await this.hashToken(token);
            await this.reregisterSubscriptions();
          } finally {
            resolve();
          }
        })();
      });

      this.addEventSourceListener("update", (e) => {
        if (currentConnectionId !== this.connectionId) return;
        const data = JSON.parse((e as MessageEvent).data) as SsePayload;
        if (data.target) {
          const callback = this.subscriptions.get(data.target);
          if (callback) callback(data.payload);
        }
      });

      this.addEventSourceListener("error", (e) => {
        if (currentConnectionId !== this.connectionId) return;
        let data: SsePayload | null = null;
        try {
          const rawData = (e as MessageEvent).data;
          data = rawData ? (JSON.parse(rawData) as SsePayload) : null;
        } catch {
          // Malformed error payload from server
        }
        if (data?.target) {
          const callback = this.subscriptions.get(data.target);
          if (callback) {
            callback({ error: { code: data.code, message: data.message } });
          }
        }
      });

      this.addEventSourceListener("gap", (e) => {
        if (currentConnectionId !== this.connectionId) return;
        try {
          const data = JSON.parse((e as MessageEvent).data) as SsePayload;
          if (data.target) {
            const callback = this.subscriptions.get(data.target);
            if (callback) callback(undefined);
          }
        } catch { /* ignore malformed gap */ }
      });

      this.addEventSourceListener("channel", () => {
        // Reserved for pub-sub fan-out; no-op until channel subscriptions are implemented.
      });

      this.eventSource.onerror = () => {
        if (resolved || currentConnectionId !== this.connectionId) return;
        resolved = true;
        clearTimeout(timeoutId);

        // EventSource goes to CLOSED (not CONNECTING) on HTTP 401/403.
        // Treat this as an auth error instead of a retriable network failure.
        const isClosed = this.eventSource?.readyState === EventSource.CLOSED;
        this.setConnectionState("disconnected");

        if (isClosed && token) {
          this.config.onAuthError?.(new ForgeClientError("UNAUTHORIZED", "SSE authentication failed"));
        } else {
          this.scheduleReconnect();
        }
        resolve();
      };
    });
  }

  disconnect(): void {
    this.connectionId++;
    this.removeAllEventSourceListeners();
    this.eventSource?.close();
    this.eventSource = null;
    this.sessionId = null;
    this.sessionSecret = null;
    this.setConnectionState("disconnected");
    this.subscriptions.clear();
    this.subscriptionMeta.clear();
    this.jobMeta.clear();
    this.workflowMeta.clear();
  }

  async reconnect(): Promise<void> {
    // Coalesce concurrent callers onto one reconnection attempt
    if (this.reconnectPromise) return this.reconnectPromise;

    this.reconnectPromise = this.doReconnect();
    try {
      await this.reconnectPromise;
    } finally {
      this.reconnectPromise = null;
    }
  }

  private async doReconnect(): Promise<void> {
    const savedMeta = new Map(this.subscriptionMeta);
    const savedCallbacks = new Map(this.subscriptions);

    this.connectionId++;
    this.removeAllEventSourceListeners();
    this.eventSource?.close();
    this.eventSource = null;
    this.sessionId = null;
    this.sessionSecret = null;
    this.setConnectionState("disconnected");
    this.reconnectAttempts = 0;

    this.subscriptionMeta = savedMeta;
    this.subscriptions = savedCallbacks;

    await this.connect();
  }

  private addEventSourceListener(event: string, handler: EventListener): void {
    this.eventSource?.addEventListener(event, handler);
    this.eventListeners.push({ event, handler });
  }

  private removeAllEventSourceListeners(): void {
    for (const { event, handler } of this.eventListeners) {
      this.eventSource?.removeEventListener(event, handler);
    }
    this.eventListeners = [];
  }

  async call<T>(functionName: string, args: unknown): Promise<T> {
    const token = await this.getToken();

    // If a reconnect is already in flight (e.g. one kicked off right after a
    // login), wait for it to finish restoring subscriptions before issuing the
    // RPC — otherwise the reactive update this call triggers can be pushed to a
    // subscription that hasn't been re-registered on the new session yet.
    if (this.reconnectPromise) await this.reconnectPromise;

    // Token rotated since SSE was established; reconnect so subscriptions
    // pick up the new identity. Await so two simultaneous mutations during
    // rotation don't spawn interleaved reconnects (reconnect() coalesces
    // via reconnectPromise, but the previous fire-and-forget call could
    // still race the in-flight RPC against a session-less state).
    const tokenHash = await this.hashToken(token);
    if (this.sessionId && tokenHash !== this.connectedTokenHash) {
      await this.reconnect();
    }

    let response = await this.sendRpc(functionName, args, token);

    if (response.status === 401 && this.config.refreshToken) {
      const refreshed = await this.tryRefresh();
      if (refreshed) {
        // NOTE: for multipart uploads, the retry will re-stream `args`. If a
        // caller passes a one-shot ReadableStream or a Blob already piped
        // elsewhere, the second send may transmit empty content. Callers
        // that need refresh-safe uploads should pass File/Blob backed by
        // bytes (re-readable) rather than streamed sources.
        response = await this.sendRpc(functionName, args, refreshed);
      }
    }

    if (response.status === 401 || response.status === 403) {
      const err = new ForgeClientError("UNAUTHORIZED", "Authentication failed");
      this.config.onAuthError?.(err);
      throw err;
    }

    const contentType = response.headers.get("content-type");

    if (contentType?.includes("application/octet-stream") || contentType?.includes("application/pdf")) {
      return (await response.blob()) as T;
    }

    const result: RpcResponse<T> = await response.json();
    if (!result.success || result.error) {
      const error = result.error || { code: "UNKNOWN", message: "Unknown error" };
      const clientError = new ForgeClientError(error.code, error.message, error.retry_after_secs, typeof error.details === 'object' && error.details !== null ? error.details as Record<string, unknown> : undefined);
      if (error.code === "UNAUTHORIZED" || error.code === "FORBIDDEN") {
        this.config.onAuthError?.(clientError);
      }
      throw clientError;
    }
    return result.data as T;
  }

  private async sendRpc(
    functionName: string,
    args: unknown,
    token: string | null,
  ): Promise<Response> {
    const hasFiles = this.containsFiles(args);
    const correlationId = this.signals?.nextCorrelationId();

    if (hasFiles) {
      const formData = this.buildFormData(args);
      // X-Forge-CSRF forces a CORS preflight on cross-origin POSTs so the
      // server's CORS allowlist gates cross-site requests despite credentials.
      const headers: Record<string, string> = { "x-forge-platform": "web", "X-Forge-CSRF": "1" };
      if (token) headers["Authorization"] = `Bearer ${token}`;
      if (correlationId) headers["x-correlation-id"] = correlationId;
      return fetch(`${this.config.url}/_api/rpc/${functionName}/upload`, {
        method: "POST",
        headers,
        body: formData,
        credentials: "include",
      });
    }

    const normalizedArgs =
      args && typeof args === "object" && Object.keys(args as object).length === 0
        ? null
        : args;

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      "Accept": "application/vnd.forge.v1+json",
      "x-forge-platform": "web",
      "X-Forge-CSRF": "1",
    };
    if (token) headers["Authorization"] = `Bearer ${token}`;
    if (correlationId) headers["x-correlation-id"] = correlationId;

    return fetch(`${this.config.url}/_api/rpc/${functionName}`, {
      method: "POST",
      headers,
      body: JSON.stringify({ args: normalizedArgs }),
      credentials: "include",
    });
  }

  private refreshInFlight: Promise<string | null> | null = null;

  private async tryRefresh(): Promise<string | null> {
    if (!this.config.refreshToken) return null;
    // Coalesce concurrent callers: one refresh per burst of failures
    if (this.refreshInFlight) return this.refreshInFlight;
    this.refreshInFlight = (async () => {
      try {
        return await this.config.refreshToken!();
      } catch {
        return null;
      } finally {
        this.refreshInFlight = null;
      }
    })();
    return this.refreshInFlight;
  }

  _subscribe(target: string, callback: (data: unknown) => void): () => void {
    this.subscriptions.set(target, callback);
    return () => this.subscriptions.delete(target);
  }

  /**
   * Ensure the SSE session is settled and matches the current auth token before
   * a subscription registers against it. Awaits any in-flight reconnect, then
   * reconnects if the token rotated since the session was established. Without
   * this, a subscription can bind to a session that's mid-reconnect and about to
   * be abandoned — its server-side entry then lives on a dead session and every
   * push is silently dropped. All subscription kinds funnel through here so they
   * always land on the live session.
   */
  private async ensureCurrentSession(): Promise<void> {
    if (this.reconnectPromise) await this.reconnectPromise;
    const token = await this.getToken();
    const hash = await this.hashToken(token);
    if (this.sessionId && hash !== this.connectedTokenHash) {
      await this.reconnect();
    }
  }

  async _registerQuery(subscriptionId: string, functionName: string, args: unknown): Promise<unknown> {
    // Settle the session before registering (needs the live session_id before
    // hitting /_api/subscribe).
    await this.ensureCurrentSession();

    this.subscriptionMeta.set(subscriptionId, { functionName, args, failedAttempts: 0 });

    if (this.sessionId) {
      const response = await this.registerSseSubscription(subscriptionId, functionName, args);
      return response.data;
    }

    return this.call(functionName, args);
  }

  _unregisterQuery(subscriptionId: string): void {
    this.subscriptions.delete(`sub:${subscriptionId}`);
    this.subscriptionMeta.delete(subscriptionId);

    if (this.sessionId) {
      this.unregisterSseSubscription(subscriptionId).catch(() => {});
    }
  }

  private async registerSseSubscription(
    id: string,
    functionName: string,
    args: unknown
  ): Promise<{ success: boolean; data?: unknown; error?: { code: string; message: string } }> {
    const token = await this.getToken();
    const response = await fetch(`${this.config.url}/_api/subscribe`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Forge-CSRF": "1",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({
        session_id: this.sessionId,
        session_secret: this.sessionSecret,
        id,
        function: functionName,
        args: args,
      }),
      credentials: "include",
    });

    const result = await response.json();
    if (!result.success) {
      const code = result.error?.code ?? "SUBSCRIPTION_FAILED";
      const error = new ForgeClientError(code, result.error?.message ?? "Failed to register subscription");
      if (code === "UNAUTHORIZED" || code === "FORBIDDEN") {
        this.config.onAuthError?.(error);
      }
      throw error;
    }
    return result;
  }

  private async unregisterSseSubscription(id: string): Promise<void> {
    const token = await this.getToken();
    await fetch(`${this.config.url}/_api/unsubscribe`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Forge-CSRF": "1",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({
        session_id: this.sessionId,
        session_secret: this.sessionSecret,
        id,
      }),
      credentials: "include",
    });
  }

  // Raw job-subscribe POST against the current session. Used both by the
  // public `_registerJob` (after settling the session) and by
  // `reregisterSubscriptions` during a reconnect — the latter must NOT settle
  // the session (it runs inside the reconnect) or it would deadlock.
  private async registerSseJob(clientSubId: string, jobId: string): Promise<unknown> {
    if (!this.sessionId) return null;
    const token = await this.getToken();
    const res = await fetch(`${this.config.url}/_api/subscribe-job`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Forge-CSRF": "1",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({
        session_id: this.sessionId,
        session_secret: this.sessionSecret,
        id: clientSubId,
        job_id: jobId,
      }),
      credentials: "include",
    });
    return this.parseTrackerResponse(res, "JOB_SUBSCRIBE_FAILED");
  }

  async _registerJob(clientSubId: string, jobId: string): Promise<unknown> {
    // Track for re-registration on reconnect (see `jobMeta`). Recorded even if
    // there's no session yet so a subscription made pre-connect is restored.
    this.jobMeta.set(clientSubId, jobId);
    await this.ensureCurrentSession();
    return this.registerSseJob(clientSubId, jobId);
  }

  // Raw workflow-subscribe POST. See `registerSseJob` for why this is split.
  private async registerSseWorkflow(
    clientSubId: string,
    workflowId: string,
  ): Promise<unknown> {
    if (!this.sessionId) return null;
    const token = await this.getToken();
    const res = await fetch(`${this.config.url}/_api/subscribe-workflow`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Forge-CSRF": "1",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({
        session_id: this.sessionId,
        session_secret: this.sessionSecret,
        id: clientSubId,
        workflow_id: workflowId,
      }),
      credentials: "include",
    });
    return this.parseTrackerResponse(res, "WORKFLOW_SUBSCRIBE_FAILED");
  }

  async _registerWorkflow(clientSubId: string, workflowId: string): Promise<unknown> {
    this.workflowMeta.set(clientSubId, workflowId);
    await this.ensureCurrentSession();
    return this.registerSseWorkflow(clientSubId, workflowId);
  }

  /** Drop a job subscription's local state so it isn't re-registered on reconnect. */
  _unregisterJob(clientSubId: string): void {
    this.jobMeta.delete(clientSubId);
    this.subscriptions.delete(`job:${clientSubId}`);
  }

  /** Drop a workflow subscription's local state so it isn't re-registered on reconnect. */
  _unregisterWorkflow(clientSubId: string): void {
    this.workflowMeta.delete(clientSubId);
    this.subscriptions.delete(`wf:${clientSubId}`);
  }

  /**
   * Common envelope handling for job/workflow subscribe endpoints. Surfaces
   * non-200 responses as ForgeClientError so the store can render a real
   * failure state instead of "loading=false, error=null, data=null".
   */
  private async parseTrackerResponse(res: Response, fallbackCode: string): Promise<unknown> {
    if (res.ok) {
      const json = await res.json();
      if (json && json.success === false) {
        const err = json.error ?? {};
        throw new ForgeClientError(err.code ?? fallbackCode, err.message ?? `Server returned ${res.status}`);
      }
      return json.data ?? null;
    }
    let body: { error?: { code?: string; message?: string } } | null = null;
    try {
      body = await res.json();
    } catch {
      // Non-JSON error body — fall through to status-based message.
    }
    const code = body?.error?.code ?? fallbackCode;
    const message = body?.error?.message ?? `Server returned ${res.status}`;
    throw new ForgeClientError(code, message);
  }

  private async reregisterSubscriptions(): Promise<void> {
    // Snapshot keys; callbacks for failed subs may mutate the map mid-loop
    const subscriptionIds = Array.from(this.subscriptionMeta.keys());

    let first = true;
    for (const id of subscriptionIds) {
      // Stagger re-registrations so a page with 100 subs doesn't issue 100
      // POSTs in <1s after a reconnect. Skip the very first to keep latency
      // low for single-sub pages.
      if (!first) {
        await new Promise((r) => setTimeout(r, 50 + Math.random() * 100));
      }
      first = false;

      const meta = this.subscriptionMeta.get(id);
      if (!meta) continue;

      if (meta.failedAttempts >= this.maxSubscriptionRetries) {
        this.subscriptionMeta.delete(id);
        const callback = this.subscriptions.get(`sub:${id}`);
        if (callback) {
          callback({
            error: {
              code: "MAX_RETRIES_EXCEEDED",
              message: `Subscription failed after ${this.maxSubscriptionRetries} attempts`,
            },
            reconnecting: false,
          });
        }
        continue;
      }

      try {
        await this.registerSseSubscription(id, meta.functionName, meta.args);
        meta.failedAttempts = 0;
      } catch (err) {
        meta.failedAttempts++;
        // Routed through onDebug so production builds don't leak subscription
        // IDs into the browser console.
        this.config.onDebug?.(`Failed to re-register subscription ${id} (attempt ${meta.failedAttempts}): ${err instanceof Error ? err.message : String(err)}`);
        const callback = this.subscriptions.get(`sub:${id}`);
        if (callback) {
          callback({
            error: {
              code: "REREGISTRATION_FAILED",
              message: `Failed to re-register: ${err instanceof Error ? err.message : String(err)}`,
            },
            reconnecting: meta.failedAttempts < this.maxSubscriptionRetries,
          });
        }
      }
    }

    // Re-register job/workflow subscriptions on the new session. Their server
    // entries are bound to the prior (now-closed) session, so without this their
    // progress/status pushes land on a channel no client reads. Skip any whose
    // store has already torn down (callback removed) and forward the fresh
    // snapshot so the store catches up to any state missed during the gap.
    for (const [clientSubId, jobId] of Array.from(this.jobMeta)) {
      if (!this.subscriptions.has(`job:${clientSubId}`)) {
        this.jobMeta.delete(clientSubId);
        continue;
      }
      try {
        // Raw register — we're already inside the reconnect, so don't funnel
        // through `_registerJob` (which would await this same reconnect).
        const data = await this.registerSseJob(clientSubId, jobId);
        if (data) this.subscriptions.get(`job:${clientSubId}`)?.(data);
      } catch (err) {
        this.config.onDebug?.(
          `Failed to re-register job ${clientSubId}: ${err instanceof Error ? err.message : String(err)}`,
        );
      }
    }
    for (const [clientSubId, workflowId] of Array.from(this.workflowMeta)) {
      if (!this.subscriptions.has(`wf:${clientSubId}`)) {
        this.workflowMeta.delete(clientSubId);
        continue;
      }
      try {
        // Raw register — see the job loop above.
        const data = await this.registerSseWorkflow(clientSubId, workflowId);
        if (data) this.subscriptions.get(`wf:${clientSubId}`)?.(data);
      } catch (err) {
        this.config.onDebug?.(
          `Failed to re-register workflow ${clientSubId}: ${err instanceof Error ? err.message : String(err)}`,
        );
      }
    }
  }

  private containsFiles(obj: unknown, seen: WeakSet<object> = new WeakSet()): boolean {
    if (obj instanceof File || obj instanceof Blob) return true;
    if (obj && typeof obj === "object") {
      if (seen.has(obj)) return false;
      seen.add(obj);
      // Date has no nested user data; skip to avoid scanning its prototype chain.
      if (obj instanceof Date) return false;
      if (obj instanceof Map) {
        for (const v of obj.values()) {
          if (this.containsFiles(v, seen)) return true;
        }
        return false;
      }
      if (obj instanceof Set) {
        for (const v of obj.values()) {
          if (this.containsFiles(v, seen)) return true;
        }
        return false;
      }
      if (Array.isArray(obj)) return obj.some((item) => this.containsFiles(item, seen));
      return Object.values(obj).some((value) => this.containsFiles(value, seen));
    }
    return false;
  }

  private buildFormData(args: unknown): FormData {
    const formData = new FormData();
    const jsonArgs: Record<string, unknown> = {};

    if (args && typeof args === "object") {
      for (const [key, value] of Object.entries(args as Record<string, unknown>)) {
        if (value instanceof File) {
          formData.append(key, value, value.name);
        } else if (value instanceof Blob) {
          formData.append(key, value, "blob");
        } else {
          jsonArgs[key] = value;
        }
      }
    }

    try {
      formData.append("_json", JSON.stringify(jsonArgs));
    } catch (e) {
      throw new ForgeClientError(
        "SERIALIZATION_ERROR",
        `Failed to serialize arguments: ${e instanceof Error ? e.message : String(e)}`
      );
    }
    return formData;
  }

  private async getToken(): Promise<string | null> {
    const token = this.config.getToken?.() ?? null;
    return token instanceof Promise ? await token : token;
  }

  private setConnectionState(state: ConnectionState): void {
    this.connectionState = state;
    for (const listener of this.connectionListeners) {
      try {
        listener(state);
      } catch (err) {
        // One misbehaving listener must not abort the rest. Surface via
        // opt-in debug channel rather than the console.
        this.config.onDebug?.(`connection listener threw: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  }

  private scheduleReconnect(): void {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) return;

    // Exponential backoff with full jitter: multiplier is [0,1) so retries
    // are uniformly spread across the window rather than biased upward.
    const exponentialDelay = this.reconnectDelay * Math.pow(2, this.reconnectAttempts);
    const delay = Math.min(exponentialDelay * Math.random(), 30000);
    this.reconnectAttempts++;

    setTimeout(() => {
      if (this.connectionState === "disconnected") {
        this.connect();
      }
    }, delay);
  }
}

export function createForgeClient(config: ForgeClientConfig): ForgeClient {
  return new ForgeClient(config);
}


import type { ForgeError, ConnectionState } from "./types.js";

export interface ForgeClientConfig {
  url: string;
  getToken?: () => string | null | Promise<string | null>;
  /** Called on 401; return a new token to retry once, or null/throw to fail with UNAUTHORIZED. */
  refreshToken?: () => Promise<string | null>;
  onAuthError?: (error: ForgeError) => void;
  onMutationError?: (error: ForgeClientError) => void;
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
  last_event_id?: string;
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

  private hashToken(token: string | null): string | null {
    return token ? token.substring(0, 20) : null;
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

    // Token must be resolved before EventSource is created (sent as query param)
    const token = await this.getToken();

    if (currentConnectionId !== this.connectionId) return;

    const params = new URLSearchParams();
    if (token) params.set("token", token);

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
        this.connectedTokenHash = this.hashToken(token);
        this.setConnectionState("connected");
        this.reconnectAttempts = 0;
        this.hasConnectedBefore = true;
        this.reregisterSubscriptions();

        resolve();
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

    // Token rotated since SSE was established; reconnect so subscriptions
    // pick up the new identity without waiting for a new _registerQuery call.
    const tokenHash = this.hashToken(token);
    if (this.sessionId && tokenHash !== this.connectedTokenHash) {
      this.reconnect();
    }

    let response = await this.sendRpc(functionName, args, token);

    if (response.status === 401 && this.config.refreshToken) {
      const refreshed = await this.tryRefresh();
      if (refreshed) {
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
      const headers: Record<string, string> = { "x-forge-platform": "web" };
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

  async _registerQuery(subscriptionId: string, functionName: string, args: unknown): Promise<unknown> {
    // Must await here (unlike the fire-and-forget in call()) because
    // registration needs the new session_id before hitting /_api/subscribe.
    const currentToken = await this.getToken();
    const currentHash = this.hashToken(currentToken);
    if (this.sessionId && currentHash !== this.connectedTokenHash) {
      await this.reconnect();
    }

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

  async _registerJob(clientSubId: string, jobId: string): Promise<unknown> {
    if (!this.sessionId) return null;
    const token = await this.getToken();
    const res = await fetch(`${this.config.url}/_api/subscribe-job`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
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
    if (res.ok) {
      const json = await res.json();
      return json.data ?? null;
    }
    return null;
  }

  async _registerWorkflow(clientSubId: string, workflowId: string): Promise<unknown> {
    if (!this.sessionId) return null;
    const token = await this.getToken();
    const res = await fetch(`${this.config.url}/_api/subscribe-workflow`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
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
    if (res.ok) {
      const json = await res.json();
      return json.data ?? null;
    }
    return null;
  }

  private async reregisterSubscriptions(): Promise<void> {
    // Snapshot keys; callbacks for failed subs may mutate the map mid-loop
    const subscriptionIds = Array.from(this.subscriptionMeta.keys());

    for (const id of subscriptionIds) {
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
        console.error(`Failed to re-register subscription ${id} (attempt ${meta.failedAttempts}):`, err);
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
  }

  private containsFiles(obj: unknown): boolean {
    if (obj instanceof File || obj instanceof Blob) return true;
    if (Array.isArray(obj)) return obj.some((item) => this.containsFiles(item));
    if (obj && typeof obj === "object") {
      return Object.values(obj).some((value) => this.containsFiles(value));
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
    this.connectionListeners.forEach((listener) => listener(state));
  }

  private scheduleReconnect(): void {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) return;

    // Exponential backoff with jitter to prevent synchronized retry storms
    const exponentialDelay = this.reconnectDelay * Math.pow(2, this.reconnectAttempts);
    const jitter = 0.5 + Math.random();
    const delay = Math.min(exponentialDelay * jitter, 30000);
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

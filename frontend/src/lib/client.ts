import type { ForgeError, ConnectionState, ForgeClientInterface } from './types.js';

/**
 * Client configuration options.
 */
export interface ForgeClientConfig {
  url: string;
  getToken?: () => string | null | Promise<string | null>;
  onAuthError?: (error: ForgeError) => void;
  timeout?: number;
  retries?: number;
}

/**
 * RPC request structure.
 */
interface RpcRequest {
  function: string;
  args: unknown;
  requestId?: string;
}

/**
 * RPC response structure.
 */
interface RpcResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: ForgeError;
  requestId?: string;
}

/**
 * WebSocket message types.
 */
interface WsMessage {
  type: 'subscribe' | 'unsubscribe' | 'data' | 'delta' | 'response' | 'error';
  id?: string;
  subscriptionId?: string;
  requestId?: string;
  function?: string;
  args?: unknown;
  data?: unknown;
  success?: boolean;
  error?: ForgeError;
}

/**
 * FORGE client error class.
 */
export class ForgeClientError extends Error {
  code: string;
  details?: Record<string, unknown>;

  constructor(code: string, message: string, details?: Record<string, unknown>) {
    super(message);
    this.name = 'ForgeClientError';
    this.code = code;
    this.details = details;
  }
}

/**
 * Main FORGE client for communicating with the backend.
 */
export class ForgeClient implements ForgeClientInterface {
  private config: Required<Pick<ForgeClientConfig, 'url' | 'timeout' | 'retries'>> &
                  Pick<ForgeClientConfig, 'getToken' | 'onAuthError'>;
  private ws: WebSocket | null = null;
  private connectionState: ConnectionState = 'disconnected';
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 10;
  private reconnectDelay = 1000;
  private subscriptions = new Map<string, (data: unknown) => void>();
  private pendingRequests = new Map<string, {
    resolve: (value: unknown) => void;
    reject: (error: Error) => void;
  }>();
  private connectionListeners = new Set<(state: ConnectionState) => void>();

  constructor(config: ForgeClientConfig) {
    this.config = {
      timeout: 30000,
      retries: 3,
      ...config,
    };
  }

  /**
   * Get the current connection state.
   */
  getConnectionState(): ConnectionState {
    return this.connectionState;
  }

  /**
   * Add a connection state listener.
   */
  onConnectionStateChange(listener: (state: ConnectionState) => void): () => void {
    this.connectionListeners.add(listener);
    return () => this.connectionListeners.delete(listener);
  }

  /**
   * Connect to the WebSocket server.
   */
  async connect(): Promise<void> {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      return;
    }

    return new Promise((resolve, reject) => {
      const wsUrl = this.config.url.replace(/^http/, 'ws') + '/ws';
      this.ws = new WebSocket(wsUrl);
      this.setConnectionState('connecting');

      this.ws.onopen = async () => {
        // Authenticate if we have a token
        const token = await this.getToken();
        if (token) {
          this.ws?.send(JSON.stringify({ type: 'auth', token }));
        }
        this.setConnectionState('connected');
        this.reconnectAttempts = 0;
        resolve();
      };

      this.ws.onerror = () => {
        reject(new Error('WebSocket connection failed'));
      };

      this.ws.onclose = () => {
        this.setConnectionState('disconnected');
        this.handleDisconnect();
      };

      this.ws.onmessage = (event) => {
        this.handleMessage(event.data);
      };
    });
  }

  /**
   * Disconnect from the server.
   */
  disconnect(): void {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this.setConnectionState('disconnected');
    this.subscriptions.clear();
    this.pendingRequests.clear();
  }

  /**
   * Call a remote function via HTTP RPC.
   */
  async call<T>(functionName: string, args: unknown): Promise<T> {
    const token = await this.getToken();

    const response = await fetch(`${this.config.url}/rpc/${functionName}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { 'Authorization': `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({ args }),
    });

    const result: RpcResponse<T> = await response.json();

    if (!result.success || result.error) {
      const error = result.error || { code: 'UNKNOWN', message: 'Unknown error' };
      if (error.code === 'UNAUTHORIZED' && this.config.onAuthError) {
        this.config.onAuthError(error);
      }
      throw new ForgeClientError(error.code, error.message, error.details);
    }

    return result.data as T;
  }

  /**
   * Subscribe to a query for real-time updates.
   */
  subscribe<T>(
    functionName: string,
    args: unknown,
    callback: (data: T) => void
  ): () => void {
    const subscriptionId = this.generateId();

    // Store the callback
    this.subscriptions.set(subscriptionId, callback as (data: unknown) => void);

    // Send subscription request if connected
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({
        type: 'subscribe',
        id: subscriptionId,
        function: functionName,
        args,
      }));
    }

    // Return unsubscribe function
    return () => {
      this.subscriptions.delete(subscriptionId);
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify({
          type: 'unsubscribe',
          id: subscriptionId,
        }));
      }
    };
  }

  /**
   * Get the auth token.
   */
  private async getToken(): Promise<string | null> {
    if (!this.config.getToken) {
      return null;
    }
    return this.config.getToken();
  }

  /**
   * Set connection state and notify listeners.
   */
  private setConnectionState(state: ConnectionState): void {
    this.connectionState = state;
    this.connectionListeners.forEach(listener => listener(state));
  }

  /**
   * Handle WebSocket messages.
   */
  private handleMessage(data: string): void {
    try {
      const message: WsMessage = JSON.parse(data);

      switch (message.type) {
        case 'data':
        case 'delta': {
          const callback = this.subscriptions.get(message.subscriptionId!);
          if (callback) {
            callback(message.data);
          }
          break;
        }
        case 'response': {
          const pending = this.pendingRequests.get(message.requestId!);
          if (pending) {
            if (message.success) {
              pending.resolve(message.data);
            } else {
              pending.reject(new ForgeClientError(
                message.error?.code || 'UNKNOWN',
                message.error?.message || 'Unknown error',
                message.error?.details
              ));
            }
            this.pendingRequests.delete(message.requestId!);
          }
          break;
        }
        case 'error': {
          console.error('FORGE error:', message.error);
          break;
        }
      }
    } catch (e) {
      console.error('Failed to parse WebSocket message:', e);
    }
  }

  /**
   * Handle disconnection with reconnection logic.
   */
  private handleDisconnect(): void {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      return;
    }

    this.setConnectionState('reconnecting');
    this.reconnectAttempts++;

    const delay = Math.min(
      this.reconnectDelay * Math.pow(2, this.reconnectAttempts - 1),
      30000
    );

    setTimeout(() => {
      this.connect().catch(() => {
        // Will retry on next disconnect
      });
    }, delay);
  }

  /**
   * Generate a unique ID.
   */
  private generateId(): string {
    return Math.random().toString(36).substring(2, 15);
  }
}

/**
 * Create a new FORGE client instance.
 */
export function createForgeClient(config: ForgeClientConfig): ForgeClient {
  return new ForgeClient(config);
}

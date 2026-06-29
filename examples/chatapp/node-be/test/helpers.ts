import { WebSocket } from "ws";
import { createClient, type Client } from "graphql-ws";

export interface GqlResult<T> {
  data?: T;
  errors?: Array<{ message: string; extensions?: { code?: string } }>;
}

export class HttpClient {
  constructor(
    private readonly url: string,
    private token?: string,
  ) {}

  withToken(token: string): HttpClient {
    return new HttpClient(this.url, token);
  }

  async gql<T = Record<string, unknown>>(
    query: string,
    variables: Record<string, unknown> = {},
  ): Promise<GqlResult<T>> {
    const headers: Record<string, string> = { "content-type": "application/json" };
    if (this.token) headers.authorization = `Bearer ${this.token}`;
    const res = await fetch(this.url, {
      method: "POST",
      headers,
      body: JSON.stringify({ query, variables }),
    });
    return (await res.json()) as GqlResult<T>;
  }

  async ok<T = Record<string, unknown>>(
    query: string,
    variables: Record<string, unknown> = {},
  ): Promise<T> {
    const r = await this.gql<T>(query, variables);
    if (r.errors?.length) {
      throw new Error(`GraphQL errors: ${JSON.stringify(r.errors)}`);
    }
    if (!r.data) throw new Error("no data");
    return r.data;
  }
}

export function wsClient(wsUrl: string, token?: string): Client {
  return createClient({
    url: wsUrl,
    webSocketImpl: WebSocket,
    connectionParams: token ? { authorization: `Bearer ${token}` } : {},
    lazy: false,
  });
}

// Subscribe and collect into a pushable queue so a test can await events one at a time.
export function subscribe<T>(
  client: Client,
  query: string,
  variables: Record<string, unknown> = {},
): { next: () => Promise<T>; dispose: () => void } {
  const queue: T[] = [];
  const waiters: Array<(v: T) => void> = [];
  let error: unknown = null;

  const push = (v: T): void => {
    const w = waiters.shift();
    if (w) w(v);
    else queue.push(v);
  };

  const unsubscribe = client.subscribe<T>(
    { query, variables },
    {
      next: (msg) => {
        if (msg.data) push(msg.data as T);
      },
      error: (e) => {
        error = e;
      },
      complete: () => {},
    },
  );

  return {
    next: (): Promise<T> => {
      if (error) return Promise.reject(error);
      const v = queue.shift();
      if (v !== undefined) return Promise.resolve(v);
      return new Promise<T>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("subscription timeout")), 10_000);
        waiters.push((val) => {
          clearTimeout(timer);
          resolve(val);
        });
      });
    },
    dispose: unsubscribe,
  };
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

let counter = 0;
export function uniqueName(prefix: string): string {
  counter += 1;
  return `${prefix}_${Date.now().toString(36)}_${counter}`;
}

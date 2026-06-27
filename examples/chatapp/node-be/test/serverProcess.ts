import { spawn, type ChildProcess } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER_ENTRY = join(HERE, "..", "src", "server.ts");

export interface ServerHandle {
  port: number;
  close(): Promise<void>;
}

// Boot the real server in a child Node process (native TS stripping) so the test
// talks to it only over HTTP/WS: no shared module graph, no dual-graphql realm.
export function startServerProcess(env: Record<string, string>): Promise<ServerHandle> {
  const child: ChildProcess = spawn(process.execPath, [SERVER_ENTRY], {
    env: { ...process.env, ...env, PORT: "0" },
    stdio: ["ignore", "pipe", "inherit"],
  });

  return new Promise<ServerHandle>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("server did not become ready in time")), 30_000);
    let buffer = "";
    child.stdout!.on("data", (chunk: Buffer) => {
      buffer += chunk.toString("utf8");
      const m = /READY (\d+)/.exec(buffer);
      if (m) {
        clearTimeout(timer);
        const port = parseInt(m[1]!, 10);
        resolve({
          port,
          close: () =>
            new Promise<void>((res) => {
              child.once("exit", () => res());
              child.kill("SIGTERM");
              setTimeout(() => {
                child.kill("SIGKILL");
                res();
              }, 3000);
            }),
        });
      }
    });
    child.once("error", reject);
    child.once("exit", (code) => {
      if (code !== 0 && code !== null) reject(new Error(`server exited early with code ${code}`));
    });
  });
}

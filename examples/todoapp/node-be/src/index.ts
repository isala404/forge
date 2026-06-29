import { serve } from "@hono/node-server";
import { ForgeClient } from "forgelib";

import { api, type Bindings } from "./routes.ts";
import { envOr } from "./utils.ts";

async function start(): Promise<void> {
  const forge = await ForgeClient.init(); // reads ./forge.toml
  const bindings = { forge } satisfies Bindings;

  setInterval(() => {
    forge.runSchedulerOnce().catch((err) => console.warn("scheduler tick failed:", err));
    forge.maintain().catch((err) => console.warn("maintenance sweep failed:", err));
  }, 30_000).unref();

  const port = parseInt(envOr("PORT", "9082"), 10);
  const host = envOr("BIND", "127.0.0.1");
  serve({ fetch: (request) => api.fetch(request, bindings), hostname: host, port }, () => {
    console.error(`todoapp-node-be listening on http://${host}:${port}`);
  });
}

start().catch((err) => {
  console.error(err);
  process.exit(1);
});

import { serve } from "@hono/node-server";
import { ForgeClient } from "forgelib";

import { api, type Bindings } from "./routes.ts";
import { runClicksWorker, runExpireWorker, runSchedulerLoop } from "./worker.ts";
import { envOr } from "./utils.ts";

async function start(): Promise<void> {
  const forge = await ForgeClient.init(); // reads ./forge.toml
  const bindings = { forge } satisfies Bindings;

  runClicksWorker(forge);
  runExpireWorker(forge);
  runSchedulerLoop(forge);

  const port = parseInt(envOr("PORT", "9092"), 10);
  const host = envOr("BIND", "127.0.0.1");
  serve({ fetch: (request) => api.fetch(request, bindings), hostname: host, port }, () => {
    console.error(`linksapp-node-be listening on http://${host}:${port}`);
  });
}

start().catch((err) => {
  console.error(err);
  process.exit(1);
});

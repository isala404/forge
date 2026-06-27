import { serve } from "@hono/node-server";
import { ForgeClient } from "forge-node";

import { api, type Bindings } from "./routes.ts";
import { runClicksWorker, runExpireWorker, runSchedulerLoop } from "./worker.ts";
import { envOr } from "./utils.ts";

async function start(): Promise<void> {
  process.env.FORGE_POSTGRES_URL ||= "postgres://postgres:forge@127.0.0.1:5432/linksapp_node";
  process.env.FORGE_BLOB_SIGNING_SECRET ||= "dev-secret-change-me";

  const forge = await ForgeClient.connectFromEnv();
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

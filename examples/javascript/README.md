# Forge — JavaScript example

A guided tour of every Forge primitive from Node.js, through the
[`forge-node`](../../bindings/forge-node) napi-rs binding. Mirrors
[`examples/full_tour.rs`](../full_tour.rs).

## Run

```sh
# 1. Start Postgres (from the repo root)
docker compose up -d db

# 2. Build the native binding
cd bindings/forge-node
npm install
npm run build           # or: npm run build:debug

# 3. Run the example
cd ../../examples/javascript
npm install
FORGE_POSTGRES_URL=postgres://postgres:forge@localhost:5432/forge_dev node full_tour.js
```

The binding exposes a representative slice of each primitive as camelCase async
methods (`configSet`, `flag`, `rateLimitCheck`, `blobPut`, `hashPassword`,
`createSession`, `createApiKey`, `scheduleAt`, `runSchedulerOnce`, …). See
[`bindings/forge-node/src/lib.rs`](../../bindings/forge-node/src/lib.rs) for the full
surface.

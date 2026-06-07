# forge-node

Node.js bindings for [Forge](../..) via [napi-rs](https://napi.rs). Exposes the
`kv` and `queue` primitives to JavaScript/TypeScript as a native addon.

## Build

The platform binary (`*.node`) is not committed — build it from source:

```sh
npm install          # installs @napi-rs/cli
npm run build:debug  # or `npm run build` for a release binary
```

This produces `forge-node.<platform>.node` next to the committed `index.js` /
`index.d.ts` (the generated JS entry + TypeScript types).

## Use

```js
import { ForgeClient } from 'forge-node';

const forge = await ForgeClient.connect('postgres://localhost/myapp');

// kv (Redis lineage)
await forge.kvSet('user:42:name', 'Ada', /* ttlSeconds */ null, /* ifNotExists */ false);
await forge.kvIncr('clicks:42', 1);
const name = await forge.kvGet('user:42:name');

// queue (SQS lineage)
const id = await forge.queueEnqueue('emails', JSON.stringify({ to: 'a@b.c' }));
const job = await forge.queueDequeue('emails', /* visibilitySeconds */ 30, /* waitSeconds */ 1);
if (job) {
  // ... process job.payload ...
  await forge.queueAck(job.id);   // or forge.queueNack(job.id)
}
```

Async Rust methods become JS `Promise`s; method names are camelCased. Leased
jobs are held Rust-side and referenced by `id`, so the opaque lease fence never
crosses into JS. See `index.d.ts` for the full typed surface.

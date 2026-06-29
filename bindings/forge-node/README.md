# forge-node

Node.js bindings for [Forge](../..) via [napi-rs](https://napi.rs). A native addon
exposing the full primitive surface (kv, queue, config, ratelimit, blob, auth,
schedule, pubsub) plus `backendReport`. Async Rust methods become JS `Promise`s;
method names are camelCased.

## Build

The platform binary (`*.node`) is not committed; build it from source:

```sh
npm install          # installs @napi-rs/cli
npm run build:debug  # or `npm run build` for a release binary
```

This produces `forge-node.<platform>.node` next to the committed `index.js` /
`index.d.ts` (the generated JS entry + TypeScript types).

## Use

Configuration lives in a `forge.toml` at the project root; `init()` reads it and
instantiates the runtime. A minimal one:

```toml
[postgres]
url = "${DATABASE_URL:-postgres://localhost/myapp}"
```

```js
import { ForgeClient } from 'forgelib';

const forge = await ForgeClient.init(); // reads ./forge.toml

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

Leased jobs are held Rust-side and referenced by `id`, so the opaque lease fence never
crosses into JS. Every per-deployment knob (namespace, pool size, blob backend, …) lives
in `forge.toml`; `ForgeClient.initFrom(path)` loads a file outside the current directory.
See `index.d.ts` for the full typed surface.

### Typed projection

`forge-node/typed` binds a name + JSON codec to a type, so you enqueue a typed payload
instead of a raw queue string + `JSON.stringify` (the Node view of the Rust
`forgelib::typed` layer):

```ts
import { ForgeClient } from 'forgelib';
import { typedQueue, forgeErrorCode } from 'forgelib/typed';

interface SendEmail { to: string; template: string }
const emails = typedQueue<SendEmail>(forge, 'emails');
await emails.enqueue({ to: 'a@b.c', template: 'welcome' }, { maxAttempts: 3 });
const job = await emails.dequeue({ waitSeconds: 1 });
if (job) { handle(job.payload); await emails.ack(job.id); }

// forgeErrorCode(e) -> 'INVALID' | 'LIMIT' | ... parses the code Forge prefixes
// onto the thrown error's message.
```

Per-primitive semantics live in [`docs/contracts/`](../../docs/contracts/) and recipes in
[`docs/cookbook/`](../../docs/cookbook/).

# forge-py

Python bindings for Forge via [pyo3](https://pyo3.rs). A **natively async**
`ForgeClient` over the async Rust core: every method returns an awaitable driven on a
shared Tokio runtime, so an asyncio app `await`s the binding directly — no thread-pool
wrapper, the event loop is never blocked. The full primitive surface is exposed (kv,
queue, config, ratelimit, blob, auth, schedule, pubsub), plus `backend_report`.

Forge errors surface as a typed exception hierarchy (`forge_py.NotFound`,
`forge_py.Invalid`, `forge_py.Limit`, … all subclasses of `forge_py.ForgeError`).

Built against the stable ABI (`abi3-py39`), so one wheel runs on CPython ≥ 3.9.

## Build

```sh
docker compose up -d db          # a Postgres for local use

cd bindings/forge-py
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop                  # compiles + installs `forge_py` into the venv
```

(On a CPython newer than this pyo3 release knows about, prefix the build with
`PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1`.)

## Use

```python
import forge_py

forge = await forge_py.ForgeClient.connect(
    "postgres://postgres:forge@localhost:5432/forge_dev",
    "signing-secret",   # optional; enables presigned blob URLs
)
await forge.kv_set("greeting", "hi")
print(await forge.kv_get("greeting"))

# Realtime: a Subscription is an async iterator yielding bytes.
async for payload in await forge.pubsub_subscribe("chat:1"):
    handle(payload)
```

### Typed projection

`forge_py.typed` binds a name + JSON codec to a type, so you enqueue a model instead of
a raw queue string + `json.dumps` (the Python view of the Rust `forge::typed` layer):

```python
from forge_py.typed import TypedQueue, TypedKvKey

emails = TypedQueue(forge, "emails")
await emails.enqueue({"to": "a@b.c", "template": "welcome"}, max_attempts=3)
job = await emails.dequeue(wait_seconds=1.0)
if job:
    handle(job.payload)            # already decoded
    await emails.ack(job.id)

profile = TypedKvKey(forge, "user:1:profile")
await profile.set({"name": "Ada"})
```

The full method surface is in [`src/lib.rs`](src/lib.rs); per-primitive semantics live in
[`docs/contracts/`](../../docs/contracts/) and recipes in [`docs/cookbook/`](../../docs/cookbook/).

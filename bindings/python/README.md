# forgelib

Python bindings for Forge via [pyo3](https://pyo3.rs). A **natively async**
`ForgeClient` over the async Rust core: every method returns an awaitable driven on a
shared Tokio runtime, so an asyncio app `await`s the binding directly: no thread-pool
wrapper, the event loop is never blocked. The full primitive surface is exposed (kv,
queue, config, ratelimit, blob, auth, schedule, pubsub), plus `backend_report`.

Forge errors surface as a typed exception hierarchy (`forgelib.NotFoundError`,
`forgelib.InvalidError`, `forgelib.LimitError`, … all subclasses of
`forgelib.ForgeError`), and every raised instance carries a `retryable` attribute.

Built against the stable ABI (`abi3-py39`), so one wheel runs on CPython ≥ 3.9.

## Build

```sh
docker compose up -d db          # a Postgres for local use

cd bindings/python
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop                  # compiles + installs `forgelib` into the venv
```

(On a CPython newer than this pyo3 release knows about, prefix the build with
`PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1`.)

## Use

Configuration lives in a `forge.toml` at the project root; `init()` reads it and
instantiates the runtime. A minimal one:

```toml
[postgres]
url = "${DATABASE_URL:-postgres://postgres:forge@localhost:5432/forge_dev}"

[blob]
signing_secret = "${BLOB_SIGNING_SECRET:-dev-secret}"  # enables presigned blob URLs
```

```python
import forgelib

forge = await forgelib.ForgeClient.init()  # reads ./forge.toml

greeting = forge.kv("greeting")
await greeting.set("hi")
print(await greeting.get())

# Realtime: topic handles decode JSON events for you.
async for event in forge.topic("chat:1").subscribe():
    handle(event)
```

### Native typed handles

The main `forgelib` package binds names to JSON value types directly on the client,
so most app code does not need raw queue strings plus `json.dumps`:

```python
import forgelib

emails = forge.queue("emails")
await emails.enqueue({"to": "a@b.c", "template": "welcome"}, max_attempts=3)
job = await emails.dequeue(wait_seconds=1.0)
if job:
    handle(job.payload)            # already decoded
    await emails.ack(job.receipt)

profile = forge.kv("user:1:profile")
await profile.set({"name": "Ada"})
```

The raw methods (`kv_set`, `queue_enqueue`, ...) remain available for exact
cross-language parity and string/byte contracts. The packaged stubs include both the
raw client and the typed handles.

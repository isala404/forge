# forgelib

Python bindings for Forge via [pyo3](https://pyo3.rs). A **natively async** `ForgeClient` over the async Rust core: every method returns an awaitable driven on a shared Tokio runtime, so an asyncio app `await`s the binding directly: no thread-pool wrapper, the event loop is never blocked. The full primitive surface is exposed (kv, queue, config, ratelimit, blob, auth, schedule, pubsub), plus capability inspection, readiness probes, per-instance metrics, and W3C queue trace propagation.

Scheduler methods expose UTC schedule inspection, pause/resume, due lag/count, last successful tick, enqueue failures, and explicit bounded misfire policies. The scheduler enqueues ordinary deterministic queue jobs and does not own workflow state.

The top-level `encode_cloud_event`/`decode_cloud_event` and `import_env_config`/`export_env_config` helpers are state-free. They add no transport, framework, or environment mutation to the native client; see the repository's integration recipes for HTTP frameworks, deployment, and MCP patterns.

Forge errors surface as a typed exception hierarchy (`forgelib.NotFoundError`, `forgelib.InvalidError`, `forgelib.LimitError`, … all subclasses of `forgelib.ForgeError`), and every raised instance carries a `retryable` attribute.

Built against the stable ABI (`abi3-py39`), so one wheel runs on CPython ≥ 3.9. The `1.x` release publishes wheels for Linux x64, Linux arm64, macOS arm64, and Windows x64, plus a source distribution. Intel macOS installs from source.

## Build

```sh
docker compose up -d db          # a Postgres for local use

cd bindings/python
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop                  # compiles + installs `forgelib` into the venv
```

(On a CPython newer than this pyo3 release knows about, prefix the build with `PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1`.)

## Use

Configuration lives in a `forge.toml` at the project root; `init()` reads it and instantiates the runtime. A minimal one:

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

The main `forgelib` package binds names to JSON value types directly on the client, so most app code does not need raw queue strings plus `json.dumps`:

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

The raw methods (`kv_set`, `queue_enqueue`, ...) remain available for exact cross-language parity and string/byte contracts. The packaged stubs include both the raw client and the typed handles.

## Deterministic tests

`await ForgeClient.init_memory_for_testing(toml, start_ms, seed)` creates the normal memory profile with manual time and repeatable test-only entropy. Call `forge.advance_test_clock(seconds)` to drive expiry, delayed work, scheduling, and rate-limit refill without sleeping. The seeded tokens are predictable and must never leave tests.

Application-owned names can use the v1 helper: `forgelib.scope_kv_key("billing", tenant_id, user_id, invoice_id)`. `parse_scoped_name` reverses the length-prefixed encoding. This is a naming aid only; authorize every component before calling Forge.

## OpenFeature, bulk reads, and snapshots

Install `forgelib[openfeature]` and use `forgelib.openfeature.ForgeProvider` with the official OpenFeature async client. The provider preserves stable variants and reasons, installs no global hooks, and exposes the official OpenTelemetry tracing hook for application-scoped registration. Startup code can use `config_get_many` and `flag_details_many` for ordered 256-item reads. `config_snapshot` captures only requested keys and pre-evaluated flags for 1–86,400 seconds; access it through `config_snapshot_get` and `config_snapshot_flag_details`, and protect `application_protected` snapshots before they leave the trusted server boundary.

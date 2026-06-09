# forge-py

Python bindings for Forge via [pyo3](https://pyo3.rs). A synchronous `ForgeClient`
wraps the async Rust core (driving it on an embedded Tokio runtime), exposing a
representative slice of every primitive — kv, queue, config, ratelimit, blob, auth,
and schedule.

Built against the stable ABI (`abi3-py39`), so one wheel runs on CPython ≥ 3.9.

## Build

```sh
docker compose up -d db          # from the repo root

cd bindings/forge-py
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop                  # compiles + installs `forge_py` into the venv
```

(On a CPython newer than this pyo3 release knows about, prefix the build with
`PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1`.)

## Use

```python
from forge_py import ForgeClient

forge = ForgeClient.connect(
    "postgres://postgres:forge@localhost:5432/forge_dev",
    "signing-secret",   # optional; enables presigned blob URLs
)
forge.kv_set("greeting", "hi")
print(forge.kv_get("greeting"))
```

See [`examples/python/full_tour.py`](../../examples/python/full_tour.py) for a tour of
every primitive. The full method surface is in [`src/lib.rs`](src/lib.rs).

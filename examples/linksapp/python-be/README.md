# linksapp: Python/FastAPI backend

FastAPI backend for the linksapp URL shortener. Runs on port **9093**. Forge KV is the only data store; with no configuration the Forge database is embedded.

## Run

```sh
uv run uvicorn app.main:app --port 9093 --reload   # no database needed: boots an embedded Postgres
```

Forge configures itself from `forge.toml` in this directory: with no configuration it boots an embedded Postgres (data persists in `.forge/pg`), and a set `FORGE_POSTGRES_URL` (interpolated by that file) wins when you'd rather use your own server:

```sh
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_python \
  uv run uvicorn app.main:app --port 9093 --reload
```

The clicks worker publishes bounded invalidation hints over Forge pub/sub. The application-owned SSE route forwards the hint. Consumers refetch `/api/links/{slug}/state`; notification payloads never become authoritative state.

The scheduler loop fires one-shot expiry work every 30 seconds and reports remaining due count/lag before maintenance. Forge stores canonical times in UTC and keeps workflow state outside the scheduler.

The direct custom-slug flag keeps this example small. A production OpenFeature integration can install `forgelib[openfeature]`, register `forgelib.openfeature.ForgeProvider` with the official async SDK, and attach `telemetry_hook()` at client scope. Use `config_get_many`/`flag_details_many` for startup reads and only use `config_snapshot` with an explicit expiry and secret declaration.

## Dev

```sh
uv sync
uv run ruff check .
```

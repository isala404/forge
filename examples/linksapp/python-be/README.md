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

## Dev

```sh
uv sync
uv run ruff check .
```

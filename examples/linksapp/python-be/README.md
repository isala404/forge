# linksapp: Python/FastAPI backend

FastAPI backend for the linksapp URL shortener. Runs on port **9093** and uses
`linksapp_python` as its Forge database. Forge KV is the only data store.

## Run

```sh
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_python \
  uv run uvicorn app.main:app --port 9093 --reload
```

Forge configures itself from `forge.toml` in this directory. `FORGE_POSTGRES_URL` is read
from the environment by that file (`${FORGE_POSTGRES_URL:-...}`), so the command above still
works; it is no longer passed to Forge directly.

## Dev

```sh
uv sync
uv run ruff check .
```

# todoapp Python backend

A FastAPI REST API using `forgelib`.

```sh
uv sync
createdb -h 127.0.0.1 -U postgres todoapp_python
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_python uv run uvicorn app.main:app --port 9083
```

Forge configures itself from `forge.toml` in this directory. `FORGE_POSTGRES_URL` is read
from the environment by that file (`${FORGE_POSTGRES_URL:-...}`), so the command above still
works; it is no longer passed to Forge directly.

Default port: `9083`.

Read `app/routes.py` first for the FastAPI handlers, then `app/main.py` for startup. Forge
calls stay inline in the route handlers; `app/types.py` is only Pydantic shapes, and
`app/utils.py` is only small validation/key helpers.

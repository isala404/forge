# todoapp Python backend

A FastAPI REST API using `forgelib`.

```sh
uv sync
uv run uvicorn app.main:app --port 9083    # no database needed: boots an embedded Postgres
```

Forge configures itself from `forge.toml` in this directory: with no configuration it
boots an embedded Postgres (data persists in `.forge/pg`), and `FORGE_POSTGRES_URL` —
interpolated by that file — wins when you'd rather use your own server:

```sh
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_python uv run uvicorn app.main:app --port 9083
```

Default port: `9083`.

Read `app/routes.py` first for the FastAPI handlers, then `app/main.py` for startup. Forge
calls stay inline in the route handlers; `app/types.py` is only Pydantic shapes, and
`app/utils.py` is only small validation/key helpers.

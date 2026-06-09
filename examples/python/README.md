# Forge — Python example

A guided tour of every Forge primitive from Python, through the
[`forge-py`](../../bindings/forge-py) pyo3 binding. Mirrors
[`examples/full_tour.rs`](../full_tour.rs).

## Run

```sh
# 1. Start Postgres (from the repo root)
docker compose up -d db

# 2. Build + install the binding into a venv
cd bindings/forge-py
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop

# 3. Run the example (same venv)
cd ../../examples/python
FORGE_POSTGRES_URL=postgres://postgres:forge@localhost:5432/forge_dev python full_tour.py
```

The binding exposes a representative slice of each primitive as synchronous methods
(`config_set`, `flag`, `rate_limit_check`, `blob_put`, `hash_password`,
`create_session`, `create_api_key`, `schedule_at`, `run_scheduler_once`, …).

# Forge examples

The same guided tour of **every** primitive — auth (password + session + API key),
config + feature flags, rate limiting, blob storage with a presigned URL, and a
one-shot scheduled job fired into the queue — in three languages, all against one
Postgres connection.

| Language | Example | Via |
| --- | --- | --- |
| Rust | [`full_tour.rs`](full_tour.rs) | the crate directly (`cargo run --example full_tour`) |
| JavaScript | [`javascript/full_tour.js`](javascript/full_tour.js) | the [`forge-node`](../bindings/forge-node) napi binding |
| Python | [`python/full_tour.py`](python/full_tour.py) | the [`forge-py`](../bindings/forge-py) pyo3 binding |

There is also [`webhook_processor.rs`](webhook_processor.rs) — a focused kv + queue
dogfood (idempotency gate, INCR counters, a managed worker, dead-lettering).

All examples expect Postgres from the repo's `docker compose up -d db` and honor
`FORGE_POSTGRES_URL`. The JS and Python ones need their native binding built first —
see each language's README.

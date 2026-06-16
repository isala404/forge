#!/usr/bin/env bash
# Build both bindings and run all three conformance runners against TEST_DATABASE_URL.
# Each runner exits non-zero iff its observed failure set differs from known_gaps.json.
set -euo pipefail

if [[ -z "${TEST_DATABASE_URL:-}" ]]; then
  echo "TEST_DATABASE_URL is not set" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== rust =="
cargo test --features pg-tests --test conformance -- --nocapture

echo "== node: build binding + run =="
(cd bindings/forge-node && npm install --no-audit --no-fund --silent && ./node_modules/.bin/napi build --platform --release)
(cd conformance/node && npm install --no-audit --no-fund --silent)
node conformance/node/run.js

echo "== python: build wheel + run =="
uvx maturin build -i python3 --out bindings/forge-py/dist --quiet
uv venv conformance/python/.venv --quiet
uv pip install --python conformance/python/.venv --quiet \
  bindings/forge-py/dist/*.whl 'psycopg[binary]'
conformance/python/.venv/bin/python conformance/python/run.py

echo "== conformance: all runners green =="

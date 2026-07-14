#!/usr/bin/env bash
# Build both bindings and run all three conformance runners against TEST_DATABASE_URL.
# Each runner exits non-zero iff its observed failure set differs from known_gaps.json.
set -euo pipefail

if [[ -z "${TEST_DATABASE_URL:-}" ]]; then
  echo "TEST_DATABASE_URL is not set" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

echo "== rust =="
cargo test --features pg-tests,conformance --test conformance -- --nocapture

echo "== node: build binding + run =="
(cd bindings/node && npm ci --ignore-scripts --no-audit --no-fund --silent && ./node_modules/.bin/napi build --platform --release)
(cd tools/conformance/node && npm ci --ignore-scripts --no-audit --no-fund --silent)
node tools/conformance/node/run.js

echo "== python: build wheel + run =="
rm -f bindings/python/dist/*.whl
# maturin must run from the binding crate so it detects the pyo3 bindings (the repo
# root Cargo.toml is the Rust lib, not a Python extension).
(cd bindings/python && uvx --from 'maturin>=1.5,<2' maturin build -i python3 --out dist --quiet)
uv venv tools/conformance/python/.venv --clear --quiet
uv pip install --python tools/conformance/python/.venv --quiet \
  bindings/python/dist/*.whl 'psycopg[binary]'
tools/conformance/python/.venv/bin/python tools/conformance/python/run.py

echo "== conformance: all runners green =="

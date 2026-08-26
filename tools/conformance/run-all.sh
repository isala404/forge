#!/usr/bin/env bash
# Build the language packages and run all four conformance runners.
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

echo "== go: memory + PostgreSQL contract =="
(cd bindings/go && go test -run '^TestConformance(Memory|Postgres)$' ./...)

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
python_wheels=(bindings/python/dist/*.whl)
if (( ${#python_wheels[@]} != 1 )); then
  echo "expected exactly one Python wheel, found ${#python_wheels[@]}" >&2
  exit 2
fi
uv pip install --python tools/conformance/python/.venv --quiet \
  "${python_wheels[0]}[openfeature]" 'psycopg[binary]'
tools/conformance/python/.venv/bin/python tools/conformance/python/run.py

echo "== conformance: all runners green =="

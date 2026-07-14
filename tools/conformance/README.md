# Forge conformance suite

One declarative scenario matrix, run by a native runner in each language (Rust, Node, Python) against the same throwaway-Postgres setup. It exists to make the cross-language contract executable: if Node and Python ever drift on units, shapes, defaults, or error codes, a scenario goes red instead of slipping through review.

This is the anti-regression backbone for the cross-language contract. Scenarios encode the target unified contract: Rust's shape is the reference. Where a binding does not yet conform, the gap is registered in `known_gaps.json` so CI stays honest: it asserts that *exactly* the registered gaps fail. Fixing a gap (without removing it from the registry) fails CI ("known gap now passes, remove it"), and a brand-new divergence fails CI as an unexpected red. Both directions are caught.

## Layout

The scenario matrix ships with the crate (so external backend authors can run it against their own `forgelib::Forge`); the language runners live under `tools/`.

```
src/conformance/
  mod.rs               # the scenario interpreter (forgelib::conformance), shipped with the crate
  scenarios/*.json     # the matrix, one file per primitive (source of truth)
  known_gaps.json      # (primitive, scenario, language) pairs expected to fail today

tools/conformance/
  README.md            # this file, schema + how to run
  run-all.sh           # builds both bindings and runs all three runners
  node/                # Node runner (forgelib binding)
  python/              # Python runner (forgelib binding)

tests/conformance.rs   # the Rust runner: drives forgelib::conformance against throwaway DBs
```

The scenario JSON in `src/conformance/scenarios/` is the normative, executable form of the cross-language contract; the error taxonomy it asserts is defined in `src/error.rs`, and <https://tryforge.dev> renders the same facts for humans.

## Scenario schema

A scenario file:

```json
{
  "primitive": "kv",
  "scenarios": [ { "name": "...", "steps": [ ... ] } ]
}
```

A scenario is an ordered list of **steps** run against one fresh Forge in one namespace (state persists across steps within a scenario; each scenario gets a clean database). A step:

```json
{
  "op": "kv.set",
  "args": { "key": "greeting", "value": "hello", "ttl_seconds": 60 },
  "as": "w",
  "expect": { "value": true }
}
```

- **`op`**: canonical `"<primitive>.<method>"`. Each runner maps this to the binding's actual method name (`kv.set` → `kvSet` in Node, `kv_set` in Python).
- **`args`**: named arguments in **canonical units**:
  - durations → **seconds** (`ttl_seconds`, `per_seconds`, `visibility_seconds`, `wait_seconds`, `expires_seconds`, `retry_seconds`, `idle_seconds`).
  - absolute timestamps → **epoch milliseconds** (`when_epoch_ms`). This is the P1-6 decision: milliseconds everywhere.
  - binary values → `{ "$bytes": [0, 255, 0, 254] }` (array of byte ints, so no runner needs a base64 dependency).
  - reference to an earlier step's result → `{ "$ref": "w.id" }` (the step must carry `"as": "w"`; dotted path indexes into the result shape).
- **`as`**: optional name to capture this step's result for later `$ref`.
- **`namespace`**: optional. Runs this step against a Forge bound to the named `kv_namespace` (the runner keeps one Forge per namespace within the scenario, all on the same database). Used by the isolation scenarios to prove two apps sharing a database cannot see each other's state. Absent = the default namespace.
- **`expect`**: optional (absent = run for side effects only). Exactly one of:
  - `value`: expected scalar / `null` / bool / number / string.
  - `bytes`: `{ "$bytes": [ ... ] }` for a binary return.
  - `shape`: object of expected fields for a struct return (dequeue job, queue depth, rate-limit decision, blob info, …). Runners normalize each language's native return (Node object, Python tuple/class) into canonical **snake_case** keys before comparing, so a missing/renamed field fails.
  - `error`: canonical code string, one of `Config | Unavailable | NotFound | Precondition | Limit | Invalid | Backend`. Runners map each language's surface (Node `"INVALID: ..."` prefix, Python exception class) to the canonical spelling. When P1-5 lands this also asserts the `retryable` flag via `"retryable": true|false` alongside `error`.

### Matchers (for non-deterministic results)

Inside `value`/`shape`, a field may be a matcher object instead of a literal:

- `{ "$type": "string" }`: any string (job ids, tokens, presigned URLs).
- `{ "$type": "number" }`: any number.
- `{ "$approx": 1699999999000, "tol": 2000 }`: number within `tol` (timestamps).
- `{ "$regex": "^https?://" }`: string matching a pattern.

## Canonical units & conventions (the contract being enforced)

- Durations: **seconds** (floating allowed). Absolute timestamps: **epoch ms**.
- Struct returns compared by canonical **snake_case** field set: extra or missing fields fail. (So Node's missing `reset_after`, or Python's positional tuples, surface as shape failures once those gaps are fixed.)
- Errors compared by canonical **code** (and, post-P1-5, `retryable`).
- Binary round-trips must be **lossless** (no `from_utf8_lossy`).

## Running

Each runner needs a Postgres it can create throwaway databases against, via `TEST_DATABASE_URL` (same variable the Rust suite already uses, never `DATABASE_URL`). Bring one up with the chatapp stack's Postgres, or any local Postgres, then:

```
# Rust
TEST_DATABASE_URL=postgres://… cargo test --features pg-tests,conformance --test conformance

# Node   (after `cd bindings/node && napi build --platform --release`
#         and `cd tools/conformance/node && npm install`)
TEST_DATABASE_URL=postgres://… node tools/conformance/node/run.js

# Python (after `uvx maturin build -i python3 --out bindings/python/dist`
#         and `uv venv tools/conformance/python/.venv && uv pip install --python
#         tools/conformance/python/.venv bindings/python/dist/*.whl 'psycopg[binary]'`)
TEST_DATABASE_URL=postgres://… tools/conformance/python/.venv/bin/python tools/conformance/python/run.py
```

`tools/conformance/run-all.sh` builds both bindings and runs all three runners in sequence; it is what CI invokes.

CI runs all three and compares the failure set against `known_gaps.json`.

## Adding a fix

1. If a binding diverges today, its scenario is already red and listed in `known_gaps.json` with the owning task (e.g. `"task": "P0-4"`).
2. Land the fix in the binding/core.
3. Remove the now-passing entry from `known_gaps.json`. CI flips green.

Never delete or weaken a scenario to make CI pass. That is the failure mode this suite exists to prevent.

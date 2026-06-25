# Error contract (cross-language)

One taxonomy, three surfaces. The variant set is small and every variant tells the
caller what to *do*. Retryability is part of the contract, per variant. This table
is normative: the bindings and the conformance suite
(`src/conformance/scenarios/*.json`) enforce it.

| Canonical code | Rust (`ForgeError`) | Node (thrown `Error`) | Python (exception) | Retryable | Meaning / what to do |
|---|---|---|---|---|---|
| `Config`       | `Config(String)`      | `"CONFIG: …"`       | `forge_py.Config`       | no  | Misconfiguration (bad DSN, missing migration, malformed option). Only at `connect`/`init`. Fix the config. |
| `Unavailable`  | `Unavailable(String)` | `"UNAVAILABLE: …"`  | `forge_py.Unavailable`  | **yes** | Transient backend outage (pool checkout timeout, dropped connection, PG `08xxx`/`57014`/`57P03`). Retry with backoff. |
| `NotFound`     | `NotFound`            | `"NOT_FOUND: …"`    | `forge_py.NotFound`     | no  | The requested entity does not exist. |
| `Precondition` | `Precondition(String)`| `"PRECONDITION: …"` | `forge_py.Precondition` | no  | CAS mismatch, lease/fence lost, duplicate `dedup_id`, lost receipt. Re-read state and decide. |
| `Limit`        | `Limit(String)`       | `"LIMIT: …"`        | `forge_py.Limit`        | no  | A size/length/quota limit was exceeded. |
| `Invalid`      | `Invalid(String)`     | `"INVALID: …"`      | `forge_py.Invalid`      | no  | Caller bug: invalid argument, malformed key, out-of-range option. |
| `Backend`      | `Backend { retryable, … }` | `"BACKEND: …"` | `forge_py.Backend`      | per-error | A backend/SDK error not covered above. Carries its own `retryable` flag. |

## Reading the code per language

- **Rust:** match on the `ForgeError` variant; call `err.is_retryable()`.
- **Node:** the binding prefixes the canonical code (UPPER_SNAKE) onto the thrown
  error's message; `forgeErrorCode(err)` from `forge-node/typed` returns it
  (`"INVALID"`, `"NOT_FOUND"`, …). Map to the canonical spelling via the table.
- **Python:** the binding raises a typed exception hierarchy under a base
  `forge_py.ForgeError`; the class name *is* the canonical code, and
  `forge_py.typed.forge_error_code(exc)` returns it.

The spellings differ by surface (Rust/Python use `Invalid`; Node uses `INVALID`)
but map 1:1 to the canonical code above. The conformance runners normalize each
surface to the canonical code before comparing, so a binding that maps a failure
onto the wrong variant fails the suite.

## Retryability

Only `Unavailable` (always) and `Backend` (per its flag) are retryable. Everything
else is a caller-side or terminal condition that retrying as-is cannot fix —
re-read state, fix the input, or fix the config. Agent-generated retry logic should
branch on the canonical code, not on message text.

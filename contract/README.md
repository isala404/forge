# Forge contract

`forge.json` is the single machine-readable contract for Forge's supported operations, method mappings, DTOs, defaults, units, limits, errors, backend capabilities, and lifecycle. `schema-v1.json` versions its file format.

Implementations stay handwritten and idiomatic. Run `python3 tools/contract/generate.py` after changing the contract; CI runs the same tool with `--check` and rejects stale declarations, inventories, or reference tables.

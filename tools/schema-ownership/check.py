#!/usr/bin/env python3
"""Verify the schema ownership manifest against canonical migration SQL."""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "contract" / "schema-ownership.json"


def names(kind: str, sql: str) -> set[str]:
    pattern = rf"\bCREATE\s+{kind}(?:\s+IF\s+NOT\s+EXISTS)?\s+([a-z][a-z0-9_]*)"
    return set(re.findall(pattern, sql, flags=re.IGNORECASE))


def main() -> int:
    sql = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted((ROOT / "src" / "migrations").glob("*.sql"))
    )
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    expected = manifest["objects"]
    actual = {
        "tables": names("TABLE", sql),
        "indexes": names("INDEX", sql),
        "functions": names("FUNCTION", sql),
        "triggers": names("TRIGGER", sql),
    }
    problems: list[str] = []
    for kind, found in actual.items():
        declared = set(expected[kind])
        if found != declared:
            problems.append(
                f"{kind}: missing={sorted(found - declared)} stale={sorted(declared - found)}"
            )
    if problems:
        print("schema-ownership: manifest drift")
        for problem in problems:
            print(f"  {problem}")
        return 1
    print("schema-ownership: canonical SQL matches the ownership manifest")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Check the canonical contract for breaking changes after the 1.1 reset."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]


def read_contract(source: str) -> dict[str, Any] | None:
    path = Path(source)
    if path.exists():
        return json.loads(path.read_text(encoding="utf-8"))
    completed = subprocess.run(["git", "show", f"{source}:contract/forge.json"], cwd=ROOT, check=False, capture_output=True, text=True)
    if completed.returncode != 0:
        return None
    return json.loads(completed.stdout)


def index(items: list[dict[str, Any]], key: str) -> dict[str, dict[str, Any]]:
    return {item[key]: item for item in items}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", required=True)
    args = parser.parse_args()
    current = read_contract(str(ROOT / "contract" / "forge.json"))
    assert current is not None
    baseline = read_contract(args.baseline)
    if baseline is None:
        if current["compatibility"]["reset_from"] in args.baseline:
            print(f"api-compat: {args.baseline} predates the declared 1.1 reset")
            return 0
        print(f"api-compat: {args.baseline} has no contract/forge.json", file=sys.stderr)
        return 1

    problems: list[str] = []
    current_errors = index(current["errors"], "code")
    for code, old in index(baseline["errors"], "code").items():
        new = current_errors.get(code)
        if new is None:
            problems.append(f"removed error code {code}")
        elif old["retryable"] != new["retryable"]:
            problems.append(f"changed retryability of {code}")

    current_dtos = index(current["dtos"], "name")
    for name, old in index(baseline["dtos"], "name").items():
        new = current_dtos.get(name)
        if new is None:
            problems.append(f"removed DTO {name}")
            continue
        old_fields = {field["name"]: field["type"] for field in old["fields"]}
        new_fields = {field["name"]: field["type"] for field in new["fields"]}
        for field, old_type in old_fields.items():
            if field not in new_fields:
                problems.append(f"removed field {name}.{field}")
            elif new_fields[field] != old_type:
                problems.append(f"changed field type {name}.{field}: {old_type} -> {new_fields[field]}")

    current_operations = index(current["operations"], "id")
    for operation_id, old in index(baseline["operations"], "id").items():
        new = current_operations.get(operation_id)
        if new is None:
            problems.append(f"removed operation {operation_id}")
            continue
        if old["arguments"] != new["arguments"] or old["result"] != new["result"]:
            problems.append(f"changed signature of {operation_id}")
        for language, old_methods in old["methods"].items():
            missing = set(old_methods) - set(new["methods"].get(language, []))
            if missing:
                problems.append(f"removed {language} methods from {operation_id}: {sorted(missing)}")

    if problems:
        print("api-compat: breaking changes detected:", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print(f"api-compat: {current['contract_version']} is compatible with {baseline['contract_version']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

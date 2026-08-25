#!/usr/bin/env python3
"""Validate a Forge benchmark report against its declared regression budgets."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


REQUIRED_BACKENDS = {"memory", "postgres", "filesystem", "s3"}
REQUIRED_LANGUAGES = {"rust", "node", "bun", "python", "go"}


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def validate_budgets(budgets: dict[str, Any]) -> None:
    if budgets.get("schema_version") != 1:
        raise ValueError("unsupported performance budget schema")
    backends = budgets.get("backends", {})
    languages = budgets.get("language_boundaries", {})
    if set(backends) != REQUIRED_BACKENDS:
        raise ValueError(f"backend budgets must be exactly {sorted(REQUIRED_BACKENDS)}")
    if set(languages) != REQUIRED_LANGUAGES:
        raise ValueError(f"language budgets must be exactly {sorted(REQUIRED_LANGUAGES)}")
    for group in (backends, languages):
        for owner, metrics in group.items():
            if not metrics:
                raise ValueError(f"{owner} has no performance budgets")
            for name, budget in metrics.items():
                maximum = budget.get("max") if isinstance(budget, dict) else None
                if not isinstance(maximum, (int, float)) or maximum <= 0:
                    raise ValueError(f"{owner}.{name}.max must be positive")


def check_report(budgets: dict[str, Any], report: dict[str, Any]) -> list[str]:
    if report.get("schema_version") != 1:
        raise ValueError("unsupported benchmark report schema")
    kind = report.get("kind", "backend")
    owner_group = "language_boundaries" if kind == "language_boundary" else "backends"
    owner = report.get("language") if kind == "language_boundary" else report.get("backend")
    declared = budgets[owner_group].get(owner)
    if declared is None:
        raise ValueError(f"no budgets declared for {kind} {owner!r}")
    failures: list[str] = []
    seen: set[str] = set()
    for metric in report.get("metrics", []):
        name = metric.get("name")
        value = metric.get("value")
        if name not in declared or not isinstance(value, (int, float)):
            continue
        seen.add(name)
        maximum = declared[name]["max"]
        if value > maximum:
            failures.append(f"{owner}.{name}: {value:.4f} exceeds {maximum:.4f}")
    missing = set(declared) - seen
    if missing:
        failures.append(f"{owner}: report is missing budgeted metrics {sorted(missing)}")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", nargs="?")
    parser.add_argument("--budgets", default="benchmarks/budgets.json")
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args()
    budgets = load(Path(args.budgets))
    validate_budgets(budgets)
    if args.validate_only:
        print("performance budgets are valid")
        return 0
    if not args.report:
        parser.error("report is required unless --validate-only is used")
    failures = check_report(budgets, load(Path(args.report)))
    if failures:
        print("\n".join(failures))
        return 1
    print("performance report is within budget")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

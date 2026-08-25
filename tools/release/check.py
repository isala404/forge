#!/usr/bin/env python3
"""Fail a coordinated release when its versioned inputs disagree."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def toml_version(path: str) -> str:
    text = (ROOT / path).read_text()
    match = re.search(r'^version = "([^"]+)"$', text, re.MULTILINE)
    if match is None:
        raise ValueError(f"{path}: missing top-level version")
    return match.group(1)


def check(version: str) -> list[str]:
    errors: list[str] = []
    versions = {
        "Cargo.toml": toml_version("Cargo.toml"),
        "bindings/node/Cargo.toml": toml_version("bindings/node/Cargo.toml"),
        "bindings/node/package.json": json.loads(
            (ROOT / "bindings/node/package.json").read_text()
        )["version"],
        "bindings/node/package-lock.json": json.loads(
            (ROOT / "bindings/node/package-lock.json").read_text()
        )["version"],
        "bindings/python/Cargo.toml": toml_version("bindings/python/Cargo.toml"),
        "bindings/python/pyproject.toml": toml_version(
            "bindings/python/pyproject.toml"
        ),
        "contract/forge.json": json.loads(
            (ROOT / "contract/forge.json").read_text()
        )["contract_version"],
    }
    for path, found in versions.items():
        if found != version:
            errors.append(f"{path}: expected {version}, found {found}")

    changelog = (ROOT / "CHANGELOG.md").read_text()
    if f"## [{version}]" not in changelog:
        errors.append(f"CHANGELOG.md: missing release heading for {version}")

    generated_reference = (
        ROOT / "docs/src/content/docs/contract-reference-generated.mdx"
    ).read_text()
    if f"Contract version: `{version}`" not in generated_reference:
        errors.append("generated contract documentation has the wrong version")

    scenarios = sorted((ROOT / "src/conformance/scenarios").glob("*.json"))
    if not scenarios:
        errors.append("no conformance fixtures found")
    for path in scenarios:
        try:
            fixture = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")
            continue
        if not fixture.get("primitive") or not fixture.get("scenarios"):
            errors.append(
                f"{path.relative_to(ROOT)}: primitive and scenarios are required"
            )

    migrations = sorted((ROOT / "src/migrations").glob("v*.sql"))
    if not migrations:
        errors.append("no canonical migrations found")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    errors = check(args.version)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print(f"release inputs agree on {args.version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

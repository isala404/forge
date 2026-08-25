#!/usr/bin/env python3
"""Validate Forge's canonical contract and generate language DTOs and inventories."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CONTRACT_PATH = ROOT / "contract" / "forge.json"
SCHEMA_PATH = ROOT / "contract" / "schema-v1.json"


def load_contract() -> dict[str, Any]:
    contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
    schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    validate_json_schema(contract, schema, "contract")
    required = {
        "schema_version",
        "contract_version",
        "release_status",
        "compatibility",
        "languages",
        "units",
        "defaults",
        "limits",
        "errors",
        "backend_capabilities",
        "lifecycle",
        "dtos",
        "operations",
        "development_gaps",
    }
    if set(contract) != required:
        missing = sorted(required - set(contract))
        extra = sorted(set(contract) - required)
        raise ValueError(f"contract root mismatch; missing={missing}, extra={extra}")
    if contract["schema_version"] != 1:
        raise ValueError("unsupported contract schema_version")
    if not re.fullmatch(r"\d+\.\d+\.\d+", contract["contract_version"]):
        raise ValueError("contract_version must be semantic-version shaped")
    if set(contract["languages"]) != {"rust", "javascript", "python", "go"}:
        raise ValueError("Forge requires exactly rust, javascript, python, and go")
    error_codes = [error["code"] for error in contract["errors"]]
    require_unique("error code", error_codes)
    dto_names = [dto["name"] for dto in contract["dtos"]]
    require_unique("DTO name", dto_names)
    operation_ids = [operation["id"] for operation in contract["operations"]]
    require_unique("operation id", operation_ids)
    for operation in contract["operations"]:
        if set(operation["methods"]) != set(contract["languages"]):
            raise ValueError(f"{operation['id']} does not map all four languages")
        if not all(operation["methods"].values()):
            raise ValueError(f"{operation['id']} has an empty language method mapping")
        unknown = set(operation["errors"]) - set(error_codes)
        if unknown:
            raise ValueError(f"{operation['id']} has unknown errors: {sorted(unknown)}")
    return contract


def validate_json_schema(value: Any, schema: dict[str, Any], path: str) -> None:
    expected_type = schema.get("type")
    type_matches = {
        "object": isinstance(value, dict),
        "array": isinstance(value, list),
        "string": isinstance(value, str),
        "integer": isinstance(value, int) and not isinstance(value, bool),
        "number": isinstance(value, (int, float)) and not isinstance(value, bool),
        "boolean": isinstance(value, bool),
    }
    if expected_type is not None and not type_matches.get(expected_type, False):
        raise ValueError(f"{path} must be {expected_type}")
    if "const" in schema and value != schema["const"]:
        raise ValueError(f"{path} must equal {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        raise ValueError(f"{path} must be one of {schema['enum']!r}")
    if isinstance(value, str) and "pattern" in schema and re.fullmatch(schema["pattern"], value) is None:
        raise ValueError(f"{path} does not match {schema['pattern']!r}")
    if isinstance(value, dict):
        required = set(schema.get("required", []))
        missing = sorted(required - set(value))
        if missing:
            raise ValueError(f"{path} is missing {missing}")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            extra = sorted(set(value) - set(properties))
            if extra:
                raise ValueError(f"{path} has unsupported properties {extra}")
        for name, child in value.items():
            if name in properties:
                validate_json_schema(child, properties[name], f"{path}.{name}")
    if isinstance(value, list):
        if schema.get("uniqueItems") and len({json.dumps(item, sort_keys=True) for item in value}) != len(value):
            raise ValueError(f"{path} contains duplicate items")
        item_schema = schema.get("items")
        if item_schema:
            for index, child in enumerate(value):
                validate_json_schema(child, item_schema, f"{path}[{index}]")


def require_unique(label: str, values: list[str]) -> None:
    duplicates = sorted({value for value in values if values.count(value) > 1})
    if duplicates:
        raise ValueError(f"duplicate {label}: {', '.join(duplicates)}")


def rust_type(logical: str, language: str, dto_names: dict[str, str]) -> str:
    optional = logical.endswith("?")
    array = logical.removesuffix("?").endswith("[]")
    base = logical.removesuffix("?").removesuffix("[]")
    scalars = {
        "string": "String",
        "boolean": "bool",
        "uint32": "u32",
        "int64": "f64" if language == "node" else "i64",
        "float64": "f64",
        "count": "u32" if language == "node" else "u64",
        "size": "f64" if language == "node" else "u64",
        "bytes": "Buffer" if language == "node" else ("Py<PyBytes>" if language == "python" else "Vec<u8>"),
        "string_map": "HashMap<String, String>",
    }
    rendered = scalars.get(base, dto_names.get(base))
    if rendered is None:
        raise ValueError(f"unknown DTO field type {logical}")
    if array:
        rendered = f"Vec<{rendered}>"
    if optional:
        rendered = f"Option<{rendered}>"
    return rendered


def python_type(logical: str, dto_names: dict[str, str]) -> str:
    optional = logical.endswith("?")
    array = logical.removesuffix("?").endswith("[]")
    base = logical.removesuffix("?").removesuffix("[]")
    rendered = {
        "string": "str",
        "boolean": "bool",
        "uint32": "int",
        "int64": "int",
        "float64": "float",
        "count": "int",
        "size": "int",
        "bytes": "bytes",
        "string_map": "dict[str, str]",
    }.get(base, dto_names.get(base))
    if rendered is None:
        raise ValueError(f"unknown DTO field type {logical}")
    if array:
        rendered = f"list[{rendered}]"
    if optional:
        rendered = f"Optional[{rendered}]"
    return rendered


def typescript_type(logical: str, dto_names: dict[str, str]) -> str:
    optional = logical.endswith("?")
    array = logical.removesuffix("?").endswith("[]")
    base = logical.removesuffix("?").removesuffix("[]")
    rendered = {
        "string": "string",
        "boolean": "boolean",
        "uint32": "number",
        "int64": "number",
        "float64": "number",
        "count": "number",
        "size": "number",
        "bytes": "Buffer",
        "string_map": "Record<string, string>",
    }.get(base, dto_names.get(base))
    if rendered is None:
        raise ValueError(f"unknown DTO field type {logical}")
    if array:
        rendered = f"Array<{rendered}>"
    if optional:
        rendered = f"{rendered} | null"
    return rendered


def go_type(logical: str, dto_names: dict[str, str]) -> str:
    optional = logical.endswith("?")
    array = logical.removesuffix("?").endswith("[]")
    base = logical.removesuffix("?").removesuffix("[]")
    rendered = {
        "string": "string",
        "boolean": "bool",
        "uint32": "uint32",
        "int64": "int64",
        "float64": "float64",
        "count": "uint64",
        "size": "uint64",
        "bytes": "[]byte",
        "string_map": "map[string]string",
    }.get(base, dto_names.get(base))
    if rendered is None:
        raise ValueError(f"unknown DTO field type {logical}")
    if array:
        rendered = f"[]{rendered}"
    if optional:
        rendered = f"*{rendered}"
    return rendered


def camel(value: str) -> str:
    head, *tail = value.split("_")
    return head + "".join(part.title() for part in tail)


def pascal(value: str) -> str:
    initialisms = {"id": "ID", "etag": "ETag", "url": "URL", "json": "JSON"}
    return "".join(initialisms.get(part, part[:1].upper() + part[1:]) for part in value.split("_"))


def doc_lines(indent: str, value: str) -> str:
    return f"{indent}/// {value}\n" if value else ""


def gen_node_rust(contract: dict[str, Any]) -> str:
    names = {dto["name"]: dto["node_name"] for dto in contract["dtos"]}
    out = "// @generated by tools/contract/generate.py. DO NOT EDIT.\n// Source of truth: contract/forge.json\n"
    for dto in contract["dtos"]:
        out += f"\n/// {dto['doc']}\n#[napi(object)]\npub struct {dto['node_name']} {{\n"
        for field in dto["fields"]:
            out += doc_lines("    ", field["doc"])
            out += f"    pub {field['name']}: {rust_type(field['type'], 'node', names)},\n"
        out += "}\n"
    return out


def gen_python_rust(contract: dict[str, Any]) -> str:
    names = {dto["name"]: dto["python_name"] for dto in contract["dtos"]}
    out = "// @generated by tools/contract/generate.py. DO NOT EDIT.\n// Source of truth: contract/forge.json\n"
    for dto in contract["dtos"]:
        out += f"\n/// {dto['doc']}\n#[pyclass(get_all"
        out += ", skip_from_py_object" if dto["clone"] else ""
        out += ")]\n"
        if dto["clone"]:
            out += "#[derive(Clone)]\n"
        out += f"struct {dto['python_name']} {{\n"
        for field in dto["fields"]:
            out += doc_lines("    ", field["doc"])
            out += f"    {field['name']}: {rust_type(field['type'], 'python', names)},\n"
        out += "}\n"
    return out


def gen_python_stub(contract: dict[str, Any]) -> str:
    names = {dto["name"]: dto["python_name"] for dto in contract["dtos"]}
    out = "# @generated by tools/contract/generate.py. DO NOT EDIT.\n# Source of truth: contract/forge.json\nfrom __future__ import annotations\nfrom typing import Optional\n"
    for dto in contract["dtos"]:
        out += f"\nclass {dto['python_name']}:\n    \"\"\"{dto['doc']}\"\"\"\n"
        for field in dto["fields"]:
            out += f"    {field['name']}: {python_type(field['type'], names)}\n"
    return out


def gen_typescript(contract: dict[str, Any]) -> str:
    names = {dto["name"]: dto["node_name"].removeprefix("Js") for dto in contract["dtos"]}
    out = "// @generated by tools/contract/generate.py. DO NOT EDIT.\n// Source of truth: contract/forge.json\n"
    for dto in contract["dtos"]:
        out += f"\n/** {dto['doc']} */\nexport interface {names[dto['name']]} {{\n"
        for field in dto["fields"]:
            out += f"  {camel(field['name'])}: {typescript_type(field['type'], names)};\n"
        out += "}\n"
    return out


def gen_go(contract: dict[str, Any]) -> str:
    names = {dto["name"]: dto["name"] for dto in contract["dtos"]}
    out = "// Code generated by tools/contract/generate.py. DO NOT EDIT.\n\npackage forge\n"
    for dto in contract["dtos"]:
        out += f"\n// {dto['name']} {dto['doc']}\ntype {dto['name']} struct {{\n"
        for field in dto["fields"]:
            out += f"\t{pascal(field['name'])} {go_type(field['type'], names)} `json:\"{field['name']}\"`\n"
        out += "}\n"
    formatted = subprocess.run(["gofmt"], input=out, capture_output=True, text=True, check=False)
    if formatted.returncode != 0:
        raise ValueError(f"gofmt failed: {formatted.stderr.strip()}")
    return formatted.stdout


def gen_inventory(contract: dict[str, Any]) -> str:
    inventory = {
        "contract_version": contract["contract_version"],
        "languages": {
            language: {
                operation["id"]: operation["methods"][language]
                for operation in contract["operations"]
            }
            for language in contract["languages"]
        },
    }
    return json.dumps(inventory, indent=2, sort_keys=True) + "\n"


def gen_reference(contract: dict[str, Any]) -> str:
    out = "---\ntitle: Contract reference\ndescription: Generated cross-language operation and result mappings.\n---\n\n"
    out += "{/* @generated by tools/contract/generate.py. DO NOT EDIT. */}\n\n"
    out += f"Contract version: `{contract['contract_version']}`. Durations use seconds unless a field ends in `_ms`; sizes use bytes.\n\n"
    out += "| Operation | Rust | JavaScript | Python | Go | Result |\n| --- | --- | --- | --- | --- | --- |\n"
    for operation in contract["operations"]:
        methods = operation["methods"]
        cells = [", ".join(f"`{name}`" for name in methods[language]) for language in ("rust", "javascript", "python", "go")]
        out += f"| `{operation['id']}` | {cells[0]} | {cells[1]} | {cells[2]} | {cells[3]} | `{operation['result']}` |\n"
    return out


def outputs(contract: dict[str, Any]) -> dict[Path, str]:
    return {
        ROOT / "bindings" / "node" / "src" / "types.generated.rs": gen_node_rust(contract),
        ROOT / "bindings" / "node" / "contract.generated.d.ts": gen_typescript(contract),
        ROOT / "bindings" / "python" / "src" / "types.generated.rs": gen_python_rust(contract),
        ROOT / "bindings" / "python" / "python" / "forgelib" / "_generated.pyi": gen_python_stub(contract),
        ROOT / "bindings" / "go" / "contract_types_gen.go": gen_go(contract),
        ROOT / "contract" / "api-inventory.json": gen_inventory(contract),
        ROOT / "docs" / "src" / "content" / "docs" / "contract-reference-generated.mdx": gen_reference(contract),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--release", action="store_true", help="reject development gaps")
    args = parser.parse_args()
    try:
        contract = load_contract()
        if args.release and contract["development_gaps"]:
            raise ValueError("supported releases require an empty development_gaps set")
        stale: list[Path] = []
        for path, content in outputs(contract).items():
            current = path.read_text(encoding="utf-8") if path.exists() else ""
            if current == content:
                continue
            if args.check:
                stale.append(path)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
                print(f"generated {path.relative_to(ROOT)}")
        if stale:
            print("generated contract files are stale:", file=sys.stderr)
            for path in stale:
                print(f"  {path.relative_to(ROOT)}", file=sys.stderr)
            return 1
        print(f"contract {contract['contract_version']}: {len(contract['operations'])} operations, {len(contract['dtos'])} DTOs")
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"contract generation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

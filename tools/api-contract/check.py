#!/usr/bin/env python3
"""Reject public API drift from contract/forge.json."""

from __future__ import annotations

import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONTRACT = json.loads((ROOT / "contract" / "forge.json").read_text(encoding="utf-8"))

RUST_TRAITS = {
    "kv": ("Kv", ROOT / "src" / "kv" / "mod.rs"),
    "queue": ("Queue", ROOT / "src" / "queue" / "mod.rs"),
    "config": ("ConfigStore", ROOT / "src" / "config_store" / "mod.rs"),
    "ratelimit": ("RateLimit", ROOT / "src" / "ratelimit" / "mod.rs"),
    "blob": ("Blob", ROOT / "src" / "blob" / "mod.rs"),
    "auth": ("Auth", ROOT / "src" / "auth" / "mod.rs"),
    "schedule": ("Schedule", ROOT / "src" / "schedule" / "mod.rs"),
    "pubsub": ("Pubsub", ROOT / "src" / "pubsub" / "mod.rs"),
}
NODE_RAW = ROOT / "bindings" / "node" / "index.d.ts"
NODE_CLIENT = ROOT / "bindings" / "node" / "client.d.ts"
PY_STUB = ROOT / "bindings" / "python" / "python" / "forgelib" / "__init__.pyi"
GO_DIR = ROOT / "bindings" / "go"

NODE_IDIOMATIC = {
    "ForgeClient": {"init", "initFrom", "queue", "kv", "config", "topic", "worker", "runOutboxRelay"},
    "Queue": {"enqueue", "dequeue", "depth", "worker"},
    "QueueJob": {"ack", "nack", "heartbeat"},
    "KvKey": {"get", "getOrDefault", "set", "delete", "exists", "expire", "compareAndSwap"},
    "ConfigKey": {"get", "getOrDefault", "set", "delete", "flag"},
    "Topic": {"publish", "subscribe", "channel"},
    "TopicSubscription": {"next", "return", "close"},
}
NODE_TOP_LEVEL = {"runWorker", "queue", "kv", "config", "topic", "scopeKvKey", "scopeBlobKey", "scopeRateLimitSubject", "scopeTopic", "parseScopedName", "forgeErrorCode", "forgeErrorRetryable"}
NODE_WRAPPER_CONTRACT = {"runOutboxRelay"}
PY_IDIOMATIC = {
    "ForgeClient": {"init", "init_from", "queue", "kv", "config", "topic", "rate_limit", "worker", "run_outbox_relay"},
    "Queue": {"enqueue", "dequeue", "ack", "nack", "heartbeat", "depth", "worker"},
    "KvKey": {"get", "get_or_default", "set", "delete", "exists", "expire", "compare_and_swap"},
    "ConfigKey": {"get", "get_or_default", "set", "delete"},
    "Topic": {"publish", "subscribe"},
}
PY_TOP_LEVEL = {"run_worker", "queue", "kv", "config", "topic", "scope_kv_key", "scope_blob_key", "scope_rate_limit_subject", "scope_topic", "parse_scoped_name", "forge_error_code", "forge_error_retryable"}
GO_FORGE_ADDITIONS = {"BlobGetRange", "BlobOpen", "BlobPutStream", "Close", "Mode", "Namespace", "RunWorker"}
GO_TOP_LEVEL = {"CreateOnly", "DecodeCloudEvent", "DecodeInvalidationEvent", "DecodeQueueEnvelope", "EncodeCloudEvent", "EncodeInvalidationEvent", "ErrorCodeOf", "ExportEnvConfig", "ImportEnvConfig", "Init", "InitDefault", "InitFrom", "InitFromString", "IsRetryable", "KMSManagedEncryption", "MatchVersion", "NewManualClock", "NewMemory", "NewMemoryForTesting", "NewMemoryStore", "NewQueueEnvelope", "NewSeededReader", "NewTraceContext", "ParseScopedName", "S3ManagedEncryption", "ScopeKVKey", "ScopeBlobKey", "ScopeRateLimitSubject", "ScopeTopic"}
GO_CLIENT_TOP_LEVEL = {
    "InitDefault",
    "InitFrom",
    "InitFromString",
    "NewMemoryForTesting",
    "Migrate",
    "MigrateFrom",
    "MigrateFromString",
    "MigrationStatus",
    "MigrationStatusFrom",
    "MigrationStatusFromString",
    "ValidateSchema",
    "ValidateSchemaFrom",
    "ValidateSchemaFromString",
    "ScopeKVKey",
    "ScopeBlobKey",
    "ScopeRateLimitSubject",
    "ScopeTopic",
}


def brace_body(text: str, pattern: str, label: str) -> str:
    match = re.search(pattern, text)
    if match is None:
        raise ValueError(f"could not find {label}")
    open_at = text.find("{", match.end())
    if open_at < 0:
        raise ValueError(f"could not find body for {label}")
    depth = 0
    for index in range(open_at, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : index]
    raise ValueError(f"unclosed body for {label}")


def rust_trait_methods(trait: str, path: Path) -> set[str]:
    body = brace_body(path.read_text(encoding="utf-8"), rf"\bpub\s+trait\s+{re.escape(trait)}\b", f"Rust trait {trait}")
    return set(re.findall(r"\b(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", body))


def rust_impl_methods(path: Path, type_name: str) -> set[str]:
    body = brace_body(path.read_text(encoding="utf-8"), rf"\bimpl\s+{re.escape(type_name)}\b", f"Rust impl {type_name}")
    return set(re.findall(r"\bpub\s+(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", body))


def ts_class_methods(path: Path, class_name: str) -> set[str]:
    body = brace_body(path.read_text(encoding="utf-8"), rf"\bclass\s+{re.escape(class_name)}\b", f"TypeScript class {class_name}")
    out: set[str] = set()
    for line in body.splitlines():
        match = re.match(r"\s*(?:static\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^;{]*>)?\s*\(", line)
        if match:
            out.add(match.group(1))
    return out


def ts_top_level_functions(path: Path) -> set[str]:
    return set(re.findall(r"^export declare function ([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^;{]*>)?\s*\(", path.read_text(encoding="utf-8"), re.M))


def py_class_methods(path: Path, class_name: str) -> set[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    start = next((index for index, line in enumerate(lines) if re.match(rf"^class\s+{re.escape(class_name)}(?:\(|:)", line)), None)
    if start is None:
        return set()
    out: set[str] = set()
    for line in lines[start + 1 :]:
        if line and not line.startswith((" ", "\t")):
            break
        match = re.match(r"\s+(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", line)
        if match:
            out.add(match.group(1))
    return out


def py_top_level_functions(path: Path) -> set[str]:
    return set(re.findall(r"^(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", path.read_text(encoding="utf-8"), re.M))


def go_source() -> str:
    return "\n".join(path.read_text(encoding="utf-8") for path in GO_DIR.glob("*.go") if not path.name.endswith("_test.go"))


def go_forge_methods() -> set[str]:
    return set(re.findall(r"^func\s+\([^)]*\*Forge\)\s+([A-Z][A-Za-z0-9_]*)\s*\(", go_source(), re.M))


def go_top_level_functions() -> set[str]:
    return set(re.findall(r"^func\s+([A-Z][A-Za-z0-9_]*)\s*\(", go_source(), re.M))


@dataclass
class Check:
    problems: list[str] = field(default_factory=list)

    def exact(self, label: str, actual: set[str], expected: set[str]) -> None:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        if missing:
            self.problems.append(f"{label} missing: {', '.join(missing)}")
        if extra:
            self.problems.append(f"{label} has unregistered methods: {', '.join(extra)}")

    def contains(self, label: str, actual: set[str], expected: set[str]) -> None:
        missing = sorted(expected - actual)
        if missing:
            self.problems.append(f"{label} missing: {', '.join(missing)}")


def mapped_methods(language: str, primitive: str | None = None) -> set[str]:
    return {
        method
        for operation in CONTRACT["operations"]
        if primitive is None or operation["primitive"] == primitive
        for method in operation["methods"][language]
    }


def main() -> int:
    check = Check()
    for primitive, (trait, path) in RUST_TRAITS.items():
        check.exact(f"Rust trait {trait}", rust_trait_methods(trait, path), mapped_methods("rust", primitive))
    check.exact("Rust outbox", rust_impl_methods(ROOT / "src" / "outbox.rs", "Forge"), mapped_methods("rust", "outbox"))
    check.contains("Rust Forge", rust_impl_methods(ROOT / "src" / "lib.rs", "Forge"), mapped_methods("rust", "client"))
    check.exact(
        "Node raw ForgeClient",
        ts_class_methods(NODE_RAW, "ForgeClient"),
        mapped_methods("javascript") - mapped_methods("javascript", "scope") - NODE_WRAPPER_CONTRACT,
    )
    check.exact("Python ForgeClient", py_class_methods(PY_STUB, "ForgeClient"), (mapped_methods("python") - mapped_methods("python", "scope")) | PY_IDIOMATIC["ForgeClient"])
    for class_name, expected in NODE_IDIOMATIC.items():
        check.contains(f"Node {class_name}", ts_class_methods(NODE_CLIENT, class_name), expected)
    check.contains("Node top-level exports", ts_top_level_functions(NODE_CLIENT), NODE_TOP_LEVEL)
    for class_name, expected in PY_IDIOMATIC.items():
        check.contains(f"Python {class_name}", py_class_methods(PY_STUB, class_name), expected)
    check.contains("Python top-level exports", py_top_level_functions(PY_STUB), PY_TOP_LEVEL)
    go_mapped = mapped_methods("go")
    check.exact("Go Forge", go_forge_methods(), (go_mapped - GO_CLIENT_TOP_LEVEL) | GO_FORGE_ADDITIONS)
    check.exact("Go top-level functions", go_top_level_functions(), GO_TOP_LEVEL | (go_mapped & GO_CLIENT_TOP_LEVEL))
    if check.problems:
        print("api-contract-check: public APIs drifted from contract/forge.json:", file=sys.stderr)
        for problem in check.problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print("api-contract-check: Rust, JavaScript, Python, and Go public APIs match contract/forge.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

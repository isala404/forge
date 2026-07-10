#!/usr/bin/env python3
"""Static API contract drift guard for Forge's client libraries.

Conformance tests prove behavior. This guard proves the public surface is wired:
every core primitive method listed here must exist in Rust and in the raw Node/Python
bindings, and the idiomatic client handles must keep their expected methods.

When adding a core primitive method, update this matrix and expose the matching
Node/Python methods in the same change. If a method is deliberately Rust-only, add
an explicit comment and exception here instead of letting it drift silently.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

RUST_TRAITS = {
    "Kv": ROOT / "src" / "kv" / "mod.rs",
    "Queue": ROOT / "src" / "queue" / "mod.rs",
    "ConfigStore": ROOT / "src" / "config_store" / "mod.rs",
    "RateLimit": ROOT / "src" / "ratelimit" / "mod.rs",
    "Blob": ROOT / "src" / "blob" / "mod.rs",
    "Auth": ROOT / "src" / "auth" / "mod.rs",
    "Schedule": ROOT / "src" / "schedule" / "mod.rs",
    "Pubsub": ROOT / "src" / "pubsub" / "mod.rs",
}

NODE_RAW = ROOT / "bindings" / "node" / "index.d.ts"
NODE_CLIENT = ROOT / "bindings" / "node" / "client.d.ts"
PY_STUB = ROOT / "bindings" / "python" / "python" / "forgelib" / "__init__.pyi"


@dataclass(frozen=True)
class Operation:
    name: str
    rust_trait: str
    rust_methods: tuple[str, ...]
    node: tuple[str, ...]
    python: tuple[str, ...]


OPERATIONS = [
    Operation("kv.get", "Kv", ("get",), ("kvGet", "kvGetBytes"), ("kv_get", "kv_get_bytes")),
    Operation("kv.mget", "Kv", ("mget",), ("kvMget",), ("kv_mget",)),
    Operation("kv.set", "Kv", ("set",), ("kvSet", "kvSetBytes"), ("kv_set", "kv_set_bytes")),
    Operation("kv.delete", "Kv", ("delete",), ("kvDelete",), ("kv_delete",)),
    Operation("kv.exists", "Kv", ("exists",), ("kvExists",), ("kv_exists",)),
    Operation("kv.incr", "Kv", ("incr",), ("kvIncr",), ("kv_incr",)),
    Operation("kv.expire", "Kv", ("expire",), ("kvExpire",), ("kv_expire",)),
    Operation(
        "kv.compare_and_swap",
        "Kv",
        ("compare_and_swap",),
        ("kvCompareAndSwap",),
        ("kv_compare_and_swap",),
    ),
    Operation("kv.scan", "Kv", ("scan",), ("kvScan", "kvScanPage"), ("kv_scan", "kv_scan_page")),
    Operation("queue.enqueue", "Queue", ("enqueue",), ("queueEnqueue",), ("queue_enqueue",)),
    Operation("queue.dequeue", "Queue", ("dequeue",), ("queueDequeue",), ("queue_dequeue",)),
    Operation("queue.ack", "Queue", ("ack",), ("queueAck",), ("queue_ack",)),
    Operation("queue.nack", "Queue", ("nack",), ("queueNack",), ("queue_nack",)),
    Operation("queue.heartbeat", "Queue", ("heartbeat",), ("queueHeartbeat",), ("queue_heartbeat",)),
    Operation("queue.depth", "Queue", ("depth",), ("queueDepth",), ("queue_depth",)),
    Operation("config.get_raw", "ConfigStore", ("get_raw",), ("configGet",), ("config_get",)),
    Operation("config.set_raw", "ConfigStore", ("set_raw",), ("configSet",), ("config_set",)),
    Operation("config.delete_raw", "ConfigStore", ("delete_raw",), ("configDelete",), ("config_delete",)),
    Operation("config.flag", "ConfigStore", ("flag",), ("flag",), ("flag",)),
    Operation(
        "config.set_flag",
        "ConfigStore",
        ("set_flag",),
        ("setFlagPercent", "setFlagOn", "setFlagOff", "setFlagAllowList"),
        ("set_flag_percent", "set_flag_on", "set_flag_off", "set_flag_allow_list"),
    ),
    Operation("config.delete_flag", "ConfigStore", ("delete_flag",), ("deleteFlag",), ("delete_flag",)),
    Operation("ratelimit.check", "RateLimit", ("check", "check_with"), ("rateLimitCheck",), ("rate_limit_check",)),
    Operation(
        "blob.put",
        "Blob",
        ("put",),
        ("blobPut", "blobPutBytes", "blobPutObject"),
        ("blob_put", "blob_put_object"),
    ),
    Operation("blob.get", "Blob", ("get",), ("blobGet", "blobGetBytes"), ("blob_get",)),
    Operation("blob.head", "Blob", ("head",), ("blobHead", "blobContentType"), ("blob_head", "blob_content_type")),
    Operation("blob.delete", "Blob", ("delete",), ("blobDelete",), ("blob_delete",)),
    Operation("blob.list", "Blob", ("list",), ("blobList",), ("blob_list",)),
    Operation("blob.presign_upload", "Blob", ("presign_upload",), ("blobPresignUpload",), ("blob_presign_upload",)),
    Operation(
        "blob.presign_download",
        "Blob",
        ("presign_download",),
        ("blobPresignDownload",),
        ("blob_presign_download",),
    ),
    Operation(
        "blob.verify_presigned",
        "Blob",
        ("verify_presigned",),
        ("blobVerifyPresign",),
        ("blob_verify_presign",),
    ),
    Operation("auth.hash_password", "Auth", ("hash_password",), ("hashPassword",), ("hash_password",)),
    Operation("auth.verify_password", "Auth", ("verify_password",), ("verifyPassword",), ("verify_password",)),
    Operation("auth.needs_rehash", "Auth", ("needs_rehash",), ("needsRehash",), ("needs_rehash",)),
    Operation("auth.create_session", "Auth", ("create_session",), ("createSession",), ("create_session",)),
    Operation(
        "auth.validate_session",
        "Auth",
        ("validate_session",),
        ("validateSession", "validateSessionInfo"),
        ("validate_session", "validate_session_info"),
    ),
    Operation("auth.revoke_session", "Auth", ("revoke_session",), ("revokeSession",), ("revoke_session",)),
    Operation(
        "auth.revoke_all_sessions",
        "Auth",
        ("revoke_all_sessions",),
        ("revokeAllSessions",),
        ("revoke_all_sessions",),
    ),
    Operation("auth.create_api_key", "Auth", ("create_api_key",), ("createApiKey",), ("create_api_key",)),
    Operation(
        "auth.verify_api_key",
        "Auth",
        ("verify_api_key",),
        ("verifyApiKey", "verifyApiKeyInfo"),
        ("verify_api_key", "verify_api_key_info"),
    ),
    Operation("auth.revoke_api_key", "Auth", ("revoke_api_key",), ("revokeApiKey",), ("revoke_api_key",)),
    Operation("auth.create_token", "Auth", ("create_token",), ("createToken",), ("create_token",)),
    Operation("auth.consume_token", "Auth", ("consume_token",), ("consumeToken",), ("consume_token",)),
    Operation("schedule.cron", "Schedule", ("cron",), ("scheduleCron",), ("schedule_cron",)),
    Operation("schedule.at", "Schedule", ("at",), ("scheduleAt",), ("schedule_at",)),
    Operation("schedule.cancel", "Schedule", ("cancel",), ("scheduleCancel",), ("schedule_cancel",)),
    Operation("schedule.cancel_at", "Schedule", ("cancel_at",), ("scheduleCancelAt",), ("schedule_cancel_at",)),
    Operation("schedule.list", "Schedule", ("list",), ("scheduleList",), ("schedule_list",)),
    Operation(
        "schedule.process_due",
        "Schedule",
        ("process_due",),
        ("runSchedulerOnce",),
        ("run_scheduler_once",),
    ),
    Operation("pubsub.channel_for", "Pubsub", ("channel_for",), ("pubsubChannel",), ("pubsub_channel",)),
    Operation("pubsub.publish", "Pubsub", ("publish",), ("pubsubPublish",), ("pubsub_publish",)),
    Operation("pubsub.subscribe", "Pubsub", ("subscribe",), ("pubsubSubscribe",), ("pubsub_subscribe",)),
]

ROOT_RAW = {
    # Root client helpers that are not primitive operations.
    "node": {"init", "initFrom", "backendReport", "maintain", "postgresUrl"},
    "python": {"init", "init_from", "backend_report", "maintain", "postgres_url"},
}

NODE_IDIOMATIC = {
    "ForgeClient": {"init", "initFrom", "queue", "kv", "config", "topic", "worker"},
    "Queue": {"enqueue", "dequeue", "depth", "worker"},
    "QueueJob": {"ack", "nack", "heartbeat"},
    "KvKey": {"get", "getOrDefault", "set", "delete", "exists", "expire", "compareAndSwap"},
    "ConfigKey": {"get", "getOrDefault", "set", "delete", "flag"},
    "Topic": {"publish", "subscribe", "channel"},
    "TopicSubscription": {"next", "return", "close"},
}

NODE_TOP_LEVEL = {
    "runWorker",
    "queue",
    "kv",
    "config",
    "topic",
    "forgeErrorCode",
    "forgeErrorRetryable",
}

PY_IDIOMATIC = {
    "ForgeClient": {"init", "init_from", "queue", "kv", "config", "topic", "worker"},
    "Queue": {"enqueue", "dequeue", "ack", "nack", "heartbeat", "depth", "worker"},
    "KvKey": {"get", "get_or_default", "set", "delete", "exists", "expire", "compare_and_swap"},
    "ConfigKey": {"get", "get_or_default", "set", "delete"},
    "Topic": {"publish", "subscribe"},
}

PY_TOP_LEVEL = {
    "run_worker",
    "queue",
    "kv",
    "config",
    "topic",
    "forge_error_code",
    "forge_error_retryable",
}


def brace_body(text: str, pattern: str, label: str) -> str:
    match = re.search(pattern, text)
    if match is None:
        raise ValueError(f"could not find {label}")
    start = match.start()
    open_at = text.find("{", match.end())
    if open_at < 0:
        raise ValueError(f"could not find body for {label}")
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : i]
    raise ValueError(f"unclosed body for {label}")


def rust_trait_methods(trait: str) -> set[str]:
    text = RUST_TRAITS[trait].read_text(encoding="utf-8")
    body = brace_body(text, rf"\bpub\s+trait\s+{re.escape(trait)}\b", f"Rust trait {trait}")
    return set(re.findall(r"\b(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", body))


def ts_class_methods(path: Path, cls: str) -> set[str]:
    text = path.read_text(encoding="utf-8")
    body = brace_body(text, rf"\bclass\s+{re.escape(cls)}\b", f"TypeScript class {cls}")
    out: set[str] = set()
    for line in body.splitlines():
        m = re.match(r"\s*(?:static\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^;{]*>)?\s*\(", line)
        if m:
            out.add(m.group(1))
    return out


def ts_top_level_functions(path: Path) -> set[str]:
    text = path.read_text(encoding="utf-8")
    return set(re.findall(r"^export declare function ([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^;{]*>)?\s*\(", text, re.M))


def py_class_methods(path: Path, cls: str) -> set[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    class_re = re.compile(rf"^class\s+{re.escape(cls)}(?:\(|:)")
    start = next((i for i, line in enumerate(lines) if class_re.match(line)), None)
    if start is None:
        return set()
    out: set[str] = set()
    for line in lines[start + 1 :]:
        if line and not line.startswith((" ", "\t")):
            break
        m = re.match(r"\s+(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", line)
        if m:
            out.add(m.group(1))
    return out


def py_top_level_functions(path: Path) -> set[str]:
    text = path.read_text(encoding="utf-8")
    return set(re.findall(r"^(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", text, re.M))


@dataclass
class CheckState:
    problems: list[str] = field(default_factory=list)

    def require(self, label: str, actual: set[str], expected: set[str]) -> None:
        missing = sorted(expected - actual)
        if missing:
            self.problems.append(f"{label} missing: {', '.join(missing)}")

    def no_extra(self, label: str, actual: set[str], expected: set[str]) -> None:
        extra = sorted(actual - expected)
        if extra:
            self.problems.append(f"{label} has untracked methods: {', '.join(extra)}")


def main() -> int:
    state = CheckState()
    rust_expected: dict[str, set[str]] = {trait: set() for trait in RUST_TRAITS}
    node_expected = set(ROOT_RAW["node"])
    py_expected = set(ROOT_RAW["python"])

    for op in OPERATIONS:
        rust_expected[op.rust_trait].update(op.rust_methods)
        node_expected.update(op.node)
        py_expected.update(op.python)

    for trait, expected in rust_expected.items():
        actual = rust_trait_methods(trait)
        state.require(f"Rust trait {trait}", actual, expected)
        state.no_extra(f"Rust trait {trait}", actual, expected)

    node_raw = ts_class_methods(NODE_RAW, "ForgeClient")
    state.require("Node raw ForgeClient", node_raw, node_expected)
    state.no_extra("Node raw ForgeClient", node_raw, node_expected)

    py_client = py_class_methods(PY_STUB, "ForgeClient")
    state.require("Python ForgeClient", py_client, py_expected | PY_IDIOMATIC["ForgeClient"])
    state.no_extra("Python ForgeClient", py_client, py_expected | PY_IDIOMATIC["ForgeClient"])

    for cls, expected in NODE_IDIOMATIC.items():
        state.require(f"Node {cls}", ts_class_methods(NODE_CLIENT, cls), expected)

    state.require("Node top-level exports", ts_top_level_functions(NODE_CLIENT), NODE_TOP_LEVEL)

    for cls, expected in PY_IDIOMATIC.items():
        state.require(f"Python {cls}", py_class_methods(PY_STUB, cls), expected)

    state.require("Python top-level exports", py_top_level_functions(PY_STUB), PY_TOP_LEVEL)

    if state.problems:
        print("api-contract-check: client libraries drifted from the core contract:\n", file=sys.stderr)
        for problem in state.problems:
            print(f"  {problem}", file=sys.stderr)
        print(
            "\nUpdate the Rust/Node/Python APIs together, or update tools/api-contract/check.py "
            "with an explicit reviewed exception.",
            file=sys.stderr,
        )
        return 1

    print(
        "api-contract-check: OK — Rust core, Node, and Python public APIs match the contract"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

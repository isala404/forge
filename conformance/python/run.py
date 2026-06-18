"""Cross-language conformance runner — Python side.

Reads ../scenarios/*.json and runs each scenario against the forge_py binding on
a throwaway database. Asserts the observed failure set equals exactly the
``python`` entries in ../known_gaps.json. See ../README.md.

    TEST_DATABASE_URL=postgres://… python conformance/python/run.py
"""

import asyncio
import json
import os
import pathlib
import sys
import uuid

import psycopg
import forge_py

HERE = pathlib.Path(__file__).resolve().parent
SCENARIO_DIR = HERE.parent / "scenarios"
GAPS_FILE = HERE.parent / "known_gaps.json"
LANG = "python"

ADMIN_URL = os.environ.get("TEST_DATABASE_URL")
if not ADMIN_URL:
    print("TEST_DATABASE_URL is not set", file=sys.stderr)
    sys.exit(2)


# ---- throwaway database ----
def swap_db(url: str, name: str) -> str:
    from urllib.parse import urlsplit, urlunsplit

    parts = urlsplit(url)
    return urlunsplit(parts._replace(path="/" + name))


def admin_exec(sql: str) -> None:
    with psycopg.connect(ADMIN_URL, autocommit=True) as conn:
        conn.execute(sql)


# ---- error mapping (exception class name is already the canonical code) ----
CODES = {"Config", "Unavailable", "NotFound", "Precondition", "Limit", "Invalid", "Backend"}


def canonical_error_code(exc: BaseException) -> str:
    name = type(exc).__name__
    return name if name in CODES else "Backend"


# ---- value helpers ----
def value_to_str(v):
    if isinstance(v, str):
        return v
    if isinstance(v, dict) and "$bytes" in v:
        return bytes(v["$bytes"]).decode("utf-8", "replace")  # lossy until a bytes API exists
    raise ValueError(f"cannot coerce value to string: {v!r}")


def as_bytes(actual):
    if actual is None:
        return None
    if isinstance(actual, (bytes, bytearray)):
        return list(actual)
    if isinstance(actual, str):
        return list(actual.encode("utf-8"))
    if isinstance(actual, dict) and "$bytes" in actual:
        return actual["$bytes"]
    return None


# ---- op dispatch ----
async def dispatch(client, op, args):
    if op == "kv.set":
        return await client.kv_set(
            args["key"], value_to_str(args["value"]),
            args.get("ttl_seconds"), args.get("if_not_exists"),
        )
    if op == "kv.get":
        return await client.kv_get(args["key"])
    if op == "kv.set_bytes":
        return await client.kv_set_bytes(args["key"], bytes(args["value"]["$bytes"]), args.get("ttl_seconds"), args.get("if_not_exists"))
    if op == "kv.get_bytes":
        return await client.kv_get_bytes(args["key"])
    if op == "kv.exists":
        return await client.kv_exists(args["key"])
    if op == "kv.delete":
        return await client.kv_delete(args["key"])
    if op == "kv.incr":
        return await client.kv_incr(args["key"], args["by"])
    if op == "kv.compare_and_swap":
        old = None if args.get("old") is None else value_to_str(args["old"])
        return await client.kv_compare_and_swap(args["key"], old, value_to_str(args["new"]))
    if op == "kv.scan_page":
        keys, cursor = await client.kv_scan_page(args["prefix"], args.get("cursor"), args.get("limit", 100))
        return {"keys": list(keys), "cursor": cursor}
    if op == "ratelimit.check":
        d = await client.rate_limit(args["bucket"], args["key"], args["max"], args["per_seconds"], args.get("fail_open"))
        return {
            "allowed": d.allowed,
            "limit": d.limit,
            "remaining": d.remaining,
            "reset_after_seconds": d.reset_after_seconds,
            "retry_after_seconds": d.retry_after_seconds,
        }
    if op == "schedule.at":
        return await client.schedule_at(args["when_epoch_ms"], args["queue"], value_to_str(args["payload"]))
    if op == "schedule.cron":
        return await client.schedule_cron(args["name"], args["expr"], args["queue"], value_to_str(args["payload"]))
    if op == "schedule.cancel":
        return await client.schedule_cancel(args["name"])
    if op == "schedule.list":
        items = await client.schedule_list()
        return [
            {
                "name": getattr(s, "name", None),
                "kind": s.kind,
                "queue": getattr(s, "queue", None),
                "next_run_ms": s.next_run_ms,
                "last_run_ms": s.last_run_ms,
                "cron_expr": s.cron_expr,
            }
            for s in items
        ]
    if op == "queue.enqueue":
        return await client.queue_enqueue(args["queue"], value_to_str(args["payload"]), args.get("max_attempts"), args.get("dedup_id"))
    if op == "queue.dequeue":
        job = await client.queue_dequeue(args["queue"], args["visibility_seconds"], args["wait_seconds"])
        if job is None:
            return None
        return {"id": job.id, "receipt": job.receipt, "payload": job.payload, "attempt": job.attempt}
    if op == "queue.ack":
        return await client.queue_ack(args["receipt"])
    if op == "queue.nack":
        return await client.queue_nack(args["receipt"], args.get("retry_seconds"))
    if op == "queue.depth":
        visible, in_flight, delayed = await client.queue_depth(args["queue"])
        return {"visible": visible, "in_flight": in_flight, "delayed": delayed}
    if op == "config.set":
        return await client.config_set(args["key"], args["value"])
    if op == "config.get":
        return await client.config_get(args["key"])
    if op == "config.flag":
        return await client.flag(args["key"], args.get("default", False), args.get("targeting_key"))
    if op == "config.set_flag_on":
        return await client.set_flag_on(args["key"])
    if op == "config.set_flag_off":
        return await client.set_flag_off(args["key"])
    if op == "auth.create_session":
        return await client.create_session(args["user_id"], args.get("idle_seconds"), args.get("absolute_seconds"))
    if op == "auth.validate_session":
        return await client.validate_session(args["token"])
    if op == "auth.revoke_session":
        return await client.revoke_session(args["token"])
    if op == "auth.create_api_key":
        k = await client.create_api_key(args["owner_id"], args["label"])
        return {"id": k.id, "secret": k.secret, "label": k.label, "created_at_ms": k.created_at_ms}
    if op == "auth.verify_api_key":
        return await client.verify_api_key(args["key"])
    raise ValueError(f"python conformance runner has no dispatch for op {op}")


# ---- expectation checking ----
def type_matches(t, a):
    checks = {
        "string": isinstance(a, str),
        "number": isinstance(a, (int, float)) and not isinstance(a, bool),
        "boolean": isinstance(a, bool),
        "array": isinstance(a, list),
        "object": isinstance(a, dict),
        "null": a is None,
    }
    if t not in checks:
        raise ValueError(f"unknown $type matcher {t}")
    return checks[t]


def deep_match(exp, act):
    if isinstance(exp, str) and not isinstance(act, str):
        b = as_bytes(act)
        if b is not None:
            return bytes(b).decode("utf-8", "replace") == exp
    if isinstance(exp, dict):
        if isinstance(exp.get("$type"), str):
            return type_matches(exp["$type"], act)
        if isinstance(exp.get("$approx"), (int, float)):
            tol = exp.get("tol", 0)
            return isinstance(act, (int, float)) and abs(act - exp["$approx"]) <= tol
        if "$bytes" in exp:
            a = as_bytes(act)
            return a is not None and a == exp["$bytes"]
        if not isinstance(act, dict):
            return False
        return all(deep_match(v, act.get(k)) for k, v in exp.items())
    if isinstance(exp, list):
        return isinstance(act, list) and len(exp) == len(act) and all(deep_match(e, a) for e, a in zip(exp, act))
    return exp == act


def check_value(exp, act):
    if isinstance(exp, str):
        b = as_bytes(act)
        if b is not None:
            got = bytes(b).decode("utf-8", "replace")
            if got != exp:
                raise AssertionError(f"expected {exp!r}, got {got!r}")
            return
    if not deep_match(exp, act):
        raise AssertionError(f"expected {exp!r}, got {act!r}")


def check_bytes(exp, act):
    a = as_bytes(act)
    if a is None:
        raise AssertionError(f"expected a byte value, got {act!r}")
    if a != exp["$bytes"]:
        raise AssertionError(f"byte mismatch: expected {exp['$bytes']}, got {a}")


def check(expect, ok, value, code):
    if isinstance(expect.get("error"), str):
        if ok:
            raise AssertionError(f"expected error {expect['error']}, got value {value!r}")
        if code != expect["error"]:
            raise AssertionError(f"expected error {expect['error']}, got {code}")
        return
    if not ok:
        raise AssertionError(f"expected a value, got error {code}")
    if "value" in expect:
        return check_value(expect["value"], value)
    if "bytes" in expect:
        return check_bytes(expect["bytes"], value)
    if "shape" in expect:
        if not deep_match(expect["shape"], value):
            raise AssertionError(f"shape mismatch: expected {expect['shape']!r}, got {value!r}")
        return
    raise AssertionError("expect block has none of value/bytes/shape/error")


# ---- $ref ----
def resolve(v, captures):
    if isinstance(v, dict):
        if isinstance(v.get("$ref"), str):
            cur = captures
            for k in v["$ref"].split("."):
                cur = cur[k]
            return cur
        return {k: resolve(x, captures) for k, x in v.items()}
    if isinstance(v, list):
        return [resolve(x, captures) for x in v]
    return v


async def run_scenario(scenario):
    name = "forge_conf_" + uuid.uuid4().hex[:12]
    admin_exec(f'CREATE DATABASE "{name}"')
    url = swap_db(ADMIN_URL, name)
    try:
        clients = {}
        captures = {}
        for i, step in enumerate(scenario["steps"]):
            ns = step.get("namespace", "")
            if ns not in clients:
                clients[ns] = await forge_py.ForgeClient.connect_with(url, kv_namespace=ns)
            client = clients[ns]
            args = resolve(step.get("args", {}), captures)
            ok, value, code = True, None, None
            try:
                value = await dispatch(client, step["op"], args)
            except forge_py.ForgeError as e:
                ok, code = False, canonical_error_code(e)
            if step.get("as") and ok:
                captures[step["as"]] = value
            if "expect" in step:
                try:
                    check(step["expect"], ok, value, code)
                except AssertionError as e:
                    raise AssertionError(f"step {i} ({step['op']}): {e}")
            elif not ok:
                raise AssertionError(f"step {i} ({step['op']}): unexpected error {code}")
    finally:
        admin_exec(f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE)')


def load_gaps():
    doc = json.loads(GAPS_FILE.read_text())
    return {f"{g['primitive']}/{g['scenario']}" for g in doc["gaps"] if LANG in g["languages"]}


async def main():
    gaps = load_gaps()
    problems = []
    passed = 0
    for file in sorted(SCENARIO_DIR.glob("*.json")):
        doc = json.loads(file.read_text())
        for scenario in doc["scenarios"]:
            key = f"{doc['primitive']}/{scenario['name']}"
            expected_fail = key in gaps
            err = None
            try:
                await run_scenario(scenario)
            except Exception as e:  # noqa: BLE001
                err = e
            if err is None and not expected_fail:
                passed += 1
                print(f"PASS  {key}")
            elif err is not None and expected_fail:
                passed += 1
                print(f"XFAIL {key}: {err}")
            elif err is None and expected_fail:
                problems.append(f"{key}: PASSED but is a registered python gap — remove it from known_gaps.json")
            else:
                problems.append(f"{key}: {err}")
    print(f"\nconformance(python): {passed} ok, {len(problems)} unexpected")
    if problems:
        print("unexpected conformance results:\n  " + "\n  ".join(problems), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())

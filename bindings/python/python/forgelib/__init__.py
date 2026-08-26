from __future__ import annotations

import asyncio
import base64
import binascii
import datetime
import json
import math
import random
import re
import time
from dataclasses import dataclass
from typing import Any, AsyncIterator, Awaitable, Callable, Generic, Optional, TypeVar, Union

from .forgelib import *  # noqa: F401,F403

T = TypeVar("T")

Loads = Callable[[Union[str, bytes]], Any]
Dumps = Callable[[Any], Union[str, bytes]]
OnError = Callable[[BaseException, Optional["QueueJob[Any]"]], Awaitable[None]]


def forge_error_code(exc: BaseException) -> str:
    """Return the canonical Forge error code (e.g. ``"NotFound"``, ``"Limit"``) for a
    raised exception. Leaf exception classes are named code + ``Error``, so this is
    the class name with the suffix stripped; non-Forge exceptions return their
    class name unchanged."""

    name = type(exc).__name__
    if isinstance(exc, ForgeError) and name != "ForgeError" and name.endswith("Error"):
        return name.removesuffix("Error")
    return name


def forge_error_retryable(exc: BaseException) -> bool:
    """Return whether a raised Forge error is safe to retry.

    Every Forge exception carries a ``retryable`` attribute set by the core
    (``UnavailableError`` is always retryable; ``BackendError`` sometimes is)."""

    retryable = getattr(exc, "retryable", None)
    if isinstance(retryable, bool):
        return retryable
    return forge_error_code(exc) == "Unavailable"


def _decode_payload(raw: Any) -> str:
    if isinstance(raw, (bytes, bytearray)):
        return raw.decode("utf-8")
    return raw


def encode_queue_envelope(envelope: dict[str, Any]) -> bytes:
    version = envelope.get("version", 1)
    schema = envelope.get("schema")
    content_type = envelope.get("content_type")
    artifacts = envelope.get("artifacts", [])
    if version != 1 or not isinstance(schema, str) or not schema or not isinstance(content_type, str) or not content_type:
        raise ValueError("version 1, schema, and content_type are required")
    if len(schema.encode()) > 256 or len(content_type.encode()) > 128 or len(str(envelope.get("correlation_id", "")).encode()) > 256 or len(artifacts) > 32:
        raise ValueError("queue envelope metadata exceeds its limit")
    for artifact in artifacts:
        if not isinstance(artifact, dict) or not artifact.get("uri"):
            raise ValueError("artifact uri must not be empty")
        if len(str(artifact["uri"]).encode()) > 2048 or len(str(artifact.get("content_type", "")).encode()) > 128 or len(str(artifact.get("version", "")).encode()) > 256:
            raise ValueError("artifact metadata exceeds its limit")
    trace = envelope.get("trace_context")
    if trace is not None:
        import re
        traceparent = trace.get("traceparent", "") if isinstance(trace, dict) else ""
        tracestate = trace.get("tracestate", "") if isinstance(trace, dict) else ""
        baggage = trace.get("baggage", "") if isinstance(trace, dict) else ""
        valid_traceparent = bool(re.fullmatch(r"[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}", traceparent)) and not traceparent.startswith("ff-") and traceparent[3:35] != "0" * 32 and traceparent[36:52] != "0" * 16
        if not valid_traceparent or len(traceparent.encode()) > 512 or len(tracestate.encode()) > 512 or len(baggage.encode()) > 1024 or "\r" in tracestate + baggage or "\n" in tracestate + baggage or len([item for item in baggage.split(",") if item]) > 16:
            raise ValueError("queue envelope trace context is invalid")
    body = bytes(envelope.get("body", b""))
    wire = {"version": version, "schema": schema, "content_type": content_type, "body": list(body)}
    for key in ("correlation_id", "trace_context", "artifacts"):
        if envelope.get(key):
            wire[key] = envelope[key]
    encoded = json.dumps(wire, separators=(",", ":")).encode()
    if len(encoded) > 256 * 1024:
        raise ValueError("encoded envelope exceeds 256 KiB; use blob references for large bodies")
    return encoded


def decode_queue_envelope(encoded: bytes) -> dict[str, Any]:
    if len(encoded) > 256 * 1024:
        raise ValueError("encoded envelope exceeds 256 KiB")
    value = json.loads(encoded)
    value["body"] = bytes(value.get("body", []))
    encode_queue_envelope(value)
    return value


def _validate_invalidation_value(value: Any, depth: int, count: list[int]) -> None:
    count[0] += 1
    if count[0] > 32:
        raise ValueError("query-key fragment has too many nodes")
    if value is None or isinstance(value, (bool, int)):
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError("query-key numbers must be finite")
        return
    if isinstance(value, str):
        if len(value.encode()) > 128:
            raise ValueError("query-key string exceeds 128 bytes")
        return
    if depth >= 3:
        raise ValueError("query-key nesting exceeds 3 levels")
    if isinstance(value, list):
        if len(value) > 16:
            raise ValueError("query-key array has too many items")
        for item in value:
            _validate_invalidation_value(item, depth + 1, count)
        return
    if isinstance(value, dict):
        if len(value) > 16:
            raise ValueError("query-key object has too many items")
        for key, item in value.items():
            if not isinstance(key, str) or len(key.encode()) > 64:
                raise ValueError("query-key object keys must be strings of at most 64 bytes")
            _validate_invalidation_value(item, depth + 1, count)
        return
    raise ValueError("query-key parts must be JSON values")


def _normalize_invalidation_event(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise ValueError("unsupported invalidation schema version")
    tags = value.get("tags", [])
    query_keys = value.get("query_keys", [])
    if not isinstance(tags, list) or not isinstance(query_keys, list) or not tags and not query_keys:
        raise ValueError("invalidation event requires a target")
    if len(tags) > 32 or len(query_keys) > 32 or len(tags) + len(query_keys) > 64:
        raise ValueError("invalidation event has too many targets")
    if any(not isinstance(tag, str) or not tag or len(tag.encode()) > 128 for tag in tags):
        raise ValueError("invalidation tags must be 1..=128 UTF-8 bytes")
    if len(set(tags)) != len(tags):
        raise ValueError("invalidation tags must be unique")
    for query_key in query_keys:
        if not isinstance(query_key, list) or not query_key or len(query_key) > 8:
            raise ValueError("query-key fragments must contain 1..=8 parts")
        count = [0]
        for part in query_key:
            _validate_invalidation_value(part, 1, count)
    revision = value.get("revision")
    if revision is not None and (not isinstance(revision, str) or not revision or len(revision.encode()) > 256):
        raise ValueError("invalidation revision must be 1..=256 UTF-8 bytes")
    event: dict[str, Any] = {
        "schema_version": 1,
        "tags": list(tags),
        "query_keys": json.loads(json.dumps(query_keys, allow_nan=False)),
    }
    if revision is not None:
        event["revision"] = revision
    encoded = json.dumps(event, separators=(",", ":"), allow_nan=False).encode()
    if len(encoded) > 4096:
        raise ValueError("invalidation event exceeds 4096 bytes")
    return event


def encode_invalidation_event(event: dict[str, Any]) -> bytes:
    """Validate and encode a version-1 lossy invalidation hint."""
    normalized = _normalize_invalidation_event(event)
    return json.dumps(normalized, separators=(",", ":"), allow_nan=False).encode()


def decode_invalidation_event(encoded: str | bytes) -> dict[str, Any]:
    """Decode a bounded hint and discard unknown additive version-1 fields."""
    raw = encoded.encode() if isinstance(encoded, str) else bytes(encoded)
    if len(raw) > 4096:
        raise ValueError("invalidation event exceeds 4096 bytes")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("invalidation event must be valid JSON") from exc
    return _normalize_invalidation_event(value)


_CLOUD_EVENT_MAX_BYTES = 1024 * 1024
_CLOUD_EVENT_RESERVED = {
    "specversion", "id", "source", "type", "datacontenttype", "dataschema",
    "subject", "time", "data", "data_base64", "dataref", "dataref_base64",
}


def _valid_cloud_event_string(value: Any) -> bool:
    return isinstance(value, str) and bool(value) and not any(
        ord(char) <= 0x1F or 0x7F <= ord(char) <= 0x9F for char in value
    )


def _validate_cloud_event_extension(name: str, value: Any) -> None:
    if not re.fullmatch(r"[a-z0-9]+", name) or name in _CLOUD_EVENT_RESERVED:
        raise ValueError(
            "CloudEvents extension names must be lowercase alphanumeric and non-reserved"
        )
    if value is None or isinstance(value, (bool, str)):
        return
    if isinstance(value, int) and not isinstance(value, bool) and -(2**31) <= value < 2**31:
        return
    raise ValueError(
        "CloudEvents extension values must be null, boolean, 32-bit integer, or string"
    )


def _is_json_content_type(content_type: Optional[str]) -> bool:
    if content_type is None:
        return True
    media_type = content_type.split(";", 1)[0].strip().lower()
    _, separator, subtype = media_type.partition("/")
    return bool(separator) and (subtype == "json" or subtype.endswith("+json"))


def _normalize_cloud_event(event: Any) -> dict[str, Any]:
    if not isinstance(event, dict):
        raise ValueError("CloudEvent must be an object")
    for name in ("id", "source", "type"):
        if not _valid_cloud_event_string(event.get(name)):
            raise ValueError(f"CloudEvent {name} is empty or contains control characters")
    for name in ("subject", "datacontenttype", "dataschema"):
        if name in event and not _valid_cloud_event_string(event[name]):
            raise ValueError(f"CloudEvent {name} is empty or contains control characters")
    if "time" in event:
        value = event["time"]
        if not _valid_cloud_event_string(value) or not re.fullmatch(
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})",
            value,
        ):
            raise ValueError("CloudEvents time must be RFC 3339")
        try:
            datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError as exc:
            raise ValueError("CloudEvents time must be RFC 3339") from exc
    if "data" in event and not isinstance(event["data"], (bytes, bytearray, memoryview)):
        raise ValueError("CloudEvent data must be bytes")
    extensions = event.get("extensions", {})
    if not isinstance(extensions, dict) or len(extensions) > 64:
        raise ValueError("CloudEvent extensions are invalid")
    for name, value in extensions.items():
        if not isinstance(name, str):
            raise ValueError("CloudEvent extension names must be strings")
        _validate_cloud_event_extension(name, value)
    normalized: dict[str, Any] = {
        "id": event["id"],
        "source": event["source"],
        "type": event["type"],
        "extensions": dict(extensions),
    }
    for name in ("subject", "time", "datacontenttype", "dataschema"):
        if name in event:
            normalized[name] = event[name]
    if "data" in event:
        normalized["data"] = bytes(event["data"])
    return normalized


def encode_cloud_event(event: dict[str, Any]) -> bytes:
    """Encode CloudEvents 1.0 structured JSON with binary-safe ``data_base64``."""
    normalized = _normalize_cloud_event(event)
    envelope: dict[str, Any] = {
        "specversion": "1.0",
        "id": normalized["id"],
        "source": normalized["source"],
        "type": normalized["type"],
    }
    for name in ("subject", "time", "datacontenttype", "dataschema"):
        if name in normalized:
            envelope[name] = normalized[name]
    envelope.update(normalized["extensions"])
    if "data" in normalized:
        envelope["data_base64"] = base64.b64encode(normalized["data"]).decode("ascii")
    encoded = json.dumps(envelope, separators=(",", ":"), allow_nan=False).encode()
    if len(encoded) > _CLOUD_EVENT_MAX_BYTES:
        raise ValueError("CloudEvent exceeds 1 MiB")
    return encoded


def decode_cloud_event(encoded: str | bytes) -> dict[str, Any]:
    """Decode bounded CloudEvents 1.0 structured JSON into binary-safe data."""
    raw = encoded.encode() if isinstance(encoded, str) else bytes(encoded)
    if len(raw) > _CLOUD_EVENT_MAX_BYTES:
        raise ValueError("CloudEvent exceeds 1 MiB")
    try:
        envelope = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("CloudEvent must be valid JSON") from exc
    if not isinstance(envelope, dict):
        raise ValueError("CloudEvent must be a JSON object")
    if envelope.get("specversion") != "1.0":
        raise ValueError("unsupported CloudEvents specversion")
    if "data" in envelope and "data_base64" in envelope:
        raise ValueError("CloudEvent data and data_base64 are mutually exclusive")
    event: dict[str, Any] = {
        "id": envelope.get("id"),
        "source": envelope.get("source"),
        "type": envelope.get("type"),
    }
    for name in ("subject", "time", "datacontenttype", "dataschema"):
        if envelope.get(name) is not None:
            event[name] = envelope[name]
    if "data_base64" in envelope:
        value = envelope["data_base64"]
        if not isinstance(value, str):
            raise ValueError("CloudEvent data_base64 must be a string")
        try:
            event["data"] = base64.b64decode(value, validate=True)
        except (ValueError, binascii.Error) as exc:
            raise ValueError("CloudEvent data_base64 is invalid") from exc
    elif "data" in envelope:
        if _is_json_content_type(event.get("datacontenttype")):
            event.setdefault("datacontenttype", "application/json")
            event["data"] = json.dumps(
                envelope["data"], separators=(",", ":"), allow_nan=False
            ).encode()
        elif isinstance(envelope["data"], str):
            event["data"] = envelope["data"].encode()
        else:
            raise ValueError("non-JSON CloudEvent data must be a string")
    known = {
        "specversion", "id", "source", "type", "subject", "time",
        "datacontenttype", "dataschema", "data", "data_base64",
    }
    event["extensions"] = {
        name: value for name, value in envelope.items() if name not in known
    }
    return _normalize_cloud_event(event)


def _validate_scope_component(label: str, value: str) -> str:
    if not isinstance(value, str) or not 1 <= len(value.encode()) <= 255:
        raise _scope_error(
            InvalidError, "Invalid", f"scope {label} must contain 1 to 255 bytes"
        )
    if any(ord(character) < 32 or 127 <= ord(character) <= 159 for character in value):
        raise _scope_error(
            InvalidError, "Invalid", f"scope {label} must not contain control characters"
        )
    return value


def _scope_error(
    error_type: type[ForgeError],
    code: str,
    message: str,
    operation: str = "scope",
) -> ForgeError:
    error = error_type(message)
    error.code = code
    error.retryable = False
    error.operation = operation
    error.backend = None
    error.safe_message = message
    return error


def _render_scoped_name(kind: str, parts: tuple[str, str, str, str]) -> str:
    parts = tuple(
        _validate_scope_component(label, part)
        for label, part in zip(("application", "tenant", "user", "resource"), parts)
    )
    value = f"v1|{kind}|" + "".join(f"{len(part.encode())}:{part}" for part in parts)
    if len(value.encode()) > (895 if kind == "blob" else 383):
        raise _scope_error(
            LimitError, "Limit", f"scoped {kind} name exceeds its backend-safe length"
        )
    return value


def scope_kv_key(application: str, tenant: str, user: str, resource: str) -> str:
    return _render_scoped_name("kv", (application, tenant, user, resource))


def scope_blob_key(application: str, tenant: str, user: str, resource: str) -> str:
    return _render_scoped_name("blob", (application, tenant, user, resource))


def scope_rate_limit_subject(application: str, tenant: str, user: str, resource: str) -> str:
    return _render_scoped_name("rate", (application, tenant, user, resource))


def scope_topic(application: str, tenant: str, user: str, resource: str) -> str:
    return _render_scoped_name("topic", (application, tenant, user, resource))


def parse_scoped_name(value: str) -> dict[str, str]:
    if not isinstance(value, str) or not value.startswith("v1|"):
        raise _scope_error(
            InvalidError, "Invalid", "scoped name must use v1", "scope.parse"
        )
    try:
        kind, encoded_text = value[3:].split("|", 1)
    except ValueError as exc:
        raise _scope_error(
            InvalidError, "Invalid", "scoped name is malformed", "scope.parse"
        ) from exc
    budget = 895 if kind == "blob" else 383 if kind in {"kv", "rate", "topic"} else 0
    if budget == 0:
        raise _scope_error(
            InvalidError, "Invalid", "scoped name kind is unknown", "scope.parse"
        )
    encoded = encoded_text.encode()
    parts: list[str] = []
    offset = 0
    for label in ("application", "tenant", "user", "resource"):
        colon = encoded.find(b":", offset)
        if colon < 0:
            raise _scope_error(
                InvalidError, "Invalid", "scoped name is malformed", "scope.parse"
            )
        length_bytes = encoded[offset:colon]
        if not length_bytes or not all(48 <= byte <= 57 for byte in length_bytes):
            raise _scope_error(
                InvalidError,
                "Invalid",
                "scoped name length is malformed",
                "scope.parse",
            )
        length = int(length_bytes)
        end = colon + 1 + length
        try:
            part = encoded[colon + 1:end].decode()
        except UnicodeDecodeError as exc:
            raise _scope_error(
                InvalidError,
                "Invalid",
                "scoped name component length is invalid",
                "scope.parse",
            ) from exc
        if end > len(encoded) or len(part.encode()) != length:
            raise _scope_error(
                InvalidError,
                "Invalid",
                "scoped name component length is invalid",
                "scope.parse",
            )
        parts.append(_validate_scope_component(label, part))
        offset = end
    if offset != len(encoded):
        raise _scope_error(
            InvalidError, "Invalid", "scoped name has trailing data", "scope.parse"
        )
    if len(value.encode()) > budget:
        raise _scope_error(
            LimitError,
            "Limit",
            f"scoped {kind} name exceeds its backend-safe length",
            "scope.parse",
        )
    application, tenant, user, resource = parts
    return {"kind": kind, "application": application, "tenant": tenant, "user": user, "resource": resource}


def _validate_env_mappings(mappings: list[dict[str, Any]]) -> None:
    if len(mappings) > 256:
        raise ValueError("environment mapping must contain at most 256 keys")
    keys: set[str] = set()
    names: set[str] = set()
    for mapping in mappings:
        key = mapping.get("key") if isinstance(mapping, dict) else None
        aliases = mapping.get("names") if isinstance(mapping, dict) else None
        if (
            not isinstance(key, str)
            or not key
            or len(key.encode()) > 256
            or key in keys
        ):
            raise ValueError("environment mapping keys must be unique 1..=256-byte strings")
        keys.add(key)
        if not isinstance(aliases, list) or not 1 <= len(aliases) <= 16:
            raise ValueError("environment mapping requires 1..=16 aliases per key")
        for name in aliases:
            if (
                not isinstance(name, str)
                or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name)
                or name in names
            ):
                raise ValueError("environment aliases must be valid and unique")
            names.add(name)


def import_env_config(
    environment: dict[str, str], mappings: list[dict[str, Any]]
) -> dict[str, str]:
    """Translate an explicit environment snapshot to logical config keys."""
    _validate_env_mappings(mappings)
    imported: dict[str, str] = {}
    for mapping in mappings:
        values = [environment[name] for name in mapping["names"] if name in environment]
        if not values:
            continue
        if any(not isinstance(value, str) or value != values[0] for value in values):
            raise ValueError(f"environment aliases for {mapping['key']} conflict")
        if len(values[0].encode()) > 65536:
            raise ValueError("environment config value exceeds 64 KiB")
        imported[mapping["key"]] = values[0]
    return imported


def export_env_config(
    config: dict[str, str], mappings: list[dict[str, Any]]
) -> dict[str, str]:
    """Translate logical config to each mapping's first environment name."""
    _validate_env_mappings(mappings)
    exported: dict[str, str] = {}
    for mapping in mappings:
        if mapping["key"] not in config:
            continue
        value = config[mapping["key"]]
        if not isinstance(value, str):
            raise ValueError("environment config values must be strings")
        if len(value.encode()) > 65536:
            raise ValueError("environment config value exceeds 64 KiB")
        exported[mapping["names"][0]] = value
    return exported


def config_snapshot_get(snapshot: Any, key: str, now_ms: Optional[float] = None) -> Optional[str]:
    """Read one captured config value, rejecting stale and out-of-scope snapshots."""
    current = time.time() * 1000 if now_ms is None else now_ms
    if current > snapshot.expires_at_ms:
        raise ValueError("config snapshot is stale")
    for entry in snapshot.config:
        if entry.key == key:
            return entry.value
    raise ValueError("config key was not included in the snapshot")


def config_snapshot_flag_details(snapshot: Any, request_id: str, now_ms: Optional[float] = None) -> Any:
    """Read one pre-evaluated flag result without re-evaluating it offline."""
    current = time.time() * 1000 if now_ms is None else now_ms
    if current > snapshot.expires_at_ms:
        raise ValueError("config snapshot is stale")
    for entry in snapshot.flags:
        if entry.id == request_id:
            return entry.evaluation
    raise ValueError("flag request id was not included in the snapshot")


def bytes_dumps(value: Any) -> bytes:
    return bytes(value)


def bytes_loads(value: str | bytes) -> bytes:
    return value.encode() if isinstance(value, str) else bytes(value)


def _job_trace_context(job: Any) -> Optional[dict[str, str]]:
    traceparent = getattr(job, "traceparent", None)
    if traceparent is None:
        return None
    return {
        key: value
        for key, value in {
            "traceparent": traceparent,
            "tracestate": getattr(job, "tracestate", None),
            "baggage": getattr(job, "baggage", None),
        }.items()
        if value is not None
    }


@dataclass
class QueueJob(Generic[T]):
    """A dequeued job whose payload has already been decoded."""

    id: str
    receipt: str
    attempt: int
    max_attempts: int
    leased_until_ms: float
    queue: str
    payload: T
    trace_context: Optional[dict[str, str]] = None
    lease_lost: Optional[asyncio.Event] = None
    cancelled: Optional[asyncio.Event] = None


class Queue(Generic[T]):
    """A queue handle bound to a JSON payload type."""

    def __init__(
        self,
        client: Any,
        name: str,
        *,
        loads: Loads = json.loads,
        dumps: Dumps = json.dumps,
    ) -> None:
        self._c = client
        self._name = name
        self._loads = loads
        self._dumps = dumps

    async def enqueue(
        self,
        payload: T,
        *,
        max_attempts: Optional[int] = None,
        dedup_id: Optional[str] = None,
        delay_seconds: Optional[float] = None,
        job_id: Optional[str] = None,
        trace_context: Optional[dict[str, str]] = None,
        baggage_allowlist: Optional[list[str]] = None,
        priority: Optional[str] = None,
        concurrency_key: Optional[str] = None,
    ) -> str:
        encoded = self._dumps(payload)
        payload_bytes = encoded.encode("utf-8") if isinstance(encoded, str) else bytes(encoded)
        return await self._c.queue_enqueue(
            self._name,
            payload_bytes,
            max_attempts,
            dedup_id,
            delay_seconds,
            job_id,
            trace_context.get("traceparent") if trace_context else None,
            trace_context.get("tracestate") if trace_context else None,
            trace_context.get("baggage") if trace_context else None,
            baggage_allowlist,
            priority,
            concurrency_key,
        )

    async def enqueue_batch(
        self, items: list[tuple[T, Optional[str]]]
    ) -> list[BatchEnqueueResult]:
        encoded_items = []
        for payload, job_id in items:
            encoded = self._dumps(payload)
            payload_bytes = encoded.encode("utf-8") if isinstance(encoded, str) else bytes(encoded)
            encoded_items.append((payload_bytes, job_id))
        return await self._c.queue_enqueue_batch(self._name, encoded_items)

    async def dequeue(
        self, *, visibility_seconds: float = 30.0, wait_seconds: float = 20.0, concurrency_limit_per_key: Optional[int] = None
    ) -> Optional[QueueJob[T]]:
        job = await self._c.queue_dequeue(self._name, visibility_seconds, wait_seconds, concurrency_limit_per_key)
        if job is None:
            return None
        return QueueJob(
            id=job.id,
            receipt=job.receipt,
            attempt=job.attempt,
            max_attempts=job.max_attempts,
            leased_until_ms=job.leased_until_ms,
            queue=job.queue,
            payload=self._loads(job.payload),
            trace_context=_job_trace_context(job),
        )

    async def dequeue_batch(
        self,
        max_items: int,
        *,
        visibility_seconds: float = 30.0,
        wait_seconds: float = 20.0,
        concurrency_limit_per_key: Optional[int] = None,
    ) -> list[QueueJob[T]]:
        jobs = await self._c.queue_dequeue_batch(
            self._name,
            max_items,
            visibility_seconds,
            wait_seconds,
            concurrency_limit_per_key,
        )
        return [
            QueueJob(
                id=job.id,
                receipt=job.receipt,
                attempt=job.attempt,
                max_attempts=job.max_attempts,
                leased_until_ms=job.leased_until_ms,
                queue=job.queue,
                payload=self._loads(job.payload),
                trace_context=_job_trace_context(job),
            )
            for job in jobs
        ]

    async def pause(self) -> None:
        await self._c.queue_pause(self._name)

    async def resume(self) -> None:
        await self._c.queue_resume(self._name)

    async def is_paused(self) -> bool:
        return await self._c.queue_is_paused(self._name)

    async def stats(self) -> QueueStats:
        return await self._c.queue_stats(self._name)

    async def ack(self, receipt: str) -> None:
        await self._c.queue_ack(receipt)

    async def nack(
        self,
        receipt: str,
        retry_seconds: Optional[float] = None,
        failure_summary: Optional[str] = None,
    ) -> None:
        await self._c.queue_nack(receipt, retry_seconds, failure_summary)

    async def heartbeat(self, receipt: str) -> None:
        await self._c.queue_heartbeat(receipt)

    async def depth(self) -> Any:
        return await self._c.queue_depth(self._name)

    async def cancel(self, job_id: str) -> Optional[dict[str, Any]]:
        value = await self._c.queue_cancel(job_id)
        return None if value is None else json.loads(value)

    async def status(self, job_id: str) -> Optional[dict[str, Any]]:
        value = await self._c.queue_status(job_id)
        return None if value is None else json.loads(value)

    async def statuses(self, *, states: Optional[list[str]] = None, cursor: Optional[str] = None, limit: int = 50) -> dict[str, Any]:
        return json.loads(await self._c.queue_list_status(self._name, states, cursor, limit))

    async def dead_letters(self, cursor: Optional[str] = None, limit: int = 50) -> Any:
        return await self._c.queue_dead_letters(self._name, cursor, limit)

    async def redrive(
        self, job_id: str, *, destination: str, dedup_policy: str
    ) -> bool:
        return await self._c.queue_redrive(job_id, destination, dedup_policy)

    async def redrive_batch(
        self,
        *,
        destination: str,
        dedup_policy: str,
        cursor: Optional[str] = None,
        limit: int = 50,
    ) -> Any:
        return await self._c.queue_redrive_batch(
            self._name, destination, dedup_policy, cursor, limit
        )

    async def purge_dry_run(self) -> int:
        return await self._c.queue_purge_dead_letters_dry_run(self._name)

    async def purge(self, confirmation: str) -> int:
        return await self._c.queue_purge_dead_letters(self._name, confirmation)

    async def worker(
        self,
        handler: Callable[[QueueJob[T]], Awaitable[None]],
        *,
        visibility_seconds: float = 30.0,
        wait_seconds: float = 20.0,
        stop: Optional[asyncio.Event] = None,
        on_error: Optional[OnError] = None,
        concurrency: int = 1,
        heartbeat_seconds: Optional[float] = None,
        retry_backoff_seconds: float = 0.25,
        drain_deadline_seconds: float = 30.0,
        identity: str = "worker",
        concurrency_limit_per_key: Optional[int] = None,
    ) -> None:
        await run_worker(
            self._c,
            self._name,
            handler,
            visibility_seconds=visibility_seconds,
            wait_seconds=wait_seconds,
            stop=stop,
            loads=self._loads,
            on_error=on_error,
            concurrency=concurrency,
            heartbeat_seconds=heartbeat_seconds,
            retry_backoff_seconds=retry_backoff_seconds,
            drain_deadline_seconds=drain_deadline_seconds,
            identity=identity,
            concurrency_limit_per_key=concurrency_limit_per_key,
        )


class RateLimiter:
    """One abstract-unit budget for a bucket and subject."""

    def __init__(self, client: Any, bucket: str, subject: str) -> None:
        self._c, self._bucket, self._subject = client, bucket, subject

    async def check(self, *, max: int, per_seconds: float, cost: int = 1, algo: Optional[str] = None, fail_open: Optional[bool] = None) -> Any:
        return await self._c.rate_limit_check(self._bucket, self._subject, max, per_seconds, fail_open, algo, cost)

    async def reserve(self, *, max: int, per_seconds: float, cost: int, ttl_seconds: float, algo: Optional[str] = None) -> Optional[dict[str, Any]]:
        value = await self._c.rate_limit_reserve(self._bucket, self._subject, max, per_seconds, cost, ttl_seconds, algo)
        return None if value is None else json.loads(value)

    async def commit(self, reservation_id: str, actual_units: int) -> dict[str, Any]:
        return json.loads(await self._c.rate_limit_commit(reservation_id, actual_units))

    async def release(self, reservation_id: str) -> dict[str, Any]:
        return json.loads(await self._c.rate_limit_release(reservation_id))


class KvKey(Generic[T]):
    """A key/value handle bound to a JSON value type."""

    def __init__(
        self,
        client: Any,
        key: str,
        *,
        loads: Loads = json.loads,
        dumps: Dumps = json.dumps,
    ) -> None:
        self._c = client
        self._key = key
        self._loads = loads
        self._dumps = dumps

    async def get(self) -> Optional[T]:
        raw = await self._c.kv_get(self._key)
        return None if raw is None else self._loads(raw)

    async def get_or_default(self, default: T) -> T:
        value = await self.get()
        return default if value is None else value

    async def set(
        self,
        value: T,
        *,
        ttl_seconds: Optional[float] = None,
        if_not_exists: Optional[bool] = None,
        if_exists: Optional[bool] = None,
    ) -> bool:
        return await self._c.kv_set(
            self._key, self._dumps(value), ttl_seconds, if_not_exists, if_exists
        )

    async def delete(self) -> bool:
        return await self._c.kv_delete(self._key)

    async def exists(self) -> bool:
        return await self._c.kv_exists(self._key)

    async def expire(self, ttl_seconds: float) -> bool:
        return await self._c.kv_expire(self._key, ttl_seconds)

    async def compare_and_swap(
        self, old: Optional[T], new_value: T
    ) -> bool:
        old_raw = None if old is None else self._dumps(old)
        return await self._c.kv_compare_and_swap(self._key, old_raw, self._dumps(new_value))


class ConfigKey(Generic[T]):
    """A config key bound to a JSON value type and default."""

    def __init__(
        self,
        client: Any,
        key: str,
        default: T,
        *,
        loads: Loads = json.loads,
        dumps: Dumps = json.dumps,
    ) -> None:
        self._c = client
        self._key = key
        self._default = default
        self._loads = loads
        self._dumps = dumps

    async def get(self) -> Optional[T]:
        raw = await self._c.config_get(self._key)
        return None if raw is None else self._loads(raw)

    async def get_or_default(self) -> T:
        value = await self.get()
        return self._default if value is None else value

    async def set(self, value: T) -> None:
        await self._c.config_set(self._key, self._dumps(value))

    async def delete(self) -> bool:
        return await self._c.config_delete(self._key)

    async def flag(self, targeting_key: Optional[str] = None) -> bool:
        return await self._c.flag(self._key, bool(self._default), targeting_key)


class Topic(Generic[T]):
    """A pub/sub topic bound to a JSON event type."""

    def __init__(
        self,
        client: Any,
        topic: str,
        *,
        loads: Loads = json.loads,
        dumps: Dumps = json.dumps,
    ) -> None:
        self._c = client
        self._topic = topic
        self._loads = loads
        self._dumps = dumps

    async def publish(self, event: T) -> None:
        await self._c.pubsub_publish(self._topic, self._dumps(event))

    async def subscribe(self) -> AsyncIterator[T]:
        sub = await self._c.pubsub_subscribe(self._topic)
        try:
            async for payload in sub:
                yield self._loads(_decode_payload(payload))
        finally:
            await sub.aclose()

    def channel(self) -> str:
        return self._c.pubsub_channel(self._topic)


async def _run_worker_loop(
    client: Any,
    queue_name: str,
    handler: Callable[[QueueJob[Any]], Awaitable[None]],
    *,
    visibility_seconds: float = 30.0,
    wait_seconds: float = 20.0,
    stop: Optional[asyncio.Event] = None,
    loads: Loads = json.loads,
    on_error: Optional[OnError] = None,
    concurrency: int = 1,
    heartbeat_seconds: Optional[float] = None,
    retry_backoff_seconds: float = 0.25,
    drain_deadline_seconds: float = 30.0,
    identity: str = "worker",
    concurrency_limit_per_key: Optional[int] = None,
    force_stop: Optional[asyncio.Event] = None,
) -> None:
    """Run a managed worker loop for a JSON queue. Set ``stop`` to drain.

    ``on_error`` is awaited with (exception, job) for every failure the loop
    absorbs — dequeue errors, undecodable payloads (job is None for both), and
    handler/ack failures — so failures are observable instead of silent.
    """

    async def report(
        exc: BaseException, job: Optional[QueueJob[Any]], state: str
    ) -> None:
        try:
            setattr(exc, "worker_identity", identity)
            setattr(exc, "worker_state", state)
        except Exception:  # noqa: BLE001
            pass
        if on_error is not None:
            await on_error(exc, job)

    if concurrency < 1:
        raise ValueError("concurrency must be positive")
    hb_every = heartbeat_seconds if heartbeat_seconds is not None else max(0.001, visibility_seconds / 3.0)
    if hb_every <= 0 or hb_every >= visibility_seconds:
        raise ValueError("heartbeat_seconds must be positive and shorter than visibility_seconds")
    retry_backoff_seconds = min(30.0, max(0.0, retry_backoff_seconds))
    drain_deadline_seconds = max(0.0, drain_deadline_seconds)

    async def process(raw: Any) -> None:
        lease_lost = asyncio.Event()
        try:
            payload = loads(raw.payload)
        except Exception as exc:  # bad payload; let retries/DLQ handle it  # noqa: BLE001
            try:
                await client.queue_nack(raw.receipt, None, "payload could not be decoded")
            except Exception:  # noqa: BLE001
                pass
            await report(exc, None, "decode")
            return

        job: QueueJob[Any] = QueueJob(
            id=raw.id,
            receipt=raw.receipt,
            attempt=raw.attempt,
            max_attempts=raw.max_attempts,
            leased_until_ms=raw.leased_until_ms,
            queue=raw.queue,
            payload=payload,
            trace_context=_job_trace_context(raw),
            lease_lost=lease_lost,
            cancelled=asyncio.Event(),
        )
        setattr(job, "worker_identity", identity)

        async def _beat(receipt: str) -> Optional[BaseException]:
            while not lease_lost.is_set():
                await asyncio.sleep(hb_every)
                try:
                    if await client.queue_cancellation_requested(receipt):
                        assert job.cancelled is not None
                        job.cancelled.set()
                        return asyncio.CancelledError("job cancellation requested")
                    await client.queue_heartbeat(receipt)
                except Exception as exc:  # lease lost; stop heartbeating  # noqa: BLE001
                    lease_lost.set()
                    await report(exc, job, "heartbeating")
                    return exc
            return None

        beat = asyncio.create_task(_beat(job.receipt))
        handled = asyncio.create_task(handler(job))
        try:
            done, _ = await asyncio.wait({handled, beat}, return_when=asyncio.FIRST_COMPLETED)
            if beat in done and not handled.done() and lease_lost.is_set():
                assert job.cancelled is not None
                job.cancelled.set()
                handled.cancel()
                await asyncio.gather(handled, return_exceptions=True)
                return
            if beat in done and isinstance(beat.result(), asyncio.CancelledError):
                handled.cancel()
                await asyncio.gather(handled, return_exceptions=True)
                try:
                    await client.queue_finish_cancellation(job.receipt)
                except Exception as exc:  # noqa: BLE001
                    await report(exc, job, "settling")
                return
            await handled
            beat.cancel()
            if not lease_lost.is_set():
                try:
                    await client.queue_ack(job.receipt)
                except Exception as exc:  # noqa: BLE001
                    await report(exc, job, "settling")
        except asyncio.CancelledError:
            assert job.cancelled is not None
            job.cancelled.set()
            handled.cancel()
            await asyncio.gather(handled, return_exceptions=True)
            if not lease_lost.is_set():
                try:
                    await asyncio.shield(client.queue_nack(job.receipt, 0.0, "worker shutdown interrupted the handler"))
                except Exception as exc:  # noqa: BLE001
                    await report(exc, job, "settling")
            raise
        except Exception as exc:  # noqa: BLE001
            beat.cancel()
            if not lease_lost.is_set():
                try:
                    await client.queue_nack(job.receipt, None, "handler returned an error")
                except Exception:  # noqa: BLE001
                    pass
            await report(exc, job, "handling")
        finally:
            beat.cancel()
            await asyncio.gather(beat, return_exceptions=True)

    active: set[asyncio.Task[None]] = set()
    retry_attempt = 0
    try:
        while not (stop is not None and stop.is_set()):
            if len(active) >= concurrency:
                stop_task = asyncio.create_task(stop.wait()) if stop is not None else None
                waiting = set(active)
                if stop_task is not None:
                    waiting.add(stop_task)
                done, _ = await asyncio.wait(waiting, return_when=asyncio.FIRST_COMPLETED)
                if stop_task is not None:
                    if stop_task in done:
                        break
                    stop_task.cancel()
                    await asyncio.gather(stop_task, return_exceptions=True)
                active = {task for task in active if not task.done()}
                continue
            try:
                raw = await client.queue_dequeue(queue_name, visibility_seconds, wait_seconds, concurrency_limit_per_key)
                retry_attempt = 0
            except Exception as exc:  # transient backend blip  # noqa: BLE001
                await report(exc, None, "polling")
                if not forge_error_retryable(exc):
                    raise
                retry_attempt += 1
                base = min(30.0, retry_backoff_seconds * (2 ** min(retry_attempt, 5)))
                delay = base * random.uniform(0.8, 1.2)
                if stop is None:
                    await asyncio.sleep(delay)
                else:
                    try:
                        await asyncio.wait_for(stop.wait(), timeout=delay)
                    except asyncio.TimeoutError:
                        pass
                continue
            if raw is None:
                continue
            if stop is not None and stop.is_set():
                try:
                    await client.queue_nack(raw.receipt, 0.0)
                except Exception as exc:  # noqa: BLE001
                    await report(exc, None, "settling")
                break
            task = asyncio.create_task(process(raw))
            active.add(task)
            task.add_done_callback(active.discard)

        if active:
            if force_stop is not None and force_stop.is_set():
                for task in active:
                    task.cancel()
                await asyncio.gather(*active, return_exceptions=True)
                return
            force_task = asyncio.create_task(force_stop.wait()) if force_stop is not None else None
            drained = asyncio.ensure_future(asyncio.gather(*active, return_exceptions=True))
            waiting: set[asyncio.Future[Any]] = {drained}
            if force_task is not None:
                waiting.add(force_task)
            done, _ = await asyncio.wait(
                waiting,
                timeout=drain_deadline_seconds,
                return_when=asyncio.FIRST_COMPLETED,
            )
            drained_cleanly = drained in done
            forced = force_task is not None and force_task in done
            if force_task is not None and not force_task.done():
                force_task.cancel()
                await asyncio.gather(force_task, return_exceptions=True)
            if not drained_cleanly or forced:
                for task in active:
                    task.cancel()
            await asyncio.gather(*active, return_exceptions=True)
    except asyncio.CancelledError:
        for task in active:
            task.cancel()
        await asyncio.gather(*active, return_exceptions=True)
        raise


_worker_states: dict[Any, tuple[set[asyncio.Event], set[asyncio.Task[Any]]]] = {}


async def run_worker(
    client: Any,
    queue_name: str,
    handler: Callable[[QueueJob[Any]], Awaitable[None]],
    *,
    visibility_seconds: float = 30.0,
    wait_seconds: float = 20.0,
    stop: Optional[asyncio.Event] = None,
    loads: Loads = json.loads,
    on_error: Optional[OnError] = None,
    concurrency: int = 1,
    heartbeat_seconds: Optional[float] = None,
    retry_backoff_seconds: float = 0.25,
    drain_deadline_seconds: float = 30.0,
    identity: str = "worker",
    concurrency_limit_per_key: Optional[int] = None,
) -> None:
    """Run a managed worker that also drains when its owning client closes."""
    stops, tasks = _worker_states.setdefault(client, (set(), set()))
    close_stop = asyncio.Event()
    stops.add(close_stop)
    current = asyncio.current_task()
    if current is not None:
        tasks.add(current)

    class CombinedStop:
        def is_set(self) -> bool:
            return close_stop.is_set() or (stop is not None and stop.is_set())

        async def wait(self) -> None:
            if self.is_set():
                return
            waits = [asyncio.create_task(close_stop.wait())]
            if stop is not None:
                waits.append(asyncio.create_task(stop.wait()))
            done, pending = await asyncio.wait(waits, return_when=asyncio.FIRST_COMPLETED)
            for task in pending:
                task.cancel()
            await asyncio.gather(*done, return_exceptions=True)

    try:
        await _run_worker_loop(
            client,
            queue_name,
            handler,
            visibility_seconds=visibility_seconds,
            wait_seconds=wait_seconds,
            stop=CombinedStop(),  # type: ignore[arg-type]
            loads=loads,
            on_error=on_error,
            concurrency=concurrency,
            heartbeat_seconds=heartbeat_seconds,
            retry_backoff_seconds=retry_backoff_seconds,
            drain_deadline_seconds=drain_deadline_seconds,
            identity=identity,
            force_stop=close_stop,
            concurrency_limit_per_key=concurrency_limit_per_key,
        )
    finally:
        stops.discard(close_stop)
        if current is not None:
            tasks.discard(current)
        if not stops and not tasks:
            _worker_states.pop(client, None)


async def run_outbox_relay(
    client: Any,
    *,
    stop: Optional[asyncio.Event] = None,
    batch_size: int = 50,
    claim_seconds: float = 30.0,
    failure_backoff_seconds: float = 1.0,
    baggage_allowlist: Optional[list[str]] = None,
    idle_seconds: float = 0.5,
    retry_backoff_seconds: float = 0.25,
    on_error: Optional[Callable[[BaseException], Awaitable[None]]] = None,
) -> None:
    attempt = 0
    while not (stop is not None and stop.is_set()):
        try:
            report = await client.run_outbox_once(
                batch_size, claim_seconds, failure_backoff_seconds, baggage_allowlist
            )
            attempt = 0
            if report.claimed > 0:
                continue
            delay = idle_seconds
        except Exception as exc:  # noqa: BLE001
            if on_error is not None:
                await on_error(exc)
            attempt += 1
            delay = min(30.0, retry_backoff_seconds * (2 ** min(attempt, 5)))
            delay *= random.uniform(0.8, 1.2)
        if stop is None:
            await asyncio.sleep(delay)
        else:
            try:
                await asyncio.wait_for(stop.wait(), timeout=delay)
            except asyncio.TimeoutError:
                pass


def queue(client: Any, name: str, *, loads: Loads = json.loads, dumps: Dumps = json.dumps) -> Queue[Any]:
    return Queue(client, name, loads=loads, dumps=dumps)


def kv(client: Any, key: str, *, loads: Loads = json.loads, dumps: Dumps = json.dumps) -> KvKey[Any]:
    return KvKey(client, key, loads=loads, dumps=dumps)


def config(
    client: Any,
    key: str,
    default: Any,
    *,
    loads: Loads = json.loads,
    dumps: Dumps = json.dumps,
) -> ConfigKey[Any]:
    return ConfigKey(client, key, default, loads=loads, dumps=dumps)


def topic(
    client: Any, name: str, *, loads: Loads = json.loads, dumps: Dumps = json.dumps
) -> Topic[Any]:
    return Topic(client, name, loads=loads, dumps=dumps)


def _client_queue(
    self: Any, name: str, *, loads: Loads = json.loads, dumps: Dumps = json.dumps
) -> Queue[Any]:
    return Queue(self, name, loads=loads, dumps=dumps)


def _client_kv(
    self: Any, key: str, *, loads: Loads = json.loads, dumps: Dumps = json.dumps
) -> KvKey[Any]:
    return KvKey(self, key, loads=loads, dumps=dumps)


def _client_config(
    self: Any,
    key: str,
    default: Any,
    *,
    loads: Loads = json.loads,
    dumps: Dumps = json.dumps,
) -> ConfigKey[Any]:
    return ConfigKey(self, key, default, loads=loads, dumps=dumps)


def _client_topic(
    self: Any, name: str, *, loads: Loads = json.loads, dumps: Dumps = json.dumps
) -> Topic[Any]:
    return Topic(self, name, loads=loads, dumps=dumps)


def _client_rate_limit(self: Any, bucket: str, subject: str) -> RateLimiter:
    return RateLimiter(self, bucket, subject)


async def _client_worker(
    self: Any,
    name: str,
    handler: Callable[[QueueJob[Any]], Awaitable[None]],
    *,
    visibility_seconds: float = 30.0,
    wait_seconds: float = 20.0,
    stop: Optional[asyncio.Event] = None,
    loads: Loads = json.loads,
    on_error: Optional[OnError] = None,
    concurrency: int = 1,
    heartbeat_seconds: Optional[float] = None,
    retry_backoff_seconds: float = 0.25,
    drain_deadline_seconds: float = 30.0,
    identity: str = "worker",
    concurrency_limit_per_key: Optional[int] = None,
) -> None:
    await run_worker(
        self,
        name,
        handler,
        visibility_seconds=visibility_seconds,
        wait_seconds=wait_seconds,
        stop=stop,
        loads=loads,
        on_error=on_error,
        concurrency=concurrency,
        heartbeat_seconds=heartbeat_seconds,
        retry_backoff_seconds=retry_backoff_seconds,
        drain_deadline_seconds=drain_deadline_seconds,
        identity=identity,
        concurrency_limit_per_key=concurrency_limit_per_key,
    )


async def _client_run_outbox_relay(
    self: Any,
    *,
    stop: Optional[asyncio.Event] = None,
    batch_size: int = 50,
    claim_seconds: float = 30.0,
    failure_backoff_seconds: float = 1.0,
    baggage_allowlist: Optional[list[str]] = None,
    idle_seconds: float = 0.5,
    retry_backoff_seconds: float = 0.25,
    on_error: Optional[Callable[[BaseException], Awaitable[None]]] = None,
) -> None:
    stops, tasks = _worker_states.setdefault(self, (set(), set()))
    close_stop = asyncio.Event()
    stops.add(close_stop)
    current = asyncio.current_task()
    if current is not None:
        tasks.add(current)

    class CombinedStop:
        def is_set(self) -> bool:
            return close_stop.is_set() or (stop is not None and stop.is_set())

        async def wait(self) -> None:
            waits = [asyncio.create_task(close_stop.wait())]
            if stop is not None:
                waits.append(asyncio.create_task(stop.wait()))
            done, pending = await asyncio.wait(waits, return_when=asyncio.FIRST_COMPLETED)
            for task in pending:
                task.cancel()
            await asyncio.gather(*done, *pending, return_exceptions=True)

    try:
        await run_outbox_relay(
            self,
            stop=CombinedStop(),  # type: ignore[arg-type]
            batch_size=batch_size,
            claim_seconds=claim_seconds,
            failure_backoff_seconds=failure_backoff_seconds,
            baggage_allowlist=baggage_allowlist,
            idle_seconds=idle_seconds,
            retry_backoff_seconds=retry_backoff_seconds,
            on_error=on_error,
        )
    finally:
        stops.discard(close_stop)
        if current is not None:
            tasks.discard(current)
        if not stops and not tasks:
            _worker_states.pop(self, None)


_native_close = ForgeClient.close  # type: ignore[name-defined,attr-defined]


async def _close_managed_tasks(self: Any, timeout_seconds: float = 30.0) -> None:
    stops, tasks = _worker_states.get(self, (set(), set()))
    for event in list(stops):
        event.set()
    current = asyncio.current_task()
    pending = [task for task in tasks if task is not current]
    if pending:
        _, still_pending = await asyncio.wait(pending, timeout=max(0.0, timeout_seconds))
        for task in still_pending:
            task.cancel()
        await asyncio.gather(*still_pending, return_exceptions=True)


async def _client_close(self: Any, timeout_seconds: float = 30.0) -> None:
    loop = asyncio.get_running_loop()
    started = loop.time()
    await _close_managed_tasks(self, timeout_seconds)
    remaining = max(0.0, timeout_seconds - (loop.time() - started))
    await _native_close(self, remaining)


# If installing these ever fails (e.g. the native class becomes non-patchable),
# the whole idiomatic surface would vanish — that must be an import error, not
# a silent downgrade.
ForgeClient.queue = _client_queue  # type: ignore[name-defined,attr-defined]
ForgeClient.kv = _client_kv  # type: ignore[name-defined,attr-defined]
ForgeClient.config = _client_config  # type: ignore[name-defined,attr-defined]
ForgeClient.topic = _client_topic  # type: ignore[name-defined,attr-defined]
ForgeClient.rate_limit = _client_rate_limit  # type: ignore[name-defined,attr-defined]
ForgeClient.worker = _client_worker  # type: ignore[name-defined,attr-defined]
ForgeClient.run_outbox_relay = _client_run_outbox_relay  # type: ignore[name-defined,attr-defined]
ForgeClient.close = _client_close  # type: ignore[name-defined,attr-defined]


__all__ = [
    "ForgeClient",
    "Subscription",
    "ForgeError",
    "NotFoundError",
    "InvalidError",
    "LimitError",
    "PreconditionError",
    "UnavailableError",
    "ConfigError",
    "NotConfiguredError",
    "BackendError",
    "BlobInfo",
    "BlobSummary",
    "ConditionalBlobGet",
    "MultipartUpload",
    "MultipartPart",
    "ProxyPresign",
    "NativePresign",
    "ScheduleInfo",
    "SchedulePage",
    "SchedulerDiagnostics",
    "SessionInfo",
    "ApiKeyInfo",
    "TokenConsumption",
    "FlagEvaluation",
    "ConfigEntry",
    "FlagEvaluationRequest",
    "FlagEvaluationEntry",
    "ConfigSnapshot",
    "config_snapshot_get",
    "config_snapshot_flag_details",
    "encode_config_snapshot",
    "decode_config_snapshot",
    "BackendInfo",
    "BackendHealth",
    "HealthReport",
    "MetricSample",
    "ApiKey",
    "Job",
    "Decision",
    "QueueDepth",
    "ScanPage",
    "BlobListPage",
    "QueueJob",
    "Queue",
    "RateLimiter",
    "KvKey",
    "ConfigKey",
    "Topic",
    "queue",
    "kv",
    "config",
    "topic",
    "run_worker",
    "run_outbox_relay",
    "forge_error_code",
    "forge_error_retryable",
    "encode_queue_envelope",
    "decode_queue_envelope",
    "encode_invalidation_event",
    "decode_invalidation_event",
    "encode_cloud_event",
    "decode_cloud_event",
    "import_env_config",
    "export_env_config",
    "scope_kv_key",
    "scope_blob_key",
    "scope_rate_limit_subject",
    "scope_topic",
    "parse_scoped_name",
    "bytes_dumps",
    "bytes_loads",
]

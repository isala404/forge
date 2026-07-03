from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass
from typing import Any, AsyncIterator, Awaitable, Callable, Generic, Optional, TypeVar

from .forgelib import *  # noqa: F401,F403

T = TypeVar("T")

Loads = Callable[[str], Any]
Dumps = Callable[[Any], str]
OnError = Callable[[BaseException, Optional["QueueJob[Any]"]], Awaitable[None]]


def forge_error_code(exc: BaseException) -> str:
    """Return the Forge error class name for a raised exception."""

    return type(exc).__name__


def forge_error_retryable(exc: BaseException) -> bool:
    """Return whether a raised Forge error is retryable from Python's exception type."""

    return type(exc).__name__ == "Unavailable"


def _decode_payload(raw: Any) -> str:
    if isinstance(raw, (bytes, bytearray)):
        return raw.decode("utf-8")
    return raw


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
    lease_lost: Optional[asyncio.Event] = None


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
    ) -> str:
        return await self._c.queue_enqueue(
            self._name, self._dumps(payload), max_attempts, dedup_id, delay_seconds
        )

    async def dequeue(
        self, *, visibility_seconds: float = 30.0, wait_seconds: float = 20.0
    ) -> Optional[QueueJob[T]]:
        job = await self._c.queue_dequeue(self._name, visibility_seconds, wait_seconds)
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
        )

    async def ack(self, receipt: str) -> None:
        await self._c.queue_ack(receipt)

    async def nack(self, receipt: str, retry_seconds: Optional[float] = None) -> None:
        await self._c.queue_nack(receipt, retry_seconds)

    async def heartbeat(self, receipt: str) -> None:
        await self._c.queue_heartbeat(receipt)

    async def depth(self) -> Any:
        return await self._c.queue_depth(self._name)

    async def worker(
        self,
        handler: Callable[[QueueJob[T]], Awaitable[None]],
        *,
        visibility_seconds: float = 30.0,
        wait_seconds: float = 20.0,
        stop: Optional[asyncio.Event] = None,
        on_error: Optional[OnError] = None,
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
        )


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
) -> None:
    """Run a managed worker loop for a JSON queue. Set ``stop`` to drain.

    ``on_error`` is awaited with (exception, job) for every failure the loop
    absorbs — dequeue errors, undecodable payloads (job is None for both), and
    handler/ack failures — so failures are observable instead of silent.
    """

    async def report(exc: BaseException, job: Optional[QueueJob[Any]]) -> None:
        if on_error is not None:
            await on_error(exc, job)

    hb_every = max(1.0, visibility_seconds / 3.0)
    while not (stop is not None and stop.is_set()):
        try:
            raw = await client.queue_dequeue(queue_name, visibility_seconds, wait_seconds)
        except Exception as exc:  # transient backend blip; back off and retry  # noqa: BLE001
            await report(exc, None)
            await asyncio.sleep(0.25)
            continue
        if raw is None:
            continue

        lease_lost = asyncio.Event()
        try:
            payload = loads(raw.payload)
        except Exception as exc:  # bad payload; let retries/DLQ handle it  # noqa: BLE001
            try:
                await client.queue_nack(raw.receipt)
            except Exception:  # noqa: BLE001
                pass
            await report(exc, None)
            continue

        job: QueueJob[Any] = QueueJob(
            id=raw.id,
            receipt=raw.receipt,
            attempt=raw.attempt,
            max_attempts=raw.max_attempts,
            leased_until_ms=raw.leased_until_ms,
            queue=raw.queue,
            payload=payload,
            lease_lost=lease_lost,
        )

        async def _beat(receipt: str) -> None:
            while not lease_lost.is_set():
                await asyncio.sleep(hb_every)
                try:
                    await client.queue_heartbeat(receipt)
                except Exception:  # lease lost; stop heartbeating  # noqa: BLE001
                    lease_lost.set()
                    return

        beat = asyncio.create_task(_beat(job.receipt))
        try:
            await handler(job)
            beat.cancel()
            if not lease_lost.is_set():
                try:
                    await client.queue_ack(job.receipt)
                except Exception as exc:  # noqa: BLE001
                    await report(exc, job)
        except Exception as exc:  # noqa: BLE001
            beat.cancel()
            if not lease_lost.is_set():
                try:
                    await client.queue_nack(job.receipt)
                except Exception:  # noqa: BLE001
                    pass
            await report(exc, job)


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
    )


# If installing these ever fails (e.g. the native class becomes non-patchable),
# the whole idiomatic surface would vanish — that must be an import error, not
# a silent downgrade.
ForgeClient.queue = _client_queue  # type: ignore[name-defined,attr-defined]
ForgeClient.kv = _client_kv  # type: ignore[name-defined,attr-defined]
ForgeClient.config = _client_config  # type: ignore[name-defined,attr-defined]
ForgeClient.topic = _client_topic  # type: ignore[name-defined,attr-defined]
ForgeClient.worker = _client_worker  # type: ignore[name-defined,attr-defined]


__all__ = [
    "ForgeClient",
    "Subscription",
    "ForgeError",
    "NotFound",
    "Invalid",
    "Limit",
    "Precondition",
    "Unavailable",
    "Config",
    "Backend",
    "BlobInfo",
    "ScheduleInfo",
    "SchedulePage",
    "SessionInfo",
    "ApiKeyInfo",
    "BackendInfo",
    "ApiKey",
    "Job",
    "Decision",
    "QueueDepth",
    "ScanPage",
    "BlobListPage",
    "QueueJob",
    "Queue",
    "KvKey",
    "ConfigKey",
    "Topic",
    "queue",
    "kv",
    "config",
    "topic",
    "run_worker",
    "forge_error_code",
    "forge_error_retryable",
]

"""Typed projection over the forge_py ForgeClient.

Bind a name + JSON codec to a type, so app code enqueues a ``SendEmail`` model
instead of a raw queue string + ``json.dumps``. The Python view of the typed layer
the Rust crate (``src/typed.rs``) and Node binding (``forge-node/typed``) expose.

Each handle takes the compiled ``ForgeClient`` plus optional ``loads``/``dumps`` so a
Pydantic/attrs model can supply its own codec; the defaults use ``json``.
"""

from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass
from typing import Any, AsyncIterator, Awaitable, Callable, Generic, Optional, TypeVar

T = TypeVar("T")

Loads = Callable[[str], Any]
Dumps = Callable[[Any], str]


def forge_error_code(exc: BaseException) -> str:
    """The Forge error class name for a raised exception (e.g. ``"Invalid"``).

    The Python binding raises a typed hierarchy, so the code is the exception's class
    name; this mirrors the Node binding's ``forgeErrorCode`` string parser.
    """
    return type(exc).__name__


def forge_error_retryable(exc: BaseException) -> bool:
    """Whether a raised Forge error is retryable, per docs/contracts/errors.md. Only
    ``Unavailable`` is retryable from the class; a retryable ``Backend`` error is not
    distinguishable here (the flag is not surfaced), so it reads False.
    """
    return type(exc).__name__ == "Unavailable"


@dataclass
class TypedJob(Generic[T]):
    """A dequeued job whose payload was decoded into ``T``. Settle by ``receipt``
    (delivery-unique); ``id`` is stable across redeliveries (the idempotency key)."""

    id: str
    receipt: str
    attempt: int
    max_attempts: int
    payload: T


class TypedQueue(Generic[T]):
    """A typed queue handle: name + codec bound to a payload type ``T``."""

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
    ) -> str:
        return await self._c.queue_enqueue(
            self._name, self._dumps(payload), max_attempts, dedup_id
        )

    async def dequeue(
        self, *, visibility_seconds: float = 30.0, wait_seconds: float = 20.0
    ) -> Optional[TypedJob[T]]:
        job = await self._c.queue_dequeue(self._name, visibility_seconds, wait_seconds)
        if job is None:
            return None
        return TypedJob(
            id=job.id,
            receipt=job.receipt,
            attempt=job.attempt,
            max_attempts=job.max_attempts,
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


class TypedKvKey(Generic[T]):
    """A typed kv key: key + codec bound to a value type ``T``."""

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

    async def set(
        self,
        value: T,
        *,
        ttl_seconds: Optional[float] = None,
        if_not_exists: Optional[bool] = None,
    ) -> bool:
        return await self._c.kv_set(
            self._key, self._dumps(value), ttl_seconds, if_not_exists
        )

    async def delete(self) -> bool:
        return await self._c.kv_delete(self._key)


class TypedConfigKey(Generic[T]):
    """A typed config key: key + codec + default bound to a value type ``T``."""

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


class TypedTopic(Generic[T]):
    """A typed pubsub topic: topic + codec bound to an event type ``T``."""

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
        """``async for event in topic.subscribe():`` decodes each item into ``T``."""
        sub = await self._c.pubsub_subscribe(self._topic)
        async for payload in sub:
            yield self._loads(
                payload.decode("utf-8")
                if isinstance(payload, (bytes, bytearray))
                else payload
            )


async def run_worker(
    client: Any,
    queue_name: str,
    handler: Callable[[TypedJob[Any]], Awaitable[None]],
    *,
    visibility_seconds: float = 30.0,
    wait_seconds: float = 20.0,
    stop: Optional[asyncio.Event] = None,
    loads: Loads = json.loads,
) -> None:
    """Managed worker loop over a queue: dequeue, heartbeat at a third of the
    visibility window, ack on success / nack on exception, abandon on lease loss,
    drain on ``stop``.

        stop = asyncio.Event()
        await run_worker(client, "emails", handle, stop=stop)

    ``handler(job)`` receives a :class:`TypedJob` (payload decoded via ``loads``);
    raising nacks the job. ``stop`` stops the loop after the in-flight job drains.
    """
    hb_every = max(1.0, visibility_seconds / 3.0)
    while not (stop is not None and stop.is_set()):
        try:
            raw = await client.queue_dequeue(queue_name, visibility_seconds, wait_seconds)
        except Exception:  # transient backend blip; back off and retry  # noqa: BLE001
            await asyncio.sleep(0.25)
            continue
        if raw is None:
            continue
        job: TypedJob[Any] = TypedJob(
            id=raw.id,
            receipt=raw.receipt,
            attempt=raw.attempt,
            max_attempts=raw.max_attempts,
            payload=loads(raw.payload),
        )
        lease_lost = asyncio.Event()

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
                except Exception:  # noqa: BLE001
                    pass
        except Exception:  # noqa: BLE001
            beat.cancel()
            # If the lease was lost the receipt is gone; nacking would just raise.
            if not lease_lost.is_set():
                try:
                    await client.queue_nack(job.receipt)
                except Exception:  # noqa: BLE001
                    pass

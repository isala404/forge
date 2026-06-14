"""Typed projection over the forge_py ForgeClient.

Bind a name + JSON codec to a type, so app code enqueues a ``SendEmail`` model
instead of a raw queue string + ``json.dumps``. This is the Python view of the same
typed layer the Rust crate exposes (``src/typed.rs``) and the Node binding exposes
(``forge-node/typed``). Pure Python — no extra build step.

Each handle takes the compiled ``ForgeClient`` plus optional ``loads``/``dumps`` so a
Pydantic/attrs model can plug its own (de)serialization in; the defaults use ``json``.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, AsyncIterator, Callable, Generic, Optional, TypeVar

T = TypeVar("T")

Loads = Callable[[str], Any]
Dumps = Callable[[Any], str]


def forge_error_code(exc: BaseException) -> str:
    """The Forge error class name for a raised exception (e.g. ``"Invalid"``).

    The Python binding already raises a typed hierarchy (``forge_py.NotFound`` …),
    so the code is just the exception's class name; this helper exists for symmetry
    with the Node binding's ``forgeErrorCode`` string parser.
    """
    return type(exc).__name__


@dataclass
class TypedJob(Generic[T]):
    """A dequeued job whose payload was decoded into ``T``. Settle by ``id``."""

    id: str
    attempt: int
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
        self, *, visibility_seconds: float = 30.0, wait_seconds: float = 1.0
    ) -> Optional[TypedJob[T]]:
        job = await self._c.queue_dequeue(self._name, visibility_seconds, wait_seconds)
        if job is None:
            return None
        jid, payload, attempt = job
        return TypedJob(id=jid, attempt=attempt, payload=self._loads(payload))

    async def ack(self, id: str) -> None:
        await self._c.queue_ack(id)

    async def nack(self, id: str, retry_seconds: Optional[float] = None) -> None:
        await self._c.queue_nack(id, retry_seconds)

    async def heartbeat(self, id: str) -> None:
        await self._c.queue_heartbeat(id)

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
        """``async for event in await topic.subscribe():`` — each item decoded into ``T``."""
        sub = await self._c.pubsub_subscribe(self._topic)
        async for payload in sub:
            yield self._loads(
                payload.decode("utf-8")
                if isinstance(payload, (bytes, bytearray))
                else payload
            )

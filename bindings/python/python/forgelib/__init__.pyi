from __future__ import annotations

import asyncio
from typing import Any, AsyncIterator, Awaitable, Callable, Generic, Optional, TypeVar

from ._generated import (
    ApiKey,
    ApiKeyInfo,
    BackendInfo,
    BlobInfo,
    BlobListPage,
    Decision,
    Job,
    QueueDepth,
    ScanPage,
    ScheduleInfo,
    SchedulePage,
    SessionInfo,
)

T = TypeVar("T")
Loads = Callable[[str], T]
Dumps = Callable[[T], str]
OnError = Callable[[BaseException, Optional[QueueJob[Any]]], Awaitable[None]]

class ForgeError(Exception):
    """Base class for all Forge errors. Raised instances carry `retryable`."""

    retryable: bool

class NotFoundError(ForgeError): ...
class InvalidError(ForgeError): ...
class LimitError(ForgeError): ...
class PreconditionError(ForgeError): ...
class UnavailableError(ForgeError):
    """Backend unreachable; always safe to retry."""

class ConfigError(ForgeError): ...
class BackendError(ForgeError):
    """Backend operation failed; check `retryable` before retrying."""

class Subscription(AsyncIterator[bytes]):
    """Raw pubsub subscription yielding message bytes; `aclose()` unsubscribes."""

    def __aiter__(self) -> Subscription: ...
    async def __anext__(self) -> bytes: ...
    def aclose(self) -> Awaitable[None]: ...

class QueueJob(Generic[T]):
    """A leased job. `id` is the stable identity; `receipt` is the lease handle
    that `ack`/`nack`/`heartbeat` take (valid in this process only)."""

    id: str
    receipt: str
    attempt: int
    max_attempts: int
    leased_until_ms: float
    queue: str
    payload: T
    lease_lost: Optional[asyncio.Event]
    def __init__(
        self,
        id: str,
        receipt: str,
        attempt: int,
        max_attempts: int,
        leased_until_ms: float,
        queue: str,
        payload: T,
        lease_lost: Optional[asyncio.Event] = ...,
    ) -> None: ...

class Queue(Generic[T]):
    """Typed handle over one named queue (JSON-codec by default).
    Create via `forge.queue(name)`."""

    def __init__(
        self,
        client: ForgeClient,
        name: str,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> None: ...
    async def enqueue(
        self,
        payload: T,
        *,
        max_attempts: Optional[int] = ...,
        dedup_id: Optional[str] = ...,
        delay_seconds: Optional[float] = ...,
    ) -> str:
        """Enqueue and return the job id. `max_attempts` defaults to 5; a repeated
        `dedup_id` within the dedup window returns the existing job's id (no error)."""

    async def dequeue(
        self,
        *,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
    ) -> Optional[QueueJob[T]]:
        """Lease the next job (invisible to others for `visibility_seconds`,
        default 30), long-polling up to `wait_seconds` (default 20); None on timeout."""

    async def ack(self, receipt: str) -> None:
        """Settle a job as done. Takes `job.receipt`, not `job.id`."""

    async def nack(self, receipt: str, retry_seconds: Optional[float] = ...) -> None:
        """Return a job for redelivery after `retry_seconds` (default: backoff)."""

    async def heartbeat(self, receipt: str) -> None:
        """Extend the lease; raises `PreconditionError` if it was lost."""

    async def depth(self) -> QueueDepth: ...
    async def worker(
        self,
        handler: Callable[[QueueJob[T]], Awaitable[None]],
        *,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
        stop: Optional[asyncio.Event] = ...,
        on_error: Optional[OnError] = ...,
    ) -> None:
        """Managed loop: dequeue, run `handler`, ack on return / nack on raise,
        auto-heartbeat, back off on dequeue errors. Runs until `stop` is set."""

class KvKey(Generic[T]):
    """Typed handle over one KV key (JSON-codec by default). Create via `forge.kv(key)`."""

    def __init__(
        self,
        client: ForgeClient,
        key: str,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> None: ...
    async def get(self) -> Optional[T]: ...
    async def get_or_default(self, default: T) -> T: ...
    async def set(
        self,
        value: T,
        *,
        ttl_seconds: Optional[float] = ...,
        if_not_exists: Optional[bool] = ...,
        if_exists: Optional[bool] = ...,
    ) -> bool:
        """Write the value; False when an `if_not_exists`/`if_exists` guard failed."""

    async def delete(self) -> bool: ...
    async def exists(self) -> bool: ...
    async def expire(self, ttl_seconds: float) -> bool:
        """Reset the TTL on a live key; False if absent. Does not create keys."""

    async def compare_and_swap(self, old: Optional[T], new_value: T) -> bool:
        """Atomic CAS: write `new_value` iff the current value equals `old`
        (`old=None` means "expected absent")."""

class ConfigKey(Generic[T]):
    """Typed handle over one config key with a default. Create via `forge.config(key, default)`."""

    def __init__(
        self,
        client: ForgeClient,
        key: str,
        default: T,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> None: ...
    async def get(self) -> Optional[T]: ...
    async def get_or_default(self) -> T: ...
    async def set(self, value: T) -> None: ...
    async def delete(self) -> bool: ...
    async def flag(self, targeting_key: Optional[str] = ...) -> bool:
        """Evaluate the key as a feature flag, optionally targeting one subject."""

class Topic(Generic[T]):
    """Typed handle over one pubsub topic. Create via `forge.topic(name)`."""

    def __init__(
        self,
        client: ForgeClient,
        topic: str,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> None: ...
    async def publish(self, event: T) -> None: ...
    def subscribe(self) -> AsyncIterator[T]:
        """Async-iterate decoded events; break out of the loop to unsubscribe."""

    def channel(self) -> str:
        """The backend channel name (namespaced; for LISTEN/debug tooling)."""

class ForgeClient:
    """The Forge handle. Construct with `await ForgeClient.init()` (reads
    `./forge.toml`) or `init_from(path)`; all connection settings, including
    `[postgres] embedded = true`, live in that file. Prefer the typed handles
    (`queue`/`kv`/`config`/`topic`) over the flat `kv_*`/`queue_*` methods,
    which speak raw strings."""

    @staticmethod
    def init() -> Awaitable[ForgeClient]:
        """Connect using `./forge.toml`."""

    @staticmethod
    def init_from(path: str) -> Awaitable[ForgeClient]:
        """Connect using the config file at `path`."""

    def queue(
        self,
        name: str,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> Queue[T]: ...
    def kv(
        self,
        key: str,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> KvKey[T]: ...
    def config(
        self,
        key: str,
        default: T,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> ConfigKey[T]: ...
    def topic(
        self,
        name: str,
        *,
        loads: Loads[T] = ...,
        dumps: Dumps[T] = ...,
    ) -> Topic[T]: ...
    def worker(
        self,
        name: str,
        handler: Callable[[QueueJob[T]], Awaitable[None]],
        *,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
        stop: Optional[asyncio.Event] = ...,
        loads: Loads[T] = ...,
        on_error: Optional[OnError] = ...,
    ) -> Awaitable[None]:
        """Shorthand for `run_worker(client, name, handler, ...)`."""

    def postgres_url(self) -> str:
        """The resolved system-database DSN — the configured `[postgres] url`, or
        the one an embedded server minted at init. Contains credentials; use it to
        point the app's own tables/pool at the same database."""

    def backend_report(self) -> list[BackendInfo]:
        """Which backend powers each primitive (for logs/health pages)."""

    def kv_get(self, key: str) -> Awaitable[Optional[str]]: ...
    def kv_get_bytes(self, key: str) -> Awaitable[Optional[bytes]]: ...
    def kv_mget(self, keys: list[str]) -> Awaitable[list[Optional[str]]]: ...
    def kv_set(
        self,
        key: str,
        value: str,
        ttl_seconds: Optional[float] = ...,
        if_not_exists: Optional[bool] = ...,
        if_exists: Optional[bool] = ...,
    ) -> Awaitable[bool]:
        """Write; False when an `if_not_exists`/`if_exists` guard failed."""

    def kv_set_bytes(
        self,
        key: str,
        value: bytes,
        ttl_seconds: Optional[float] = ...,
        if_not_exists: Optional[bool] = ...,
    ) -> Awaitable[bool]: ...
    def kv_incr(self, key: str, by: int) -> Awaitable[int]:
        """Atomic increment (exact int). Missing key starts at 0; a non-numeric
        value raises `InvalidError`."""

    def kv_scan(self, prefix: str, limit: int) -> Awaitable[list[str]]: ...
    def kv_delete(self, key: str) -> Awaitable[bool]: ...
    def kv_exists(self, key: str) -> Awaitable[bool]: ...
    def kv_expire(self, key: str, ttl_seconds: float) -> Awaitable[bool]:
        """Reset the TTL on a live key; False if absent. Does not create keys."""

    def kv_compare_and_swap(
        self, key: str, old: Optional[str], new_value: str
    ) -> Awaitable[bool]:
        """Atomic CAS; `old=None` means "expected absent"."""

    def kv_scan_page(
        self, prefix: str, cursor: Optional[str] = ..., limit: int = ...
    ) -> Awaitable[ScanPage]:
        """Cursor-paginated scan; pass the returned cursor back for the next page."""

    def queue_enqueue(
        self,
        queue: str,
        payload: str,
        max_attempts: Optional[int] = ...,
        dedup_id: Optional[str] = ...,
        delay_seconds: Optional[float] = ...,
    ) -> Awaitable[str]:
        """Enqueue and return the job id. `max_attempts` defaults to 5; a repeated
        `dedup_id` within the dedup window returns the existing job's id (no error)."""

    def queue_dequeue(
        self, queue: str, visibility_seconds: float, wait_seconds: float
    ) -> Awaitable[Optional[Job]]:
        """Lease the next job for `visibility_seconds`, long-polling up to
        `wait_seconds`; None on timeout. Settle with `job.receipt`, not `job.id`."""

    def queue_ack(self, receipt: str) -> Awaitable[None]: ...
    def queue_nack(
        self, receipt: str, retry_seconds: Optional[float] = ...
    ) -> Awaitable[None]:
        """Return the job for redelivery after `retry_seconds` (default: backoff)."""

    def queue_heartbeat(self, receipt: str) -> Awaitable[None]:
        """Extend the lease; raises `PreconditionError` if it was lost."""

    def queue_depth(self, queue: str) -> Awaitable[QueueDepth]: ...
    def config_set(self, key: str, value: str) -> Awaitable[None]: ...
    def config_get(self, key: str) -> Awaitable[Optional[str]]: ...
    def config_delete(self, key: str) -> Awaitable[bool]: ...
    def set_flag_percent(self, key: str, percent: int) -> Awaitable[None]:
        """Roll a flag out to `percent`% of targeting keys (0-100, stable bucketing
        per targeting key)."""

    def set_flag_on(self, key: str) -> Awaitable[None]: ...
    def set_flag_off(self, key: str) -> Awaitable[None]: ...
    def set_flag_allow_list(self, key: str, entries: list[str]) -> Awaitable[None]:
        """Enable a flag only for the targeting keys in `entries`."""

    def delete_flag(self, key: str) -> Awaitable[bool]: ...
    def flag(
        self, key: str, default_value: bool, targeting_key: Optional[str] = ...
    ) -> Awaitable[bool]:
        """Evaluate a flag; `default_value` when unset."""

    def rate_limit_check(
        self,
        bucket: str,
        key: str,
        max: int,
        per_seconds: float,
        fail_open: Optional[bool] = ...,
        algo: Optional[str] = ...,
    ) -> Awaitable[Decision]:
        """Allow `max` per `per_seconds` (whole seconds; < 1s is `InvalidError`).
        A denial is `Decision.allowed == False`, not an exception.
        `algo`: "token_bucket" (default) or "sliding_window"."""

    def blob_put(
        self, key: str, data: bytes, content_type: Optional[str] = ...
    ) -> Awaitable[None]: ...
    def blob_put_object(
        self,
        key: str,
        data: bytes,
        content_type: Optional[str] = ...,
        metadata: Optional[dict[str, str]] = ...,
    ) -> Awaitable[None]: ...
    def blob_get(self, key: str) -> Awaitable[Optional[bytes]]: ...
    def blob_head(self, key: str) -> Awaitable[Optional[BlobInfo]]:
        """Metadata without the bytes; None if absent."""

    def blob_list(
        self, prefix: str, cursor: Optional[str] = ..., limit: int = ...
    ) -> Awaitable[BlobListPage]: ...
    def blob_presign_download(self, key: str, expires_seconds: float) -> Awaitable[str]:
        """Signed URL path (under the configured `base_url`); needs
        `[blob] signing_secret`."""

    def blob_presign_upload(
        self, key: str, expires_seconds: float, max_bytes: int
    ) -> Awaitable[str]: ...
    def blob_verify_presign(
        self, method: str, key: str, expires_epoch: int, max_bytes: int, sig: str
    ) -> Awaitable[bool]:
        """Check a presigned request's signature/expiry when serving it."""

    def blob_content_type(self, key: str) -> Awaitable[Optional[str]]: ...
    def blob_delete(self, key: str) -> Awaitable[bool]: ...
    def hash_password(self, plain: str) -> Awaitable[str]:
        """Argon2id PHC string; store it as-is."""

    def verify_password(self, plain: str, hash: str) -> Awaitable[bool]: ...
    def needs_rehash(self, hash: str) -> bool:
        """True when `hash` predates the current parameters; rehash on next login."""

    def create_session(
        self,
        user_id: str,
        idle_seconds: Optional[float] = ...,
        absolute_seconds: Optional[float] = ...,
    ) -> Awaitable[str]:
        """Mint a session token for `user_id`."""

    def validate_session(self, token: str) -> Awaitable[Optional[str]]:
        """The session's user id, or None when invalid/expired (not an exception)."""

    def validate_session_info(self, token: str) -> Awaitable[Optional[SessionInfo]]: ...
    def revoke_session(self, token: str) -> Awaitable[None]: ...
    def revoke_all_sessions(self, user_id: str) -> Awaitable[int]:
        """Revoke every session for `user_id`; returns how many."""

    def create_api_key(self, owner_id: str, label: str) -> Awaitable[ApiKey]:
        """The returned `ApiKey.key` is shown once; only its hash is stored."""

    def verify_api_key(self, key: str) -> Awaitable[Optional[str]]:
        """The key's owner id, or None when unknown/revoked (not an exception)."""

    def verify_api_key_info(self, key: str) -> Awaitable[Optional[ApiKeyInfo]]: ...
    def revoke_api_key(self, id: str) -> Awaitable[bool]: ...
    def create_token(self, user_id: str, purpose: str, ttl_seconds: float) -> Awaitable[str]:
        """Single-use token scoped to `purpose`, shown once; only its hash is stored."""

    def consume_token(self, token: str, purpose: str) -> Awaitable[Optional[str]]:
        """Consume the token and return its user id, or None when unknown/expired/used
        (not an exception). A wrong `purpose` leaves a live token intact."""

    def schedule_at(
        self,
        when_epoch_ms: float,
        queue: str,
        payload: str,
        max_attempts: Optional[int] = ...,
    ) -> Awaitable[str]:
        """Enqueue `payload` onto `queue` at `when_epoch_ms` (Unix epoch, ms)."""

    def schedule_cron(
        self,
        name: str,
        expr: str,
        queue: str,
        payload: str,
        max_attempts: Optional[int] = ...,
    ) -> Awaitable[None]:
        """Upsert a named cron schedule (5-field expression, UTC)."""

    def schedule_cancel(self, name: str) -> Awaitable[bool]: ...
    def schedule_cancel_at(self, job_id: str) -> Awaitable[bool]: ...
    def schedule_list(
        self, cursor: Optional[str] = ..., limit: Optional[int] = ...
    ) -> Awaitable[SchedulePage]: ...
    def run_scheduler_once(self) -> Awaitable[int]:
        """Fire due schedules now (normally driven by `maintain`); returns how many."""

    def maintain(self) -> Awaitable[None]:
        """One maintenance sweep (due schedules, expired rows, retention). Call
        periodically from a background task."""

    def pubsub_publish(self, topic: str, payload: str) -> Awaitable[None]: ...
    def pubsub_subscribe(self, topic: str) -> Awaitable[Subscription]: ...
    def pubsub_channel(self, topic: str) -> str:
        """The backend channel name (namespaced; for LISTEN/debug tooling)."""

def forge_error_code(exc: BaseException) -> str:
    """Canonical code ("NotFound", "Limit", ...): the exception class name minus
    its `Error` suffix."""

def forge_error_retryable(exc: BaseException) -> bool:
    """Whether the error is safe to retry (reads the `retryable` attribute)."""

def queue(
    client: ForgeClient,
    name: str,
    *,
    loads: Loads[T] = ...,
    dumps: Dumps[T] = ...,
) -> Queue[T]: ...
def kv(
    client: ForgeClient,
    key: str,
    *,
    loads: Loads[T] = ...,
    dumps: Dumps[T] = ...,
) -> KvKey[T]: ...
def config(
    client: ForgeClient,
    key: str,
    default: T,
    *,
    loads: Loads[T] = ...,
    dumps: Dumps[T] = ...,
) -> ConfigKey[T]: ...
def topic(
    client: ForgeClient,
    name: str,
    *,
    loads: Loads[T] = ...,
    dumps: Dumps[T] = ...,
) -> Topic[T]: ...
async def run_worker(
    client: ForgeClient,
    queue_name: str,
    handler: Callable[[QueueJob[T]], Awaitable[None]],
    *,
    visibility_seconds: float = ...,
    wait_seconds: float = ...,
    stop: Optional[asyncio.Event] = ...,
    loads: Loads[T] = ...,
    on_error: Optional[OnError] = ...,
) -> None:
    """Managed worker loop: dequeue, run `handler`, ack on return / nack on raise,
    auto-heartbeat at `visibility_seconds / 3`, back off on dequeue errors. Runs
    until `stop` is set. `on_error` sees every failure (job=None for dequeue
    errors)."""

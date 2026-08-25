from __future__ import annotations

import asyncio
from typing import Any, AsyncIterator, Awaitable, Callable, Generic, Optional, TypeVar

from ._generated import (
    ApiKey,
    ApiKeyInfo,
    BackendInfo,
    BackendHealth,
    BatchEnqueueResult,
    BlobInfo,
    BlobListPage,
    BlobSummary,
    ConditionalBlobGet,
    ConfigEntry,
    ConfigSnapshot,
    Decision,
    DiagnosticCheck,
    DiagnosticsReport,
    DeadLetterPage,
    FlagEvaluation,
    FlagEvaluationEntry,
    Job,
    HealthReport,
    MetricSample,
    MigrationReport,
    MultipartPart,
    MultipartUpload,
    NativePresign,
    OutboxRelayReport,
    ProxyPresign,
    QueueDepth,
    QueueStats,
    RedriveBatchResult,
    ScanPage,
    ScheduleInfo,
    SchedulePage,
    SchedulerDiagnostics,
    SessionInfo,
    TokenConsumption,
)

def scope_kv_key(application: str, tenant: str, user: str, resource: str) -> str: ...
def scope_blob_key(application: str, tenant: str, user: str, resource: str) -> str: ...
def scope_rate_limit_subject(application: str, tenant: str, user: str, resource: str) -> str: ...
def scope_topic(application: str, tenant: str, user: str, resource: str) -> str: ...
def parse_scoped_name(value: str) -> dict[str, str]: ...

class FlagEvaluationRequest:
    id: str
    key: str
    default_json: str
    targeting_key: Optional[str]
    context_json: Optional[str]
    def __init__(self, id: str, key: str, default_json: str, targeting_key: Optional[str] = ..., context_json: Optional[str] = ...) -> None: ...

T = TypeVar("T")
Loads = Callable[[bytes | str], T]
Dumps = Callable[[T], bytes | str]
OnError = Callable[[BaseException, Optional[QueueJob[Any]]], Awaitable[None]]

def encode_invalidation_event(event: dict[str, Any]) -> bytes: ...
def decode_invalidation_event(encoded: str | bytes) -> dict[str, Any]: ...
def encode_cloud_event(event: dict[str, Any]) -> bytes: ...
def decode_cloud_event(encoded: str | bytes) -> dict[str, Any]: ...
def import_env_config(environment: dict[str, str], mappings: list[dict[str, Any]]) -> dict[str, str]: ...
def export_env_config(config: dict[str, str], mappings: list[dict[str, Any]]) -> dict[str, str]: ...
def config_snapshot_get(snapshot: ConfigSnapshot, key: str, now_ms: Optional[float] = ...) -> Optional[str]: ...
def config_snapshot_flag_details(snapshot: ConfigSnapshot, request_id: str, now_ms: Optional[float] = ...) -> FlagEvaluation: ...
def encode_config_snapshot(snapshot: ConfigSnapshot) -> bytes: ...
def decode_config_snapshot(encoded: bytes) -> ConfigSnapshot: ...

class ForgeError(Exception):
    """Structured, secret-safe Forge failure."""

    code: str
    retryable: bool
    operation: str
    backend: Optional[str]
    safe_message: str

class NotFoundError(ForgeError): ...
class InvalidError(ForgeError): ...
class LimitError(ForgeError): ...
class PreconditionError(ForgeError): ...
class UnavailableError(ForgeError):
    """Backend unreachable; always safe to retry."""

class ConfigError(ForgeError): ...
class NotConfiguredError(ForgeError): ...
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
    trace_context: Optional[dict[str, str]]
    lease_lost: Optional[asyncio.Event]
    cancelled: Optional[asyncio.Event]
    worker_identity: str
    def __init__(
        self,
        id: str,
        receipt: str,
        attempt: int,
        max_attempts: int,
        leased_until_ms: float,
        queue: str,
        payload: T,
        trace_context: Optional[dict[str, str]] = ...,
        lease_lost: Optional[asyncio.Event] = ...,
        cancelled: Optional[asyncio.Event] = ...,
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
        job_id: Optional[str] = ...,
        trace_context: Optional[dict[str, str]] = ...,
        baggage_allowlist: Optional[list[str]] = ...,
        priority: Optional[str] = ...,
        concurrency_key: Optional[str] = ...,
    ) -> str:
        """Enqueue and return the job id. `max_attempts` defaults to 5; a repeated
        `dedup_id` within the dedup window returns the existing job's id (no error)."""

    async def enqueue_batch(
        self, items: list[tuple[T, Optional[str]]]
    ) -> list[BatchEnqueueResult]: ...

    async def dequeue(
        self,
        *,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
        concurrency_limit_per_key: Optional[int] = ...,
    ) -> Optional[QueueJob[T]]:
        """Lease the next job (invisible to others for `visibility_seconds`,
        default 30), long-polling up to `wait_seconds` (default 20); None on timeout."""

    async def dequeue_batch(
        self,
        max_items: int,
        *,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
        concurrency_limit_per_key: Optional[int] = ...,
    ) -> list[QueueJob[T]]: ...

    async def ack(self, receipt: str) -> None:
        """Settle a job as done. Takes `job.receipt`, not `job.id`."""

    async def nack(
        self,
        receipt: str,
        retry_seconds: Optional[float] = ...,
        failure_summary: Optional[str] = ...,
    ) -> None:
        """Return a job for redelivery after `retry_seconds` (default: backoff)."""

    async def heartbeat(self, receipt: str) -> None:
        """Extend the lease; raises `PreconditionError` if it was lost."""

    async def cancel(self, job_id: str) -> Optional[dict[str, Any]]: ...
    async def status(self, job_id: str) -> Optional[dict[str, Any]]: ...
    async def statuses(
        self,
        *,
        states: Optional[list[str]] = ...,
        cursor: Optional[str] = ...,
        limit: int = ...,
    ) -> dict[str, Any]: ...

    async def depth(self) -> QueueDepth: ...
    async def pause(self) -> None: ...
    async def resume(self) -> None: ...
    async def is_paused(self) -> bool: ...
    async def stats(self) -> QueueStats: ...
    async def dead_letters(
        self, cursor: Optional[str] = ..., limit: int = ...
    ) -> DeadLetterPage: ...
    async def redrive(
        self, job_id: str, *, destination: str, dedup_policy: str
    ) -> bool: ...
    async def redrive_batch(
        self,
        *,
        destination: str,
        dedup_policy: str,
        cursor: Optional[str] = ...,
        limit: int = ...,
    ) -> RedriveBatchResult: ...
    async def purge_dry_run(self) -> int: ...
    async def purge(self, confirmation: str) -> int: ...
    async def worker(
        self,
        handler: Callable[[QueueJob[T]], Awaitable[None]],
        *,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
        stop: Optional[asyncio.Event] = ...,
        on_error: Optional[OnError] = ...,
        concurrency: int = ...,
        heartbeat_seconds: Optional[float] = ...,
        retry_backoff_seconds: float = ...,
        drain_deadline_seconds: float = ...,
        identity: str = ...,
        concurrency_limit_per_key: Optional[int] = ...,
    ) -> None:
        """Managed loop: dequeue, run `handler`, ack on return / nack on raise,
        auto-heartbeat, back off on dequeue errors. Runs until `stop` is set."""

class RateLimiter:
    def __init__(self, client: ForgeClient, bucket: str, subject: str) -> None: ...
    async def check(
        self,
        *,
        max: int,
        per_seconds: float,
        cost: int = ...,
        algo: Optional[str] = ...,
        fail_open: Optional[bool] = ...,
    ) -> Decision: ...
    async def reserve(
        self,
        *,
        max: int,
        per_seconds: float,
        cost: int,
        ttl_seconds: float,
        algo: Optional[str] = ...,
    ) -> Optional[dict[str, Any]]: ...
    async def commit(self, reservation_id: str, actual_units: int) -> dict[str, Any]: ...
    async def release(self, reservation_id: str) -> dict[str, Any]: ...

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

    @staticmethod
    def init_from_string(toml: str) -> Awaitable[ForgeClient]:
        """Connect using canonical TOML supplied in memory."""

    @staticmethod
    def init_memory_for_testing(toml: str, start_ms: float, seed: int) -> Awaitable[ForgeClient]:
        """Create a memory client with manual time and deterministic token entropy."""

    def advance_test_clock(self, seconds: float) -> None:
        """Advance time on a client created by ``init_memory_for_testing``."""

    @staticmethod
    def migrate() -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def migrate_from(path: str) -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def migrate_from_string(toml: str) -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def migration_status() -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def migration_status_from(path: str) -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def migration_status_from_string(toml: str) -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def validate_schema() -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def validate_schema_from(path: str) -> Awaitable[list[MigrationReport]]: ...
    @staticmethod
    def validate_schema_from_string(toml: str) -> Awaitable[list[MigrationReport]]: ...

    def close(self, timeout_seconds: float = ...) -> Awaitable[None]:
        """Idempotently drain and close within the caller's deadline."""

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
    def rate_limit(self, bucket: str, subject: str) -> RateLimiter: ...
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
        concurrency: int = ...,
        heartbeat_seconds: Optional[float] = ...,
        retry_backoff_seconds: float = ...,
        drain_deadline_seconds: float = ...,
        identity: str = ...,
    ) -> Awaitable[None]:
        """Shorthand for `run_worker(client, name, handler, ...)`."""

    def postgres_url(self) -> str:
        """The resolved system-database DSN — the configured `[postgres] url`, or
        the one an embedded server minted at init. Contains credentials; use it to
        point the app's own tables/pool at the same database."""

    def backend_capabilities(self) -> list[BackendInfo]:
        """Static provider capabilities; this performs no I/O."""

    def is_live(self) -> bool: ...
    def probe(
        self,
        deadline_seconds: float = ...,
        readiness_backends: Optional[list[str]] = ...,
    ) -> Awaitable[HealthReport]: ...
    def diagnostics(self, deadline_seconds: float = ...) -> Awaitable[DiagnosticsReport]: ...
    def metrics_snapshot(self) -> list[MetricSample]: ...
    def render_prometheus(self) -> str: ...

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
        payload: bytes,
        max_attempts: Optional[int] = ...,
        dedup_id: Optional[str] = ...,
        delay_seconds: Optional[float] = ...,
        job_id: Optional[str] = ...,
        traceparent: Optional[str] = ...,
        tracestate: Optional[str] = ...,
        baggage: Optional[str] = ...,
        baggage_allowlist: Optional[list[str]] = ...,
        priority: Optional[str] = ...,
        concurrency_key: Optional[str] = ...,
    ) -> Awaitable[str]:
        """Enqueue and return the job id. `max_attempts` defaults to 5; a repeated
        `dedup_id` within the dedup window returns the existing job's id (no error)."""

    def queue_enqueue_batch(
        self, queue: str, items: list[tuple[bytes, Optional[str]]]
    ) -> Awaitable[list[BatchEnqueueResult]]: ...

    def queue_dequeue(
        self,
        queue: str,
        visibility_seconds: float,
        wait_seconds: float,
        concurrency_limit_per_key: Optional[int] = ...,
    ) -> Awaitable[Optional[Job]]:
        """Lease the next job for `visibility_seconds`, long-polling up to
        `wait_seconds`; None on timeout. Settle with `job.receipt`, not `job.id`."""

    def queue_dequeue_batch(
        self,
        queue: str,
        max_items: int,
        visibility_seconds: float = ...,
        wait_seconds: float = ...,
        concurrency_limit_per_key: Optional[int] = ...,
    ) -> Awaitable[list[Job]]: ...

    def queue_ack(self, receipt: str) -> Awaitable[None]: ...
    def queue_nack(
        self,
        receipt: str,
        retry_seconds: Optional[float] = ...,
        failure_summary: Optional[str] = ...,
    ) -> Awaitable[None]:
        """Return the job for redelivery after `retry_seconds` (default: backoff)."""

    def queue_heartbeat(self, receipt: str) -> Awaitable[None]:
        """Extend the lease; raises `PreconditionError` if it was lost."""

    def queue_cancellation_requested(self, receipt: str) -> Awaitable[bool]: ...
    def queue_finish_cancellation(self, receipt: str) -> Awaitable[None]: ...
    def queue_cancel(self, job_id: str) -> Awaitable[Optional[str]]: ...
    def queue_status(self, job_id: str) -> Awaitable[Optional[str]]: ...
    def queue_list_status(
        self,
        queue: Optional[str] = ...,
        states: Optional[list[str]] = ...,
        cursor: Optional[str] = ...,
        limit: int = ...,
    ) -> Awaitable[str]: ...

    def queue_depth(self, queue: str) -> Awaitable[QueueDepth]: ...
    def queue_pause(self, queue: str) -> Awaitable[None]: ...
    def queue_resume(self, queue: str) -> Awaitable[None]: ...
    def queue_is_paused(self, queue: str) -> Awaitable[bool]: ...
    def queue_stats(self, queue: str) -> Awaitable[QueueStats]: ...
    def queue_dead_letters(
        self, queue: str, cursor: Optional[str] = ..., limit: int = ...
    ) -> Awaitable[DeadLetterPage]: ...
    def queue_redrive(
        self, job_id: str, destination: str, dedup_policy: str
    ) -> Awaitable[bool]: ...
    def queue_redrive_batch(
        self,
        queue: str,
        destination: str,
        dedup_policy: str,
        cursor: Optional[str] = ...,
        limit: int = ...,
    ) -> Awaitable[RedriveBatchResult]: ...
    def queue_purge_dead_letters_dry_run(self, queue: str) -> Awaitable[int]: ...
    def queue_purge_dead_letters(
        self, queue: str, confirmation: str
    ) -> Awaitable[int]: ...
    def run_outbox_once(
        self,
        batch_size: Optional[int] = ...,
        claim_seconds: Optional[float] = ...,
        failure_backoff_seconds: Optional[float] = ...,
        baggage_allowlist: Optional[list[str]] = ...,
    ) -> Awaitable[OutboxRelayReport]: ...
    def run_outbox_relay(
        self,
        *,
        stop: Optional[asyncio.Event] = ...,
        batch_size: int = ...,
        claim_seconds: float = ...,
        failure_backoff_seconds: float = ...,
        baggage_allowlist: Optional[list[str]] = ...,
        idle_seconds: float = ...,
        retry_backoff_seconds: float = ...,
        on_error: Optional[Callable[[BaseException], Awaitable[None]]] = ...,
    ) -> Awaitable[None]: ...
    def config_set(self, key: str, value: str) -> Awaitable[None]: ...
    def config_get(self, key: str) -> Awaitable[Optional[str]]: ...
    def config_get_many(self, keys: list[str]) -> Awaitable[list[ConfigEntry]]: ...
    def config_delete(self, key: str) -> Awaitable[bool]: ...
    def set_flag_percent(self, key: str, percent: int) -> Awaitable[None]:
        """Roll a flag out to `percent`% of targeting keys (0-100, stable bucketing
        per targeting key)."""

    def set_flag_on(self, key: str) -> Awaitable[None]: ...
    def set_flag_off(self, key: str) -> Awaitable[None]: ...
    def set_flag_allow_list(self, key: str, entries: list[str]) -> Awaitable[None]:
        """Enable a flag only for the targeting keys in `entries`."""

    def set_flag_value(
        self, key: str, value_json: str, variant: str
    ) -> Awaitable[None]: ...

    def delete_flag(self, key: str) -> Awaitable[bool]: ...
    def flag(
        self, key: str, default_value: bool, targeting_key: Optional[str] = ...
    ) -> Awaitable[bool]:
        """Evaluate a flag; `default_value` when unset."""

    def flag_details(
        self,
        key: str,
        default_json: str,
        targeting_key: Optional[str] = ...,
    ) -> Awaitable[FlagEvaluation]: ...
    def flag_details_many(
        self, requests: list[FlagEvaluationRequest]
    ) -> Awaitable[list[FlagEvaluationEntry]]: ...
    def config_snapshot(
        self,
        config_keys: list[str],
        flag_requests: list[FlagEvaluationRequest],
        max_stale_seconds: float,
        secret_handling: str,
    ) -> Awaitable[ConfigSnapshot]: ...
    def encode_config_snapshot(self, snapshot: ConfigSnapshot) -> bytes: ...
    def decode_config_snapshot(self, encoded: bytes) -> ConfigSnapshot: ...

    def rate_limit_check(
        self,
        bucket: str,
        key: str,
        max: int,
        per_seconds: float,
        fail_open: Optional[bool] = ...,
        algo: Optional[str] = ...,
        cost: int = ...,
    ) -> Awaitable[Decision]:
        """Allow `max` per `per_seconds` (whole seconds; < 1s is `InvalidError`).
        A denial is `Decision.allowed == False`, not an exception.
        `algo`: "token_bucket" (default) or "sliding_window"."""

    def rate_limit_reserve(
        self,
        bucket: str,
        key: str,
        max: int,
        per_seconds: float,
        cost: int,
        ttl_seconds: float,
        algo: Optional[str] = ...,
    ) -> Awaitable[Optional[str]]: ...
    def rate_limit_commit(self, reservation_id: str, actual_units: int) -> Awaitable[str]: ...
    def rate_limit_release(self, reservation_id: str) -> Awaitable[str]: ...

    def blob_put(
        self, key: str, data: bytes, content_type: Optional[str] = ...
    ) -> Awaitable[None]: ...
    def blob_put_object(
        self,
        key: str,
        data: bytes,
        content_type: Optional[str] = ...,
        metadata: Optional[dict[str, str]] = ...,
        create_only: bool = ...,
        match_version: Optional[str] = ...,
        cache_control: Optional[str] = ...,
        content_disposition: Optional[str] = ...,
        checksum_sha256: Optional[str] = ...,
        sse_algorithm: Optional[str] = ...,
        sse_kms_key_id: Optional[str] = ...,
    ) -> Awaitable[None]: ...
    def blob_put_file(
        self,
        key: str,
        path: str,
        content_type: Optional[str] = ...,
        metadata: Optional[dict[str, str]] = ...,
        create_only: bool = ...,
        match_version: Optional[str] = ...,
        cache_control: Optional[str] = ...,
        content_disposition: Optional[str] = ...,
        checksum_sha256: Optional[str] = ...,
        sse_algorithm: Optional[str] = ...,
        sse_kms_key_id: Optional[str] = ...,
    ) -> Awaitable[None]: ...
    def blob_get(self, key: str) -> Awaitable[Optional[bytes]]: ...
    def blob_get_if(
        self,
        key: str,
        if_match: Optional[str] = ...,
        if_none_match: Optional[str] = ...,
    ) -> Awaitable[ConditionalBlobGet]: ...
    def blob_get_range(
        self, key: str, start: int, end: int
    ) -> Awaitable[Optional[bytes]]: ...
    def blob_head(self, key: str) -> Awaitable[Optional[BlobInfo]]:
        """Metadata without the bytes; None if absent."""

    def blob_list(
        self, prefix: str, cursor: Optional[str] = ..., limit: int = ...
    ) -> Awaitable[BlobListPage]: ...
    def blob_copy(
        self,
        source: str,
        destination: str,
        content_type: Optional[str] = ...,
        metadata: Optional[dict[str, str]] = ...,
        create_only: bool = ...,
        match_version: Optional[str] = ...,
        cache_control: Optional[str] = ...,
        content_disposition: Optional[str] = ...,
        checksum_sha256: Optional[str] = ...,
        sse_algorithm: Optional[str] = ...,
        sse_kms_key_id: Optional[str] = ...,
    ) -> Awaitable[BlobInfo]: ...
    def blob_create_multipart(
        self,
        key: str,
        content_type: Optional[str] = ...,
        metadata: Optional[dict[str, str]] = ...,
        create_only: bool = ...,
        match_version: Optional[str] = ...,
        cache_control: Optional[str] = ...,
        content_disposition: Optional[str] = ...,
        sse_algorithm: Optional[str] = ...,
        sse_kms_key_id: Optional[str] = ...,
    ) -> Awaitable[MultipartUpload]: ...
    def blob_upload_part(
        self, upload: MultipartUpload, part_number: int, body: bytes
    ) -> Awaitable[MultipartPart]: ...
    def blob_complete_multipart(
        self, upload: MultipartUpload, parts: list[MultipartPart]
    ) -> Awaitable[BlobInfo]: ...
    def blob_abort_multipart(self, upload: MultipartUpload) -> Awaitable[None]: ...
    def blob_verify_checksum_sha256(
        self, key: str, expected_hex: str
    ) -> Awaitable[bool]: ...
    def blob_presign_download(
        self, key: str, expires_seconds: float
    ) -> Awaitable[ProxyPresign]:
        """Signed URL path (under the configured `base_url`); needs
        `[blob] signing_secret`."""

    def blob_presign_upload(
        self, key: str, expires_seconds: float, max_bytes: int
    ) -> Awaitable[ProxyPresign]: ...
    def blob_presign_native_get(
        self, key: str, expires_seconds: float
    ) -> Awaitable[NativePresign]: ...
    def blob_presign_native_put(
        self,
        key: str,
        expires_seconds: float,
        content_type: Optional[str] = ...,
        metadata: Optional[dict[str, str]] = ...,
        create_only: bool = ...,
        match_version: Optional[str] = ...,
        cache_control: Optional[str] = ...,
        content_disposition: Optional[str] = ...,
        checksum_sha256: Optional[str] = ...,
        sse_algorithm: Optional[str] = ...,
        sse_kms_key_id: Optional[str] = ...,
    ) -> Awaitable[NativePresign]: ...
    def blob_verify_presign(
        self, method: str, key: str, expires_epoch: int, max_bytes: int, sig: str
    ) -> Awaitable[bool]:
        """Check a presigned request's signature/expiry when serving it."""

    def blob_content_type(self, key: str) -> Awaitable[Optional[str]]: ...
    def blob_delete(self, key: str) -> Awaitable[None]: ...
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

    def create_api_key_with(
        self,
        owner_id: str,
        label: str,
        expires_in_seconds: Optional[float] = ...,
        scopes: Optional[list[str]] = ...,
        metadata: Optional[dict[str, str]] = ...,
    ) -> Awaitable[ApiKey]: ...

    def verify_api_key(self, key: str) -> Awaitable[Optional[ApiKeyInfo]]:
        """Full non-secret key metadata, or None when unknown/revoked."""

    def revoke_api_key(self, id: str) -> Awaitable[bool]: ...
    def create_token(
        self,
        user_id: str,
        purpose: str,
        ttl_seconds: float,
        payload: Optional[bytes] = ...,
    ) -> Awaitable[str]:
        """Single-use token scoped to `purpose`, shown once; only its hash is stored."""

    def create_token_with_payload(
        self, user_id: str, purpose: str, ttl_seconds: float, payload: bytes
    ) -> Awaitable[str]: ...

    def consume_token(self, token: str, purpose: str) -> Awaitable[Optional[TokenConsumption]]:
        """Consume the token and return its user and payload, or None when unknown/expired/used
        (not an exception). A wrong `purpose` leaves a live token intact."""

    def consume_token_with_payload(
        self, token: str, purpose: str
    ) -> Awaitable[Optional[TokenConsumption]]: ...

    def schedule_at(
        self,
        when_epoch_ms: float,
        queue: str,
        payload: str,
        max_attempts: Optional[int] = ...,
        misfire_policy: Optional[str] = ...,
        max_catch_up: Optional[int] = ...,
    ) -> Awaitable[str]:
        """Enqueue `payload` onto `queue` at `when_epoch_ms` (Unix epoch, ms)."""

    def schedule_cron(
        self,
        name: str,
        expr: str,
        queue: str,
        payload: str,
        max_attempts: Optional[int] = ...,
        misfire_policy: Optional[str] = ...,
        max_catch_up: Optional[int] = ...,
    ) -> Awaitable[None]:
        """Upsert a named cron schedule (5-field expression, UTC)."""

    def schedule_cancel(self, name: str) -> Awaitable[bool]: ...
    def schedule_cancel_at(self, job_id: str) -> Awaitable[bool]: ...
    def schedule_inspect(self, name: str) -> Awaitable[Optional[ScheduleInfo]]: ...
    def schedule_pause(self, name: str) -> Awaitable[bool]: ...
    def schedule_resume(self, name: str) -> Awaitable[bool]: ...
    def scheduler_diagnostics(self) -> Awaitable[SchedulerDiagnostics]: ...
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
    concurrency: int = ...,
    heartbeat_seconds: Optional[float] = ...,
    retry_backoff_seconds: float = ...,
    drain_deadline_seconds: float = ...,
    identity: str = ...,
) -> None:
    """Managed worker loop: dequeue, run `handler`, ack on return / nack on raise,
    auto-heartbeat at `visibility_seconds / 3`, back off on dequeue errors. Runs
    until `stop` is set. `on_error` sees every failure (job=None for dequeue
    errors)."""

async def run_outbox_relay(
    client: ForgeClient,
    *,
    stop: Optional[asyncio.Event] = ...,
    batch_size: int = ...,
    claim_seconds: float = ...,
    failure_backoff_seconds: float = ...,
    idle_seconds: float = ...,
    retry_backoff_seconds: float = ...,
    on_error: Optional[Callable[[BaseException], Awaitable[None]]] = ...,
) -> None: ...

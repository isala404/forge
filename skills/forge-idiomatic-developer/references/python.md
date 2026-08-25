# Forge — Python reference

The package is `forgelib`. Raw contract methods live directly on `ForgeClient` in flat snake_case. Most are awaitable, but methods explicitly marked **synchronous** below must not be awaited. Native JSON handles also hang off the client (`forge.queue()`, `forge.kv()`, `forge.config()`, `forge.topic()`) for app payloads. Optional raw arguments default to `None`. Check the installed extension, package helpers, and `.pyi` stubs when exact behavior matters.

```python
import forgelib
forge = await forgelib.ForgeClient.init()          # reads ./forge.toml
forge = await forgelib.ForgeClient.init_from("svc/forge.toml")
```

## Key/value

| Method | Notes |
| --- | --- |
| `kv_get()` | Value as a `str`, or `None`. Lossy for binary — use bytes. |
| `kv_get_bytes()` | Value as `bytes`, or `None`. Lossless. |
| `kv_set()` | `kv_set(key, value, ttl_seconds=None, if_not_exists=None, if_exists=None)`. Returns whether it wrote. |
| `kv_set_bytes()` | `kv_set_bytes(key, value, ttl_seconds=None, if_not_exists=None)`; unlike `kv_set`, it has no `if_exists` argument. |
| `kv_mget()` | Many keys in one round-trip; per-key `str \| None`. |
| `kv_incr()` | `kv_incr(key, by)` → exact `int` (unlike Node's f64). |
| `kv_delete()` | Whether the key existed. |
| `kv_exists()` | Presence (and unexpired). |
| `kv_expire()` | `kv_expire(key, ttl_seconds)`; `False` if absent. |
| `kv_compare_and_swap()` | `kv_compare_and_swap(key, old, new)`; `old=None` means "expected absent" — a default value never matches a missing key. |
| `kv_scan()` | First page: `kv_scan(prefix, limit)` → `list[str]`. |
| `kv_scan_page()` | `kv_scan_page(prefix, cursor, limit)` → `ScanPage(keys, cursor)`; `cursor` is `None` when done. |

## Queue

| Method | Notes |
| --- | --- |
| `queue_enqueue()` | `queue_enqueue(queue, payload, max_attempts=None, dedup_id=None, delay_seconds=None)` → id. A `dedup_id` inside the configured dedup window returns the existing id (no error); the default is 300 seconds. |
| `queue_dequeue()` | `queue_dequeue(queue, visibility_seconds, wait_seconds)` → `Job \| None` (long-polls). |
| `queue_ack()` | Ack by `receipt` (idempotent). |
| `queue_nack()` | `queue_nack(receipt, retry_seconds=None)`. Raises `PreconditionError` if the receipt is unknown. |
| `queue_heartbeat()` | Extend the lease; raises `PreconditionError` if the lease was lost. |
| `queue_depth()` | Approximate point-in-time `QueueDepth(visible, in_flight, delayed)`. Pass `"<queue>.dlq"` for the dead-letter backlog. |

Settle by the delivery-unique `receipt`, never `id`. The stable id is useful for redelivery idempotency, but scope the key to the logical effect or use a domain operation id when duplicate jobs are possible.

## Pub/sub

| Method | Notes |
| --- | --- |
| `pubsub_publish()` | `pubsub_publish(topic, payload)`, fire-and-forget, at-most-once. `payload` is a `str`; subscribers receive `bytes`. |
| `pubsub_subscribe()` | Returns a subscription; `async for payload in sub:` (payloads are `bytes` — decode before parsing). |
| `pubsub_channel()` | Backend channel mapping; with Postgres pub/sub it is the `LISTEN`/`NOTIFY` channel. **Synchronous.** |

The subscription's `aclose()` unsubscribes now and stops a pending `__anext__` immediately. Publishes after `pubsub_subscribe()` returns are delivered; earlier ones are gone (no replay). Payloads must be valid UTF-8 and at most 7,000 bytes.

## Blob

Blob is bytes-native in Python: `blob_put`/`blob_get` already take and return `bytes`. There is no `blob_*_bytes` variant.

| Method | Notes |
| --- | --- |
| `blob_put()` | `blob_put(key, data, content_type=None)` — `data` is `bytes`. |
| `blob_put_object()` | Supports metadata plus `create_only` or `match_version` preconditions. |
| `blob_put_file()` | Streams a file path without buffering it in Python. |
| `blob_get()` | Object `bytes`, or `None`. |
| `blob_get_range()` | Inclusive bounded byte range, or `None`. |
| `blob_head()` | `BlobInfo` (size, content_type, etag, last_modified_ms, metadata), or `None`. |
| `blob_list()` | `blob_list(prefix, cursor, limit)` → `BlobListPage(items, cursor)`. |
| `blob_content_type()` | Stored content type, or `None`. |
| `blob_delete()` | Idempotent; returns `None` without an existence claim. |
| `blob_presign_download()` | Returns `ProxyPresign`; use `.url`. Needs `[blob].signing_secret`. |
| `blob_presign_upload()` | Returns a size-enforcing `ProxyPresign`; use `.url`. |
| `blob_presign_native_get()` / `blob_presign_native_put()` | S3-only provider presigns with required headers; native PUT has no portable size cap. |
| `blob_verify_presign()` | `blob_verify_presign(method, key, expires_epoch, max_bytes, sig)` → validity. |

Proxy presigned URLs include a version, namespace, method, key, expiry, size constraint, and signature under `[blob].base_url`. Pass the structured ticket fields to `blob_verify_presign` verbatim. Expiry or a bad signature returns `False`; an unsupported method raises `InvalidError`. Proxy and native presigned URLs are bearer credentials, so redact their query strings.

## Auth

| Method | Notes |
| --- | --- |
| `hash_password()` | argon2id PHC string to store in your users table. |
| `verify_password()` | `verify_password(plain, hash)`; a malformed stored hash raises `InvalidError`, not `False`. |
| `needs_rehash()` | After a successful verify, re-hash if `True`. Synchronous. |
| `create_session()` | `create_session(user_id, idle_seconds=None, absolute_seconds=None)` → token (shown once). |
| `validate_session()` | Token → `user_id`, or `None`. |
| `validate_session_info()` | Token → `SessionInfo`, or `None`. |
| `revoke_session()` | Log out one device (idempotent). |
| `revoke_all_sessions()` | Log out everywhere; returns the count. |
| `create_api_key()` | `create_api_key(owner_id, label)` → `ApiKey` (`secret` shown once). |
| `verify_api_key()` | Key → `ApiKeyInfo` (id, owner, label, expiry, scopes, metadata), or `None`. |
| `revoke_api_key()` | Revoke by non-secret id. |
| `create_token()` | `create_token(user_id, purpose, ttl_seconds)` → single-use token (shown once). `purpose` is any string you choose (`"password-reset"`); create/consume must match exactly. `user_id` is any opaque string handed back on consume — for pre-account flows (invites) pass your own reference id. |
| `consume_token()` | `consume_token(token, purpose)` → `user_id`, or `None` (used/expired/wrong purpose). First consume wins. |

## Rate limit

| Method | Notes |
| --- | --- |
| `rate_limit_check()` | `rate_limit_check(bucket, key, max, per_seconds, fail_open=None, algo=None)` → `Decision`. `algo` is `"token_bucket"` (default) or `"sliding_window"`. `per_seconds` must be ≥ 1 (a sub-second window raises `InvalidError`). |

`Decision`: `allowed`, `limit`, `remaining`, `reset_after_seconds`, `retry_after_seconds`. Run credential limits before password work and choose `fail_open` deliberately. Use `False` when bypass creates unacceptable security/financial risk; use `True` with monitoring and defense in depth when availability takes precedence.

## Schedule

| Method | Notes |
| --- | --- |
| `schedule_cron()` | `schedule_cron(name, expr, queue, payload, max_attempts=None)`; upsert by name. |
| `schedule_at()` | `schedule_at(when_epoch_ms, queue, payload, max_attempts=None)` → future id. `payload` is a raw string, passed through — `json.dumps` it yourself if a JSON-handle worker consumes the queue. |
| `schedule_cancel()` | Cancel a cron by name. |
| `schedule_cancel_at()` | Cancel a one-shot by the id `schedule_at` returned. |
| `schedule_list()` | `schedule_list(cursor=None, limit=None)` → `SchedulePage(items, cursor)`. |
| `run_scheduler_once()` | Fire all due schedules once; returns jobs enqueued. Call on an interval. |

## Config and flags

| Method | Notes |
| --- | --- |
| `config_get()` | Resolve a value: env `FORGE_CFG_<KEY>` > store > `None`. |
| `config_set()` | `config_set(key, value)`. |
| `config_delete()` | Delete a stored value; env `FORGE_CFG_<KEY>` still shadows reads. |
| `flag()` | `flag(key, default, targeting_key=None)`. Never raises; falls back to the default. |
| `set_flag_percent()` | Percentage rollout, `0..=100`. Bucketing is a stable hash of `(flag, targeting_key)` — deterministic across processes; without a `targeting_key`, a percent rule returns the caller default. |
| `set_flag_on()` / `set_flag_off()` | Always-on / always-off. |
| `set_flag_allow_list()` | `set_flag_allow_list(key, entries)`. |
| `set_flag_value()` / `flag_details()` | Store typed JSON and return its value type, stable variant, reason, and default/error reason. |
| `delete_flag()` | Delete a flag rule; later `flag()` calls use the caller default. |

## Client

| Method | Notes |
| --- | --- |
| `backend_capabilities()` | Static provider and durability capabilities for each primitive. **Synchronous.** |
| `postgres_url()` | Resolved Forge system DSN. Contains credentials. Use server-side to reach embedded Postgres from another process or intentionally seed an application-owned pool; choose production isolation deliberately. **Synchronous.** |
| `maintain()` | One housekeeping sweep. Call on an interval alongside `run_scheduler_once`. |

## Native JSON handles

Bind a JSON codec once from the main package (the codec defaults to `json`; pass `loads=`/`dumps=` to swap).

```python
import forgelib

forge = await forgelib.ForgeClient.init()

emails = forge.queue("emails")
await emails.enqueue({"to": "a@b.c"}, max_attempts=5)

profile = forge.kv(f"user:{user_id}")
await profile.set(value, ttl_seconds=3600)
```

- `Queue` from `forge.queue(name)`: `enqueue()`, `dequeue()`, `ack()`, `nack()`, `heartbeat()`, `depth()`, `worker()`. A handle/worker `job.payload` is ALREADY codec-decoded (a dict) — `json.loads`-ing it again raises.
- `KvKey` from `forge.kv(key)`: `get()`, `get_or_default(default)`, `set()`, `delete()`, `exists()`, `expire()`, `compare_and_swap()`.
- `ConfigKey` from `forge.config(key, default)`: `get()`, `get_or_default()`, `set()`.
- `Topic` from `forge.topic(name)`: asynchronous `publish()`, async-generator `subscribe()` (`async for event in topic.subscribe():`), and synchronous `channel()`.
- `forge.worker(queue, handler, *, stop=..., visibility_seconds=30.0, wait_seconds=20.0)` / `run_worker(...)` — managed coroutine. Start and retain an actual task with `asyncio.create_task`. `await forge.close(deadline_seconds)` cancels an active handler task, sets `job.cancelled`, releases its lease, awaits helper cleanup, and then closes native resources.
- `forge_error_code(exc)` / `forge_error_retryable(exc)` — exceptions are named code + `Error` (`LimitError` → code `"Limit"`) and every instance carries a `retryable` attribute.

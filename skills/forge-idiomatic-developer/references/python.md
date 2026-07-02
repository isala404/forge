# Forge — Python reference

The package is `forgelib`. Every method lives directly on `ForgeClient` in flat
snake_case and is awaitable (`await` all of them, including `init`). Optional
arguments default to `None`. Verified against `bindings/python/src/lib.rs`.

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
| `kv_set()` | `kv_set(key, value, ttl_seconds=None, if_not_exists=None)`. Returns whether it wrote. |
| `kv_set_bytes()` | Same, with a `bytes` value. |
| `kv_mget()` | Many keys in one round-trip; per-key `str \| None`. |
| `kv_incr()` | `kv_incr(key, by)` → exact `int` (unlike Node's f64). |
| `kv_delete()` | Whether the key existed. |
| `kv_exists()` | Presence (and unexpired). |
| `kv_expire()` | `kv_expire(key, ttl_seconds)`; `False` if absent. |
| `kv_compare_and_swap()` | `kv_compare_and_swap(key, old, new)`; `old=None` means "expected absent". |
| `kv_scan()` | First page: `kv_scan(prefix, limit)` → `list[str]`. |
| `kv_scan_page()` | `kv_scan_page(prefix, cursor, limit)` → `ScanPage(keys, cursor)`. |

## Queue

| Method | Notes |
| --- | --- |
| `queue_enqueue()` | `queue_enqueue(queue, payload, max_attempts=None, dedup_id=None, delay_seconds=None)` → id. |
| `queue_dequeue()` | `queue_dequeue(queue, visibility_seconds, wait_seconds)` → `Job \| None` (long-polls). |
| `queue_ack()` | Ack by `receipt` (idempotent). |
| `queue_nack()` | `queue_nack(receipt, retry_seconds=None)`. Raises `Precondition` if the receipt is unknown. |
| `queue_heartbeat()` | Extend the lease; raises `Precondition` if the lease was lost. |
| `queue_depth()` | `QueueDepth(visible, in_flight, delayed)`. Pass `"<queue>.dlq"` for the dead-letter backlog. |

Settle by the delivery-unique `receipt`, never `id` (the `id` is your idempotency
key — stable across redeliveries).

## Pub/sub

| Method | Notes |
| --- | --- |
| `pubsub_publish()` | `pubsub_publish(topic, payload)`, fire-and-forget, at-most-once. |
| `pubsub_subscribe()` | Returns a subscription; `async for payload in sub:` (payloads are `bytes`). |
| `pubsub_channel()` | The Postgres `LISTEN`/`NOTIFY` channel a topic maps to. |

## Blob

Blob is bytes-native in Python: `blob_put`/`blob_get` already take and return
`bytes`. There is no `blob_*_bytes` variant.

| Method | Notes |
| --- | --- |
| `blob_put()` | `blob_put(key, data, content_type=None)` — `data` is `bytes`. |
| `blob_put_object()` | `blob_put_object(key, data, content_type=None, metadata=None)` for user metadata. |
| `blob_get()` | Object `bytes`, or `None`. |
| `blob_head()` | `BlobInfo` (size, content_type, etag, last_modified_ms, metadata), or `None`. |
| `blob_list()` | `blob_list(prefix, cursor, limit)` → `BlobListPage(items, cursor)`. |
| `blob_content_type()` | Stored content type, or `None`. |
| `blob_delete()` | Whether it existed. |
| `blob_presign_download()` | `blob_presign_download(key, expires_seconds)` — needs `[blob].signing_secret`. |
| `blob_presign_upload()` | `blob_presign_upload(key, expires_seconds, max_bytes)` — needs the secret. |
| `blob_verify_presign()` | `blob_verify_presign(method, key, expires_epoch, max_bytes, sig)` → validity. |

## Auth

| Method | Notes |
| --- | --- |
| `hash_password()` | argon2id PHC string to store in your users table. |
| `verify_password()` | `verify_password(plain, hash)`, constant-time. |
| `needs_rehash()` | After a successful verify, re-hash if `True`. Synchronous. |
| `create_session()` | `create_session(user_id, idle_seconds=None, absolute_seconds=None)` → token (shown once). |
| `validate_session()` | Token → `user_id`, or `None`. |
| `validate_session_info()` | Token → `SessionInfo`, or `None`. |
| `revoke_session()` | Log out one device (idempotent). |
| `revoke_all_sessions()` | Log out everywhere; returns the count. |
| `create_api_key()` | `create_api_key(owner_id, label)` → `ApiKey` (`secret` shown once). |
| `verify_api_key()` | Key → `owner_id`, or `None`. |
| `verify_api_key_info()` | Key → `ApiKeyInfo`, or `None`. |
| `revoke_api_key()` | Revoke by non-secret id. |

## Rate limit

| Method | Notes |
| --- | --- |
| `rate_limit_check()` | `rate_limit_check(bucket, key, max, per_seconds, fail_open=None, algo=None)` → `Decision`. `algo` is `"token_bucket"` (default) or `"sliding_window"`. |

`Decision`: `allowed`, `limit`, `remaining`, `reset_after_seconds`, `retry_after_seconds`.

## Schedule

| Method | Notes |
| --- | --- |
| `schedule_cron()` | `schedule_cron(name, expr, queue, payload, max_attempts=None)`; upsert by name. |
| `schedule_at()` | `schedule_at(when_epoch_ms, queue, payload, max_attempts=None)` → future id. |
| `schedule_cancel()` | Cancel a cron by name. |
| `schedule_cancel_at()` | Cancel a one-shot by the id `schedule_at` returned. |
| `schedule_list()` | `schedule_list(cursor=None, limit=None)` → `SchedulePage(items, cursor)`. |
| `run_scheduler_once()` | Fire all due schedules once; returns jobs enqueued. Call on an interval. |

## Config and flags

| Method | Notes |
| --- | --- |
| `config_get()` | Resolve a value: env `FORGE_CFG_<KEY>` > store > `None`. |
| `config_set()` | `config_set(key, value)`. |
| `flag()` | `flag(key, default, targeting_key=None)`. Never raises; falls back to the default. |
| `set_flag_percent()` | Percentage rollout, `0..=100`. |
| `set_flag_on()` / `set_flag_off()` | Always-on / always-off. |
| `set_flag_allow_list()` | `set_flag_allow_list(key, entries)`. |

## Client

| Method | Notes |
| --- | --- |
| `backend_report()` | Which provider powers each primitive. Synchronous. |
| `maintain()` | One housekeeping sweep. Call on an interval alongside `run_scheduler_once`. |

## Typed layer — `forgelib.typed`

Bind a JSON codec once (the codec defaults to `json`; pass `loads=`/`dumps=` to swap).

```python
from forgelib.typed import (
    TypedQueue, TypedKvKey, TypedConfigKey, TypedTopic,
    run_worker, forge_error_code, forge_error_retryable,
)

emails = TypedQueue(forge, "emails")
await emails.enqueue({"to": "a@b.c"}, max_attempts=5)

profile = TypedKvKey(forge, f"user:{user_id}")
await profile.set(value, ttl_seconds=3600)
```

- `TypedQueue`: `enqueue()`, `dequeue()`, `ack()`, `nack()`, `heartbeat()`, `depth()`.
- `TypedKvKey`: `get()`, `set()`, `delete()`.
- `TypedConfigKey`: `get()`, `get_or_default()`, `set()`.
- `TypedTopic`: `publish()`, `subscribe()` (`async for event in topic.subscribe():`).
- `run_worker(client, queue, handler, *, stop=..., visibility_seconds=30.0, wait_seconds=20.0)` — managed loop; set the `stop` event to drain.
- `forge_error_code(exc)` / `forge_error_retryable(exc)` — the code is the exception class name.
</content>

from __future__ import annotations

import asyncio
import json

from forgelib import encode_invalidation_event

from .utils import (
    CLICKS_QUEUE,
    EXPIRE_QUEUE,
    click_topic,
    clicks_key,
    link_slug_key,
    owner_key,
    qr_key,
)


async def delete_link(forge, slug: str) -> None:
    """Idempotent routine used by the DELETE route and the expire worker."""
    raw = await forge.kv_get(link_slug_key(slug))
    if raw is None:
        return

    rec = json.loads(raw)
    owner_id = rec.get("ownerId")
    if owner_id:
        raw_owner = await forge.kv_get(owner_key(owner_id))
        owned: list = json.loads(raw_owner or "[]")
        next_owned = [item for item in owned if item.get("slug") != slug]
        await forge.kv_set(
            owner_key(owner_id),
            json.dumps(next_owned, separators=(",", ":")),
        )

    await forge.kv_delete(link_slug_key(slug))
    await forge.kv_delete(clicks_key(slug))
    await forge.blob_delete(qr_key(slug))


async def clicks_worker(forge, stop: asyncio.Event) -> None:
    """Publish a lossy hint; browsers refetch the authoritative click count."""
    async def handle(job) -> None:
        slug = job.payload["slug"]
        raw_count = await forge.kv_get(clicks_key(slug))
        total = int(raw_count) if raw_count is not None else 0
        hint = encode_invalidation_event({
            "schema_version": 1,
            "tags": [f"links/{slug}"],
            "query_keys": [["link", slug]],
            "revision": str(total),
        })
        await forge.pubsub_publish(click_topic(slug), hint.decode())

    await forge.worker(
        CLICKS_QUEUE,
        handle,
        concurrency=8,
        visibility_seconds=30,
        heartbeat_seconds=10,
        retry_backoff_seconds=0.25,
        drain_deadline_seconds=30,
        identity="clicks-worker",
        concurrency_limit_per_key=1,
        wait_seconds=1.0,
        stop=stop,
    )


async def expire_worker(forge, stop: asyncio.Event) -> None:
    """Drain the expire queue; delete each link when its scheduled TTL fires."""
    async def handle(job) -> None:
        await delete_link(forge, job.payload["slug"])

    await forge.worker(
        EXPIRE_QUEUE,
        handle,
        concurrency=4,
        visibility_seconds=30,
        heartbeat_seconds=10,
        retry_backoff_seconds=0.25,
        drain_deadline_seconds=30,
        identity="expire-worker",
        wait_seconds=5.0,
        stop=stop,
    )


async def scheduler_loop(forge, stop: asyncio.Event) -> None:
    """Every 30 s: fire due scheduled jobs then sweep expired primitives."""
    while not stop.is_set():
        try:
            await forge.run_scheduler_once()
            diagnostics = await forge.scheduler_diagnostics()
            if diagnostics.due_count:
                print(
                    "scheduler remains behind: "
                    f"due={diagnostics.due_count} lag_ms={diagnostics.lag_ms}",
                    flush=True,
                )
            await forge.maintain()
        except Exception as exc:  # noqa: BLE001
            print(f"scheduler loop error: {exc}", flush=True)
        try:
            await asyncio.wait_for(stop.wait(), timeout=30.0)
        except TimeoutError:
            pass

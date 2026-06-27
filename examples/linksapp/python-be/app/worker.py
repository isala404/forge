"""Background workers: clicks drain, link expiry, and the scheduler/maintenance loop."""

from __future__ import annotations

import asyncio
import json

import forge_py

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
    """Drain the clicks queue; for each job publish the updated count via pubsub."""
    while not stop.is_set():
        try:
            job = await forge.queue_dequeue(CLICKS_QUEUE, 30.0, 1.0)
        except forge_py.ForgeError:
            await asyncio.sleep(0.2)
            continue
        if job is None:
            continue
        try:
            slug = json.loads(job.payload)["slug"]
            raw_count = await forge.kv_get(clicks_key(slug))
            total = int(raw_count) if raw_count is not None else 0
            await forge.pubsub_publish(
                click_topic(slug),
                json.dumps({"slug": slug, "clicks": total}, separators=(",", ":")),
            )
            await forge.queue_ack(job.receipt)
        except Exception as exc:  # noqa: BLE001
            print(f"clicks worker error: {exc}", flush=True)
            try:
                await forge.queue_nack(job.receipt, 5.0)
            except forge_py.ForgeError:
                pass


async def expire_worker(forge, stop: asyncio.Event) -> None:
    """Drain the expire queue; delete each link when its scheduled TTL fires."""
    while not stop.is_set():
        try:
            job = await forge.queue_dequeue(EXPIRE_QUEUE, 30.0, 5.0)
        except forge_py.ForgeError:
            await asyncio.sleep(0.2)
            continue
        if job is None:
            continue
        try:
            slug = json.loads(job.payload)["slug"]
            await delete_link(forge, slug)
            await forge.queue_ack(job.receipt)
        except Exception as exc:  # noqa: BLE001
            print(f"expire worker error: {exc}", flush=True)
            try:
                await forge.queue_nack(job.receipt, 5.0)
            except forge_py.ForgeError:
                pass


async def scheduler_loop(forge, stop: asyncio.Event) -> None:
    """Every 30 s: fire due scheduled jobs then sweep expired primitives."""
    while not stop.is_set():
        try:
            await forge.run_scheduler_once()
            await forge.maintain()
        except Exception as exc:  # noqa: BLE001
            print(f"scheduler loop error: {exc}", flush=True)
        try:
            await asyncio.wait_for(stop.wait(), timeout=30.0)
        except TimeoutError:
            pass

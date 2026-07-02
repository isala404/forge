from __future__ import annotations

import asyncio
import uuid
from datetime import UTC, datetime

import forgelib

from . import db

FANOUT_QUEUE = "fanout"
FAIL_QUEUE = "fail"
REAP_QUEUE = "reap"
RECONCILE_LIMIT = 100


async def fanout_worker(forge, pool, stop: asyncio.Event) -> None:
    async def handle(job) -> None:
        mid = uuid.UUID(job.payload["message_id"])
        msg = await db.message(pool, mid)
        if msg is not None:
            for recipient in await db.other_member_ids(pool, msg["chat_id"], msg["sender_id"]):
                await db.mark_delivered(pool, mid, recipient)

    await forge.worker(FANOUT_QUEUE, handle, wait_seconds=1.0, stop=stop)


async def reap_worker(forge, pool, stop: asyncio.Event) -> None:
    async def handle(job) -> None:
        mid = uuid.UUID(job.payload["message_id"])
        row = await db.message_for_reap(pool, mid)
        if row is None:
            return
        expires_at = row["expires_at"]
        if expires_at is None or expires_at > datetime.now(UTC):
            return
        if row["media_key"]:
            await forge.blob_delete(row["media_key"])
        await db.delete_expired_message(pool, mid)

    await forge.worker(REAP_QUEUE, handle, wait_seconds=1.0, stop=stop)


async def fail_worker(forge, stop: asyncio.Event) -> None:
    async def fail(_job) -> None:
        raise RuntimeError("intentional failure for DLQ demo")

    await forge.worker(FAIL_QUEUE, fail, wait_seconds=1.0, stop=stop, loads=lambda raw: raw)


async def reconcile_once(forge, pool) -> None:
    """Heal work whose post-commit enqueue/schedule was dropped. The app and Forge
    hold separate pools, so the send tx can't enlist the enqueue; this bounded sweep
    is the safety net. Both repairs are idempotent, so running them every tick is safe."""
    # Dropped reaps: delete due disappearing messages (blob best-effort here; a missed
    # blob is retried next tick, and we'd rather make progress than block the sweep).
    for row in await db.due_disappearing_messages(pool, RECONCILE_LIMIT):
        if row["media_key"]:
            try:
                await forge.blob_delete(row["media_key"])
            except forgelib.ForgeError:
                pass
        await db.delete_expired_message(pool, row["id"])
    # Dropped fanout: re-enqueue for messages whose receipts were never delivered.
    # Fanout is idempotent on mark_delivered, so a duplicate job is harmless.
    for mid in await db.undelivered_message_ids(pool, RECONCILE_LIMIT):
        await forge.queue(FANOUT_QUEUE).enqueue({"message_id": str(mid)}, dedup_id=str(mid))


async def scheduler_loop(forge, pool, stop: asyncio.Event, interval: float) -> None:
    while not stop.is_set():
        try:
            await forge.run_scheduler_once()
        except forgelib.ForgeError:
            pass
        try:
            await reconcile_once(forge, pool)
        except Exception:
            pass
        try:
            # Sweep expired Forge storage rows.
            await forge.maintain()
        except forgelib.ForgeError:
            pass
        try:
            await asyncio.wait_for(stop.wait(), timeout=interval)
        except TimeoutError:
            pass


def start_workers(forge, pool, stop: asyncio.Event, scheduler_interval: float) -> list:
    return [
        asyncio.create_task(fanout_worker(forge, pool, stop)),
        asyncio.create_task(reap_worker(forge, pool, stop)),
        asyncio.create_task(fail_worker(forge, stop)),
        asyncio.create_task(scheduler_loop(forge, pool, stop, scheduler_interval)),
    ]

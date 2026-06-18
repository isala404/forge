"""In-process background workers, run as asyncio tasks alongside the API.

- fanout: marks receipts delivered, idempotent on message id.
- reap: deletes a disappearing message's blob + row when its scheduled job fires.
- fail: always nacks to drive the DLQ demo.
- scheduler: fires due `at` jobs into their queues, then runs a reconciliation sweep
  that heals fanout/reap work whose post-commit enqueue was dropped."""

from __future__ import annotations

import asyncio
import json
import uuid
from datetime import UTC, datetime

import forge_py

from . import db

FANOUT_QUEUE = "fanout"
FAIL_QUEUE = "fail"
REAP_QUEUE = "reap"
RECONCILE_LIMIT = 100


async def fanout_worker(forge, pool, stop: asyncio.Event) -> None:
    while not stop.is_set():
        try:
            job = await forge.queue_dequeue(FANOUT_QUEUE, 30.0, 1.0)
        except forge_py.ForgeError:
            await asyncio.sleep(0.2)
            continue
        if job is None:
            continue
        payload = job.payload
        try:
            mid = uuid.UUID(json.loads(payload)["message_id"])
            msg = await db.message(pool, mid)
            if msg is not None:
                for recipient in await db.other_member_ids(pool, msg["chat_id"], msg["sender_id"]):
                    await db.mark_delivered(pool, mid, recipient)
            await forge.queue_ack(job.receipt)
        except Exception:
            try:
                await forge.queue_nack(job.receipt, 5.0)
            except forge_py.ForgeError:
                pass


async def reap_worker(forge, pool, stop: asyncio.Event) -> None:
    while not stop.is_set():
        try:
            job = await forge.queue_dequeue(REAP_QUEUE, 30.0, 1.0)
        except forge_py.ForgeError:
            await asyncio.sleep(0.2)
            continue
        if job is None:
            continue
        payload = job.payload
        try:
            mid = uuid.UUID(json.loads(payload)["message_id"])
            row = await db.message_for_reap(pool, mid)
            if row is None:
                # Already gone (a previous reap or the reconciliation sweep handled it).
                await forge.queue_ack(job.receipt)
                continue
            expires_at = row["expires_at"]
            if expires_at is None or expires_at > datetime.now(UTC):
                # Recalled (disappearing toggled off) or not yet due: leave the row.
                await forge.queue_ack(job.receipt)
                continue
            # Delete the blob first and let any failure propagate (nack): an at-least-once
            # redelivery is cheaper than orphaning the object behind a deleted row.
            if row["media_key"]:
                await forge.blob_delete(row["media_key"])
            await db.delete_expired_message(pool, mid)
            await forge.queue_ack(job.receipt)
        except Exception:
            try:
                await forge.queue_nack(job.receipt, 5.0)
            except forge_py.ForgeError:
                pass


async def fail_worker(forge, stop: asyncio.Event) -> None:
    while not stop.is_set():
        try:
            job = await forge.queue_dequeue(FAIL_QUEUE, 30.0, 1.0)
        except forge_py.ForgeError:
            await asyncio.sleep(0.2)
            continue
        if job is None:
            continue
        try:
            # Nack with retry_in=0 so it redelivers immediately and exhausts attempts
            # into `fail.dlq` quickly.
            await forge.queue_nack(job.receipt, 0.0)
        except forge_py.ForgeError:
            pass


async def reconcile_once(forge, pool) -> None:
    """Heal work whose post-commit enqueue/schedule was dropped. The app and Forge
    hold separate pools, so the send tx can't enlist the enqueue; this bounded sweep
    is the safety net. Both repairs are idempotent, so running them every tick is safe."""
    # Dropped reaps: delete due disappearing messages (blob best-effort here — a missed
    # blob is retried next tick, and we'd rather make progress than block the sweep).
    for row in await db.due_disappearing_messages(pool, RECONCILE_LIMIT):
        if row["media_key"]:
            try:
                await forge.blob_delete(row["media_key"])
            except forge_py.ForgeError:
                pass
        await db.delete_expired_message(pool, row["id"])
    # Dropped fanout: re-enqueue for messages whose receipts were never delivered.
    # Fanout is idempotent on mark_delivered, so a duplicate job is harmless.
    for mid in await db.undelivered_message_ids(pool, RECONCILE_LIMIT):
        await forge.queue_enqueue(
            FANOUT_QUEUE, json.dumps({"message_id": str(mid)}), dedup_id=str(mid)
        )


async def scheduler_loop(forge, pool, stop: asyncio.Event, interval: float) -> None:
    while not stop.is_set():
        try:
            await forge.run_scheduler_once()
        except forge_py.ForgeError:
            pass
        try:
            await reconcile_once(forge, pool)
        except Exception:
            pass
        try:
            # Sweep expired kv/queue/ratelimit/auth rows (forge-py now exposes maintain).
            await forge.maintain()
        except forge_py.ForgeError:
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

from __future__ import annotations

import asyncio
import json
import uuid
from collections.abc import AsyncIterator
from datetime import UTC, datetime, timedelta

import forgelib
import strawberry
from strawberry.types import Info

from .. import db
from .helpers import (
    FANOUT_QUEUE,
    INT32_MAX,
    REAP_QUEUE,
    SEND_LIMIT,
    UPLOAD_LIMIT,
    chat_topic,
    disappearing_secs,
    gqlerr,
    map_forge,
    max_upload_bytes,
    parse_id,
    publish_event,
    require_member,
    require_user,
    sub_events,
)
from .types import Chat, Message, UploadTicket, chat_from_row, message_from_row


@strawberry.type
class MessageQuery:
    @strawberry.field
    async def messages(
        self,
        info: Info,
        chat_id: strawberry.ID,
        before: datetime | None = None,
        limit: int = 50,
    ) -> list[Message]:
        u = await require_user(info)
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        limit = max(1, min(200, limit))
        rows = await db.list_messages(info.context["pool"], cid, before, limit)
        return [message_from_row(r) for r in rows]


@strawberry.type
class MessageMutation:
    @strawberry.mutation(
        description="Hand the client a presigned PUT URL to upload an attachment directly to"
        " blob storage."
    )
    async def request_upload(self, info: Info, chat_id: strawberry.ID) -> UploadTicket:
        u = await require_user(info)
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        forge = info.context["forge"]
        try:
            decision = await forge.rate_limit_check(
                "upload", str(u["id"]), UPLOAD_LIMIT[0], UPLOAD_LIMIT[1], fail_open=False
            )
        except forgelib.ForgeError as e:
            raise map_forge(e) from e
        if not decision.allowed:
            raise gqlerr("LIMIT", "too many upload requests; slow down")
        mb = await max_upload_bytes(info)
        key = f"media/{cid}/{uuid.uuid4()}"
        url = await forge.blob_presign_upload(key, 600, mb)
        return UploadTicket(key=key, upload_url=url, max_bytes=min(mb, INT32_MAX))

    @strawberry.mutation
    async def send_message(
        self,
        info: Info,
        chat_id: strawberry.ID,
        body: str,
        media_key: str | None = None,
        idempotency_key: str | None = None,
    ) -> Message:
        u = await require_user(info)
        pool = info.context["pool"]
        forge = info.context["forge"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])

        try:
            decision = await forge.rate_limit_check(
                "send", str(u["id"]), SEND_LIMIT[0], SEND_LIMIT[1], fail_open=True
            )
        except forgelib.ForgeError as e:
            raise map_forge(e) from e
        if not decision.allowed:
            raise gqlerr("LIMIT", "you are sending too fast; slow down")
        if not body.strip() and media_key is None:
            raise gqlerr("INVALID", "message must have text or an attachment")

        content_type = None
        if media_key:
            if not media_key.startswith(f"media/{cid}/"):
                raise gqlerr("INVALID", "media_key does not belong to this chat")
            content_type = await forge.blob_content_type(media_key)

        chat_row = await db.chat(pool, cid)
        expires_at = None
        if chat_row and chat_row["disappearing_seconds"]:
            expires_at = datetime.now(UTC) + timedelta(seconds=chat_row["disappearing_seconds"])

        msg_id = uuid.uuid4()

        # Client-supplied idempotency: a resend after a lost response returns the
        # original message instead of inserting a duplicate.
        if idempotency_key:
            key = f"idem:send:{u['id']}:{idempotency_key}"
            won = await forge.kv_set(key, str(msg_id), ttl_seconds=86400, if_not_exists=True)
            if not won:
                for _ in range(5):
                    existing = await forge.kv_get(key)
                    if existing is not None:
                        row = await db.message(pool, uuid.UUID(existing))
                        if row:
                            return message_from_row(row)
                    await asyncio.sleep(0.05)
                raise gqlerr("INVALID", "duplicate send in progress; retry")

        await db.insert_message_with_receipts(
            pool, msg_id, cid, u["id"], body, media_key, content_type, expires_at
        )

        await publish_event(info, chat_topic(cid), {"type": "message", "message_id": str(msg_id)})
        # Dedup on the message id so a retried sendMessage resolver can't double-enqueue.
        await forge.queue(FANOUT_QUEUE).enqueue({"message_id": str(msg_id)}, dedup_id=str(msg_id))

        if expires_at is not None:
            await forge.schedule_at(
                expires_at.timestamp() * 1000, REAP_QUEUE, json.dumps({"message_id": str(msg_id)})
            )

        row = await db.message(pool, msg_id)
        if not row:
            raise gqlerr("BACKEND", "message vanished after insert")
        return message_from_row(row)

    @strawberry.mutation(
        description="Turn disappearing messages on/off for a chat (forge schedule)."
    )
    async def set_disappearing(self, info: Info, chat_id: strawberry.ID, enabled: bool) -> Chat:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        await db.set_disappearing(pool, cid, disappearing_secs() if enabled else None)
        if not enabled:
            # Clearing expires_at on not-yet-expired messages turns their already-scheduled
            # reap jobs into no-ops.
            await db.clear_pending_expirations(pool, cid)
        row = await db.chat(pool, cid)
        if not row:
            raise gqlerr("NOT_FOUND", "chat not found")
        return chat_from_row(row)


@strawberry.type
class MessageSubscription:
    @strawberry.subscription(description="New messages in a chat (live).")
    async def message_added(self, info: Info, chat_id: strawberry.ID) -> AsyncIterator[Message]:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        async for ev in sub_events(info, chat_topic(cid)):
            if ev.get("type") != "message":
                continue
            row = await db.message(pool, uuid.UUID(ev["message_id"]))
            if row:
                yield message_from_row(row)

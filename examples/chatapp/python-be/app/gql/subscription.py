from __future__ import annotations

import json
import time
import uuid
from collections.abc import AsyncIterator

import strawberry
from strawberry.types import Info

from .. import db
from .helpers import (
    PRESENCE_TOPIC,
    chat_topic,
    parse_id,
    require_member,
    require_user,
)
from .types import (
    Message,
    Receipt,
    TypingEvent,
    User,
    message_from_row,
    receipt_from_row,
    user_from_row,
)

# How often a long-lived subscription re-checks that its principal still validates.
REAUTH_INTERVAL_SECS = 60.0


async def _events(info: Info, topic: str) -> AsyncIterator[dict]:
    sub = await info.context["forge"].pubsub_subscribe(topic)
    # Re-validate the principal at most once per interval so a revoked session ends the
    # stream instead of streaming forever. Anonymous (token-less) streams skip this; they
    # never get past require_user anyway.
    next_check = time.monotonic() + REAUTH_INTERVAL_SECS
    revalidate = info.context.has_token()
    async for payload in sub:
        if revalidate and time.monotonic() >= next_check:
            if await info.context.revalidate() is None:
                return
            next_check = time.monotonic() + REAUTH_INTERVAL_SECS
        try:
            yield json.loads(payload)
        except (ValueError, TypeError):
            continue


@strawberry.type
class Subscription:
    @strawberry.subscription(description="New messages in a chat (live).")
    async def message_added(self, info: Info, chat_id: strawberry.ID) -> AsyncIterator[Message]:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        async for ev in _events(info, chat_topic(cid)):
            if ev.get("type") != "message":
                continue
            row = await db.message(pool, uuid.UUID(ev["message_id"]))
            if row:
                yield message_from_row(row)

    @strawberry.subscription
    async def typing(self, info: Info, chat_id: strawberry.ID) -> AsyncIterator[TypingEvent]:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        me_id = u["id"]
        async for ev in _events(info, chat_topic(cid)):
            if ev.get("type") != "typing":
                continue
            uid = uuid.UUID(ev["user_id"])
            if uid == me_id:
                continue
            row = await db.users_by_ids(pool, [uid])
            if row:
                yield TypingEvent(user=user_from_row(row[0]), typing=bool(ev.get("typing")))

    @strawberry.subscription
    async def receipt_changed(self, info: Info, chat_id: strawberry.ID) -> AsyncIterator[Receipt]:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        async for ev in _events(info, chat_topic(cid)):
            if ev.get("type") != "receipt":
                continue
            mid = uuid.UUID(ev["message_id"])
            target = uuid.UUID(ev["user_id"])
            rows = await db.receipts_by_message_ids(pool, [mid])
            match = next((r for r in rows if r["user_id"] == target), None)
            if match:
                yield receipt_from_row(match)

    @strawberry.subscription
    async def presence_changed(
        self, info: Info, user_ids: list[strawberry.ID]
    ) -> AsyncIterator[User]:
        await require_user(info)
        pool = info.context["pool"]
        wanted = {parse_id(x) for x in user_ids}
        async for ev in _events(info, PRESENCE_TOPIC):
            if ev.get("type") != "presence":
                continue
            uid = uuid.UUID(ev["user_id"])
            if uid not in wanted:
                continue
            row = await db.users_by_ids(pool, [uid])
            if row:
                yield user_from_row(row[0])

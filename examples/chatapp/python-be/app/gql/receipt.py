from __future__ import annotations

import uuid
from collections.abc import AsyncIterator

import strawberry
from strawberry.types import Info

from .. import db
from .helpers import chat_topic, parse_id, publish_event, require_member, require_user, sub_events
from .types import Receipt, receipt_from_row


@strawberry.type
class ReceiptMutation:
    @strawberry.mutation
    async def mark_read(
        self, info: Info, chat_id: strawberry.ID, message_id: strawberry.ID
    ) -> bool:
        u = await require_user(info)
        cid = parse_id(chat_id)
        mid = parse_id(message_id)
        await require_member(info, cid, u["id"])
        # mark_read sets receipts.read_at, the single source of truth for unread.
        updated = await db.mark_read(info.context["pool"], cid, mid, u["id"])
        if updated:
            await publish_event(
                info,
                chat_topic(cid),
                {"type": "receipt", "message_id": str(mid), "user_id": str(u["id"])},
            )
        return True


@strawberry.type
class ReceiptSubscription:
    @strawberry.subscription
    async def receipt_changed(
        self, info: Info, chat_id: strawberry.ID
    ) -> AsyncIterator[Receipt]:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        async for ev in sub_events(info, chat_topic(cid)):
            if ev.get("type") != "receipt":
                continue
            mid = uuid.UUID(ev["message_id"])
            target = uuid.UUID(ev["user_id"])
            rows = await db.receipts_by_message_ids(pool, [mid])
            match = next((r for r in rows if r["user_id"] == target), None)
            if match:
                yield receipt_from_row(match)

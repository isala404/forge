from __future__ import annotations

import uuid
from collections.abc import AsyncIterator

import strawberry
from strawberry.types import Info

from .. import db
from .helpers import (
    PRESENCE_TOPIC,
    chat_topic,
    loaders,
    parse_id,
    publish_event,
    require_member,
    require_user,
    sub_events,
)
from .types import TypingEvent, User, user_from_row


@strawberry.type
class PresenceQuery:
    @strawberry.field
    async def presence(self, info: Info, user_ids: list[strawberry.ID]) -> list[User]:
        await require_user(info)
        ids = [parse_id(x) for x in user_ids]
        rows = await loaders(info)["users"].load_many(ids)
        return [user_from_row(r) for r in rows if r]


@strawberry.type
class PresenceMutation:
    @strawberry.mutation
    async def set_typing(self, info: Info, chat_id: strawberry.ID, typing: bool) -> bool:
        u = await require_user(info)
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        # The indicator rides the pubsub 'typing' event; nothing reads a kv typing key.
        await publish_event(
            info,
            chat_topic(cid),
            {"type": "typing", "user_id": str(u["id"]), "typing": typing},
        )
        return True

    @strawberry.mutation
    async def heartbeat(self, info: Info) -> bool:
        u = await require_user(info)
        forge = info.context["forge"]
        await forge.kv_set(f"online:{u['id']}", "1", info.context["presence_ttl"])
        await publish_event(
            info,
            PRESENCE_TOPIC,
            {"type": "presence", "user_id": str(u["id"]), "online": True},
        )
        return True


@strawberry.type
class PresenceSubscription:
    @strawberry.subscription
    async def typing(self, info: Info, chat_id: strawberry.ID) -> AsyncIterator[TypingEvent]:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        me_id = u["id"]
        async for ev in sub_events(info, chat_topic(cid)):
            if ev.get("type") != "typing":
                continue
            uid = uuid.UUID(ev["user_id"])
            if uid == me_id:
                continue
            row = await db.users_by_ids(pool, [uid])
            if row:
                yield TypingEvent(user=user_from_row(row[0]), typing=bool(ev.get("typing")))

    @strawberry.subscription
    async def presence_changed(
        self, info: Info, user_ids: list[strawberry.ID]
    ) -> AsyncIterator[User]:
        await require_user(info)
        pool = info.context["pool"]
        wanted = {parse_id(x) for x in user_ids}
        async for ev in sub_events(info, PRESENCE_TOPIC):
            if ev.get("type") != "presence":
                continue
            uid = uuid.UUID(ev["user_id"])
            if uid not in wanted:
                continue
            row = await db.users_by_ids(pool, [uid])
            if row:
                yield user_from_row(row[0])

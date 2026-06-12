from __future__ import annotations

from datetime import datetime

import forge_py
import strawberry
from strawberry.types import Info

from .. import db
from .helpers import (
    FAIL_QUEUE,
    current_user,
    loaders,
    parse_id,
    require_member,
    require_user,
)
from .types import (
    Chat,
    Message,
    OpsStats,
    User,
    chat_from_row,
    message_from_row,
    user_from_row,
)


@strawberry.type
class Query:
    @strawberry.field(description="The authenticated user, or null when unauthenticated.")
    async def me(self, info: Info) -> User | None:
        u = await current_user(info)
        if u is None:
            return None
        row = await loaders(info)["users"].load(u["id"])
        return user_from_row(row) if row else None

    @strawberry.field
    async def chats(self, info: Info) -> list[Chat]:
        u = await require_user(info)
        rows = await db.chats_for_user(info.context["pool"], u["id"])
        return [chat_from_row(r) for r in rows]

    @strawberry.field
    async def chat(self, info: Info, id: strawberry.ID) -> Chat | None:
        u = await require_user(info)
        cid = parse_id(id)
        await require_member(info, cid, u["id"])
        row = await db.chat(info.context["pool"], cid)
        return chat_from_row(row) if row else None

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

    @strawberry.field
    async def presence(self, info: Info, user_ids: list[strawberry.ID]) -> list[User]:
        await require_user(info)
        ids = [parse_id(x) for x in user_ids]
        rows = await loaders(info)["users"].load_many(ids)
        return [user_from_row(r) for r in rows if r]

    @strawberry.field(
        description="Whether the `reactions_v2` feature flag is enabled for the current user"
        " (forge config)."
    )
    async def reactions_enabled(self, info: Info) -> bool:
        u = await current_user(info)
        if u is None:
            return False
        return await info.context["forge"].flag("reactions_v2", False, str(u["id"]))

    @strawberry.field(
        description="Developer-tools gauges (kv scan + DLQ depth) for the settings page."
    )
    async def ops_stats(self, info: Info) -> OpsStats:
        await require_user(info)
        forge = info.context["forge"]
        try:
            online = len(await forge.kv_scan("online:", 1000))
        except forge_py.ForgeError:
            online = 0
        visible, inflight, delayed = await forge.queue_depth(f"{FAIL_QUEUE}.dlq")
        dlq_count = visible + inflight + delayed
        return OpsStats(online_count=online, dlq_count=dlq_count)

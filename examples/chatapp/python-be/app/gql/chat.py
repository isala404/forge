from __future__ import annotations

import strawberry
from strawberry.types import Info

from .. import db
from .helpers import gqlerr, parse_id, require_member, require_user
from .types import Chat, ChatKind, chat_from_row


@strawberry.type
class ChatQuery:
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


@strawberry.type
class ChatMutation:
    @strawberry.mutation
    async def create_chat(
        self,
        info: Info,
        kind: ChatKind,
        member_usernames: list[str],
        title: str | None = None,
    ) -> Chat:
        u = await require_user(info)
        pool = info.context["pool"]
        ids = [u["id"]]
        for uname in member_usernames:
            row = await db.user_by_username(pool, uname)
            if not row:
                raise gqlerr("NOT_FOUND", f"no such user: {uname}")
            if row["id"] not in ids:
                ids.append(row["id"])
        kind_str = "direct" if kind == ChatKind.DIRECT else "group"
        if kind == ChatKind.DIRECT and len(ids) != 2:
            raise gqlerr("INVALID", "a direct chat needs exactly one other member")
        cid = await db.create_chat(pool, kind_str, title, u["id"], ids)
        row = await db.chat(pool, cid)
        if not row:
            raise gqlerr("BACKEND", "chat vanished after create")
        return chat_from_row(row)

    @strawberry.mutation
    async def add_member(self, info: Info, chat_id: strawberry.ID, username: str) -> Chat:
        u = await require_user(info)
        pool = info.context["pool"]
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        row = await db.user_by_username(pool, username)
        if not row:
            raise gqlerr("NOT_FOUND", f"no such user: {username}")
        await db.add_member(pool, cid, row["id"])
        chat_row = await db.chat(pool, cid)
        if not chat_row:
            raise gqlerr("NOT_FOUND", "chat not found")
        return chat_from_row(chat_row)

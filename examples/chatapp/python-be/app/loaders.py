from __future__ import annotations

import uuid

from strawberry.dataloader import DataLoader

from . import db


def make_loaders(pool, forge) -> dict[str, DataLoader]:
    async def load_users(ids: list[uuid.UUID]):
        rows = await db.users_by_ids(pool, ids)
        by_id = {r["id"]: r for r in rows}
        return [by_id.get(i) for i in ids]

    async def load_members(chat_ids: list[uuid.UUID]):
        rows = await db.members_by_chat_ids(pool, chat_ids)
        grouped: dict[uuid.UUID, list] = {cid: [] for cid in chat_ids}
        for r in rows:
            grouped[r["chat_id"]].append(r)
        return [grouped[cid] for cid in chat_ids]

    async def load_last_message(chat_ids: list[uuid.UUID]):
        rows = await db.last_messages_by_chat_ids(pool, chat_ids)
        by_chat = {r["chat_id"]: r for r in rows}
        return [by_chat.get(cid) for cid in chat_ids]

    async def load_receipts(message_ids: list[uuid.UUID]):
        rows = await db.receipts_by_message_ids(pool, message_ids)
        grouped: dict[uuid.UUID, list] = {mid: [] for mid in message_ids}
        for r in rows:
            grouped[r["message_id"]].append(r)
        return [grouped[mid] for mid in message_ids]

    async def load_online(user_ids: list[uuid.UUID]):
        keys = [f"online:{uid}" for uid in user_ids]
        try:
            vals = await forge.kv_mget(keys)
        except Exception:
            return [False] * len(user_ids)
        return [v is not None for v in vals]

    async def load_unread(keys: list[tuple[uuid.UUID, uuid.UUID]]):
        # Unread is derived from receipts, not a kv counter: one grouped query per
        # viewer counts their still-unread receipts on live messages. Keys are
        # (chat_id, viewer_id); all keys in a batch share the one request's viewer.
        by_viewer: dict[uuid.UUID, list[uuid.UUID]] = {}
        for chat_id, user_id in keys:
            by_viewer.setdefault(user_id, []).append(chat_id)
        counts: dict[tuple[uuid.UUID, uuid.UUID], int] = {}
        for user_id, chat_ids in by_viewer.items():
            for r in await db.unread_counts(pool, user_id, chat_ids):
                counts[(r["chat_id"], user_id)] = r["n"]
        return [counts.get((chat_id, user_id), 0) for (chat_id, user_id) in keys]

    return {
        "users": DataLoader(load_fn=load_users),
        "members": DataLoader(load_fn=load_members),
        "last_message": DataLoader(load_fn=load_last_message),
        "receipts": DataLoader(load_fn=load_receipts),
        "online": DataLoader(load_fn=load_online),
        "unread": DataLoader(load_fn=load_unread),
    }

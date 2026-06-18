from __future__ import annotations

import asyncio
import json
import uuid
from datetime import UTC, datetime, timedelta

import forge_py
import strawberry
from strawberry.types import Info

from .. import db
from .helpers import (
    APIKEY_LIMIT,
    FAIL_QUEUE,
    FANOUT_QUEUE,
    INT32_MAX,
    OTP_LIMIT,
    PRESENCE_TOPIC,
    REAP_QUEUE,
    SEND_LIMIT,
    SESSION_ABSOLUTE,
    SESSION_IDLE,
    UPLOAD_LIMIT,
    chat_topic,
    current_user,
    disappearing_secs,
    gqlerr,
    map_forge,
    max_upload_bytes,
    parse_id,
    publish_event,
    require_member,
    require_user,
    valid_credentials,
)
from .types import (
    ApiKeyPayload,
    Chat,
    ChatKind,
    Message,
    SessionPayload,
    UploadTicket,
    chat_from_row,
    message_from_row,
    user_from_row,
)


async def issue_session(info: Info, user_id: uuid.UUID) -> SessionPayload:
    forge = info.context["forge"]
    token = await forge.create_session(str(user_id), SESSION_IDLE, SESSION_ABSOLUTE)
    row = await db.users_by_ids(info.context["pool"], [user_id])
    if not row:
        raise gqlerr("BACKEND", "user vanished after create")
    return SessionPayload(token=token, user=user_from_row(row[0]))


@strawberry.type
class Mutation:
    @strawberry.mutation
    async def signup(
        self, info: Info, username: str, display_name: str, password: str
    ) -> SessionPayload:
        forge = info.context["forge"]
        pool = info.context["pool"]
        # Normalize first so " alice" and "alice" share one bucket and one stored user;
        # validate before spending a rate-limit token so bad input never burns the bucket.
        username = username.strip()
        if not valid_credentials(username, password):
            raise gqlerr("INVALID", "username must be >= 3 chars and password >= 6 chars")
        # Abuse-sensitive: fail CLOSED so a backend error never grants a free pass.
        try:
            allowed, _, _ = await forge.rate_limit_check(
                "otp", username, OTP_LIMIT[0], OTP_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not allowed:
            raise gqlerr("LIMIT", "too many signup attempts; try again later")
        if await db.username_taken(pool, username):
            raise gqlerr("PRECONDITION", "username already taken")
        h = await forge.hash_password(password)
        uid = await db.create_user(pool, username, display_name, h)
        return await issue_session(info, uid)

    @strawberry.mutation
    async def login(self, info: Info, username: str, password: str) -> SessionPayload:
        forge = info.context["forge"]
        pool = info.context["pool"]
        username = username.strip()
        try:
            allowed, _, _ = await forge.rate_limit_check(
                "otp", username, OTP_LIMIT[0], OTP_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not allowed:
            raise gqlerr("LIMIT", "too many login attempts; try again later")
        creds = await db.credentials(pool, username)
        if creds is None:
            raise gqlerr("UNAUTHENTICATED", "invalid username or password")
        user_id, hash_str = creds
        if not await forge.verify_password(password, hash_str):
            raise gqlerr("UNAUTHENTICATED", "invalid username or password")
        # Transparently upgrade a hash minted under older argon2 params; a rehash
        # failure must never block an otherwise-valid login.
        try:
            if forge.needs_rehash(hash_str):
                fresh = await forge.hash_password(password)
                await db.set_password_hash(pool, user_id, fresh)
        except Exception:
            pass
        return await issue_session(info, user_id)

    @strawberry.mutation
    async def logout(self, info: Info) -> bool:
        u = await current_user(info)
        if u is not None and u["token"]:
            await info.context["forge"].revoke_session(u["token"])
        return True

    @strawberry.mutation
    async def logout_all(self, info: Info) -> bool:
        u = await require_user(info)
        await info.context["forge"].revoke_all_sessions(str(u["id"]))
        return True

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

    @strawberry.mutation(
        description="Hand the client a presigned PUT URL to upload an attachment directly to"
        " blob storage."
    )
    async def request_upload(self, info: Info, chat_id: strawberry.ID) -> UploadTicket:
        u = await require_user(info)
        cid = parse_id(chat_id)
        await require_member(info, cid, u["id"])
        forge = info.context["forge"]
        # Abuse-sensitive presign mint: fail CLOSED so a limiter hiccup can't be used
        # to flood blob storage with presigned PUTs.
        try:
            allowed, _, _ = await forge.rate_limit_check(
                "upload", str(u["id"]), UPLOAD_LIMIT[0], UPLOAD_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not allowed:
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

        # High-volume path: fail OPEN so a limiter hiccup never blocks messaging.
        try:
            allowed, _, _ = await forge.rate_limit_check(
                "send", str(u["id"]), SEND_LIMIT[0], SEND_LIMIT[1], fail_open=True
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not allowed:
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
        # original message instead of inserting a duplicate. SET NX reserves the key
        # for the first send; a loser polls for the winner's just-inserted row.
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
        await forge.queue_enqueue(
            FANOUT_QUEUE, json.dumps({"message_id": str(msg_id)}), dedup_id=str(msg_id)
        )

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
            # Recall: clearing expires_at on not-yet-expired messages turns their
            # already-scheduled reap jobs into no-ops.
            await db.clear_pending_expirations(pool, cid)
        row = await db.chat(pool, cid)
        if not row:
            raise gqlerr("NOT_FOUND", "chat not found")
        return chat_from_row(row)

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

    @strawberry.mutation(
        description="Mint a personal API key (forge auth). The secret is returned exactly once."
    )
    async def create_api_key(self, info: Info, label: str) -> ApiKeyPayload:
        u = await require_user(info)
        forge = info.context["forge"]
        # Abuse-sensitive key mint: fail CLOSED so a limiter hiccup can't be used to
        # spray long-lived credentials.
        try:
            allowed, _, _ = await forge.rate_limit_check(
                "apikey", str(u["id"]), APIKEY_LIMIT[0], APIKEY_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not allowed:
            raise gqlerr("LIMIT", "too many API keys created; try again later")
        try:
            key = await forge.create_api_key(str(u["id"]), label)
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        return ApiKeyPayload(id=key.id, secret=key.secret)

    @strawberry.mutation(
        description="Set the `reactions_v2` feature-flag rollout percentage (forge config)."
    )
    async def set_reactions_rollout(self, info: Info, percent: int) -> bool:
        await require_user(info)
        pct = max(0, min(100, percent))
        try:
            await info.context["forge"].set_flag_percent("reactions_v2", pct)
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        return True

    @strawberry.mutation(
        description="Enqueue a job destined to dead-letter (forge queue DLQ demo)."
    )
    async def trigger_failing_job(self, info: Info) -> bool:
        await require_user(info)
        try:
            await info.context["forge"].queue_enqueue(FAIL_QUEUE, "boom")
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        return True

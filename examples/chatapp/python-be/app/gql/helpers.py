from __future__ import annotations

import os
import time
import uuid
from collections.abc import AsyncIterator

import forgelib
from graphql import GraphQLError
from strawberry.types import Info

from .. import db

DEFAULT_MAX_UPLOAD_BYTES = 10 * 1024 * 1024
INT32_MAX = 2_147_483_647
SESSION_IDLE = 30 * 60
SESSION_ABSOLUTE = 7 * 24 * 60 * 60
FANOUT_QUEUE = "fanout"
FAIL_QUEUE = "fail"
REAP_QUEUE = "reap"
PRESENCE_TOPIC = "presence"
SEND_LIMIT = (5, 10.0)
OTP_LIMIT = (10, 60.0)
UPLOAD_LIMIT = (30, 60.0)
APIKEY_LIMIT = (5, 3600.0)


def chat_topic(chat_id: uuid.UUID) -> str:
    return f"chat:{chat_id}"


def disappearing_secs() -> int:
    try:
        return int(os.environ.get("APP_DISAPPEARING_SECS", "86400"))
    except ValueError:
        return 86400


def gqlerr(code: str, message: str) -> GraphQLError:
    return GraphQLError(message, extensions={"code": code})


_FORGE_CODE_BY_TYPE = {
    "NotFound": "NOT_FOUND",
    "Invalid": "INVALID",
    "Limit": "LIMIT",
    "Precondition": "PRECONDITION",
    "Unavailable": "UNAVAILABLE",
    "Config": "CONFIG",
    "Backend": "BACKEND",
}


def map_forge(err: forgelib.ForgeError) -> GraphQLError:
    return gqlerr(_FORGE_CODE_BY_TYPE.get(forgelib.forge_error_code(err), "BACKEND"), str(err))


def valid_credentials(username: str, password: str) -> bool:
    return len(username.strip()) >= 3 and len(password) >= 6


def parse_id(raw: str) -> uuid.UUID:
    try:
        return uuid.UUID(raw)
    except (ValueError, AttributeError) as e:
        raise gqlerr("INVALID", "malformed id") from e


def loaders(info: Info):
    return info.context["loaders"]


async def current_user(info: Info) -> dict | None:
    return await info.context.auth()


async def require_user(info: Info) -> dict:
    u = await current_user(info)
    if u is None:
        raise gqlerr("UNAUTHENTICATED", "not authenticated")
    return u


async def require_admin(info: Info) -> dict:
    # Gate the ops/admin mutations. The allowlist is a comma-separated list of user
    # ids in ADMIN_USER_IDS. Unset means an empty allowlist, so these mutations are
    # denied for everyone (fail closed), the right default for a demo that ships no
    # roles system. The single entry "*" allows any authenticated user: a dev/demo
    # convenience, never for production.
    u = await require_user(info)
    allowed = {i.strip() for i in os.environ.get("ADMIN_USER_IDS", "").split(",") if i.strip()}
    if "*" not in allowed and str(u["id"]) not in allowed:
        raise gqlerr("FORBIDDEN", "admin only")
    return u


async def require_member(info: Info, chat_id: uuid.UUID, user_id: uuid.UUID) -> None:
    if not await db.is_member(info.context["pool"], chat_id, user_id):
        raise gqlerr("NOT_FOUND", "chat not found or not a member")


async def publish_event(info: Info, topic: str, event: dict) -> None:
    try:
        await info.context["forge"].topic(topic).publish(event)
    except forgelib.ForgeError:
        pass


async def max_upload_bytes(info: Info) -> int:
    try:
        v = await info.context["forge"].config_get("max_upload_bytes")
        if v is not None:
            return int(v)
    except (forgelib.ForgeError, ValueError):
        pass
    return DEFAULT_MAX_UPLOAD_BYTES


REAUTH_INTERVAL_SECS = 60.0


async def sub_events(info: Info, topic: str) -> AsyncIterator[dict]:
    events = info.context["forge"].topic(topic)
    # Re-validate the principal at most once per interval so a revoked session ends
    # the stream instead of streaming forever.
    next_check = time.monotonic() + REAUTH_INTERVAL_SECS
    revalidate = info.context.has_token()
    async for event in events.subscribe():
        if revalidate and time.monotonic() >= next_check:
            if await info.context.revalidate() is None:
                return
            next_check = time.monotonic() + REAUTH_INTERVAL_SECS
        yield event

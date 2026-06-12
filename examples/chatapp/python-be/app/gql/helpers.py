from __future__ import annotations

import json
import os
import uuid

import forge_py
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


def map_forge(err: forge_py.ForgeError) -> GraphQLError:
    return gqlerr(_FORGE_CODE_BY_TYPE.get(type(err).__name__, "BACKEND"), str(err))


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


async def require_member(info: Info, chat_id: uuid.UUID, user_id: uuid.UUID) -> None:
    if not await db.is_member(info.context["pool"], chat_id, user_id):
        raise gqlerr("NOT_FOUND", "chat not found or not a member")


async def publish_event(info: Info, topic: str, event: dict) -> None:
    try:
        await info.context["forge"].pubsub_publish(topic, json.dumps(event))
    except forge_py.ForgeError:
        pass


async def max_upload_bytes(info: Info) -> int:
    try:
        v = await info.context["forge"].config_get("max_upload_bytes")
        if v is not None:
            return int(v)
    except (forge_py.ForgeError, ValueError):
        pass
    return DEFAULT_MAX_UPLOAD_BYTES

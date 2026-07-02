from __future__ import annotations

import enum
import uuid
from datetime import datetime

import strawberry
from strawberry.types import Info

from .helpers import current_user, gqlerr, loaders


@strawberry.enum
class ChatKind(enum.Enum):
    DIRECT = "DIRECT"
    GROUP = "GROUP"


@strawberry.type
class User:
    id: strawberry.ID
    username: str
    display_name: str
    _uid: strawberry.Private[uuid.UUID]

    @strawberry.field(
        description="Live presence, backed by a kv key with a short TTL refreshed by heartbeat."
    )
    async def online(self, info: Info) -> bool:
        return await loaders(info)["online"].load(self._uid)


def user_from_row(row) -> User:
    return User(
        id=strawberry.ID(str(row["id"])),
        username=row["username"],
        display_name=row["display_name"],
        _uid=row["id"],
    )


@strawberry.type(
    description="An attachment stored in blob storage, exposed via a short-lived presigned"
    " download URL."
)
class Media:
    key: str
    download_url: str
    content_type: str | None


@strawberry.type
class Receipt:
    message_id: strawberry.ID
    _user_id: strawberry.Private[uuid.UUID]
    delivered_at: datetime | None
    read_at: datetime | None

    @strawberry.field
    async def user(self, info: Info) -> User:
        row = await loaders(info)["users"].load(self._user_id)
        if not row:
            raise gqlerr("NOT_FOUND", "user not found")
        return user_from_row(row)


def receipt_from_row(row) -> Receipt:
    return Receipt(
        message_id=strawberry.ID(str(row["message_id"])),
        _user_id=row["user_id"],
        delivered_at=row["delivered_at"],
        read_at=row["read_at"],
    )


@strawberry.type
class Message:
    id: strawberry.ID
    body: str
    created_at: datetime
    chat_id: strawberry.ID
    _sender_id: strawberry.Private[uuid.UUID]
    _media_key: strawberry.Private[str | None]
    _content_type: strawberry.Private[str | None]
    _id: strawberry.Private[uuid.UUID]

    @strawberry.field
    async def sender(self, info: Info) -> User:
        row = await loaders(info)["users"].load(self._sender_id)
        if not row:
            raise gqlerr("NOT_FOUND", "sender not found")
        return user_from_row(row)

    @strawberry.field
    async def media(self, info: Info) -> Media | None:
        if not self._media_key:
            return None
        url = await info.context["forge"].blob_presign_download(self._media_key, 3600)
        return Media(key=self._media_key, download_url=url, content_type=self._content_type)

    @strawberry.field
    async def receipts(self, info: Info) -> list[Receipt]:
        rows = await loaders(info)["receipts"].load(self._id)
        return [receipt_from_row(r) for r in rows]


def message_from_row(row) -> Message:
    return Message(
        id=strawberry.ID(str(row["id"])),
        body=row["body"],
        created_at=row["created_at"],
        chat_id=strawberry.ID(str(row["chat_id"])),
        _sender_id=row["sender_id"],
        _media_key=row["media_key"],
        _content_type=row["content_type"],
        _id=row["id"],
    )


@strawberry.type
class Chat:
    id: strawberry.ID
    title: str | None
    _disappearing_seconds: strawberry.Private[int | None]
    _kind: strawberry.Private[str]
    _id: strawberry.Private[uuid.UUID]

    @strawberry.field
    def kind(self) -> ChatKind:
        return ChatKind.GROUP if self._kind == "group" else ChatKind.DIRECT

    @strawberry.field(
        description="Disappearing-message lifetime in seconds, or null when off (forge schedule)."
    )
    def disappearing_seconds(self) -> int | None:
        return self._disappearing_seconds

    @strawberry.field
    async def members(self, info: Info) -> list[User]:
        rows = await loaders(info)["members"].load(self._id)
        return [user_from_row(r) for r in rows]

    @strawberry.field
    async def last_message(self, info: Info) -> Message | None:
        row = await loaders(info)["last_message"].load(self._id)
        return message_from_row(row) if row else None

    @strawberry.field(description="Unread count for the requesting user, tracked in kv.")
    async def unread(self, info: Info) -> int:
        u = await current_user(info)
        if u is None:
            return 0
        return await loaders(info)["unread"].load((self._id, u["id"]))


def chat_from_row(row) -> Chat:
    return Chat(
        id=strawberry.ID(str(row["id"])),
        title=row["title"],
        _disappearing_seconds=row["disappearing_seconds"],
        _kind=row["kind"],
        _id=row["id"],
    )


@strawberry.type
class TypingEvent:
    user: User
    typing: bool


@strawberry.type(
    description="A presigned PUT ticket. The client uploads attachment bytes directly to"
    " blob storage."
)
class UploadTicket:
    key: str
    upload_url: str
    max_bytes: int


@strawberry.type(
    description="Returned by signup/login. The token authenticates HTTP (Authorization:"
    " Bearer) and WS."
)
class SessionPayload:
    token: str
    user: User


@strawberry.type(
    description="A freshly minted API key (forge auth). The secret is shown exactly once."
)
class ApiKeyPayload:
    id: str
    secret: str


@strawberry.type(description="Developer-tools gauges for the settings page.")
class OpsStats:
    online_count: int = strawberry.field(
        description="Users currently online, counted via a kv scan of the `online:` prefix."
    )
    dlq_count: int = strawberry.field(
        description="Jobs sitting in the `fail.dlq` dead-letter queue."
    )

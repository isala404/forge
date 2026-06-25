from __future__ import annotations

import uuid

import forge_py
import strawberry
from strawberry.types import Info

from .. import db
from .helpers import (
    APIKEY_LIMIT,
    OTP_LIMIT,
    SESSION_ABSOLUTE,
    SESSION_IDLE,
    current_user,
    gqlerr,
    loaders,
    map_forge,
    require_user,
    valid_credentials,
)
from .types import ApiKeyPayload, SessionPayload, User, user_from_row


async def issue_session(info: Info, user_id: uuid.UUID) -> SessionPayload:
    forge = info.context["forge"]
    token = await forge.create_session(str(user_id), SESSION_IDLE, SESSION_ABSOLUTE)
    row = await db.users_by_ids(info.context["pool"], [user_id])
    if not row:
        raise gqlerr("BACKEND", "user vanished after create")
    return SessionPayload(token=token, user=user_from_row(row[0]))


@strawberry.type
class AuthQuery:
    @strawberry.field(description="The authenticated user, or null when unauthenticated.")
    async def me(self, info: Info) -> User | None:
        u = await current_user(info)
        if u is None:
            return None
        row = await loaders(info)["users"].load(u["id"])
        return user_from_row(row) if row else None


@strawberry.type
class AuthMutation:
    @strawberry.mutation
    async def signup(
        self, info: Info, username: str, display_name: str, password: str
    ) -> SessionPayload:
        forge = info.context["forge"]
        pool = info.context["pool"]
        username = username.strip()
        if not valid_credentials(username, password):
            raise gqlerr("INVALID", "username must be >= 3 chars and password >= 6 chars")
        try:
            decision = await forge.rate_limit_check(
                "otp", username, OTP_LIMIT[0], OTP_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not decision.allowed:
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
            decision = await forge.rate_limit_check(
                "otp", username, OTP_LIMIT[0], OTP_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not decision.allowed:
            raise gqlerr("LIMIT", "too many login attempts; try again later")
        creds = await db.credentials(pool, username)
        if creds is None:
            # Verify against the decoy so an unknown username costs the same argon2
            # time as a real one; otherwise the timing gap enumerates valid usernames.
            try:
                await forge.verify_password(password, info.context["decoy_hash"])
            except forge_py.ForgeError:
                pass
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

    @strawberry.mutation(
        description="Mint a personal API key (forge auth). The secret is returned exactly once."
    )
    async def create_api_key(self, info: Info, label: str) -> ApiKeyPayload:
        u = await require_user(info)
        forge = info.context["forge"]
        try:
            decision = await forge.rate_limit_check(
                "apikey", str(u["id"]), APIKEY_LIMIT[0], APIKEY_LIMIT[1], fail_open=False
            )
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        if not decision.allowed:
            raise gqlerr("LIMIT", "too many API keys created; try again later")
        try:
            key = await forge.create_api_key(str(u["id"]), label)
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        return ApiKeyPayload(id=key.id, secret=key.secret)

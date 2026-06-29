from __future__ import annotations

import uuid

import forgelib
from starlette.requests import HTTPConnection
from strawberry.fastapi import BaseContext

from . import db
from .loaders import make_loaders


def _bearer(value: str | None) -> str | None:
    if not value:
        return None
    parts = value.split(" ", 1)
    if len(parts) != 2 or parts[0].lower() != "bearer":
        return None
    return parts[1].strip() or None


class Context(BaseContext):
    """Subclasses BaseContext so Strawberry keeps the object intact and injects
    `connection_params` (WS init payload) onto it. Resolvers read the shared services by
    key; the principal resolves lazily on first `auth()` and is cached for the
    request/socket lifetime (WS connection_params arrive only after this is built)."""

    def __init__(self, forge, pool, presence_ttl: float, http_token: str | None, decoy_hash: str):
        super().__init__()
        self._services = {
            "forge": forge,
            "pool": pool,
            "presence_ttl": presence_ttl,
            "loaders": make_loaders(pool, forge),
            # Throwaway argon2id hash (minted once at startup) for the login timing
            # decoy; see app.gql.auth.login.
            "decoy_hash": decoy_hash,
        }
        self._http_token = http_token
        self._resolved = False
        self._user: dict | None = None

    def __getitem__(self, key):
        return self._services[key]

    def get(self, key, default=None):
        return self._services.get(key, default)

    def _token(self) -> str | None:
        if self._http_token:
            return self._http_token
        params = self.connection_params or {}
        if isinstance(params, dict):
            return _bearer(params.get("authorization") or params.get("Authorization"))
        return None

    async def auth(self) -> dict | None:
        if self._resolved:
            return self._user
        self._resolved = True
        token = self._token()
        if token:
            self._user = await _principal(self["forge"], self["pool"], token)
        return self._user

    def has_token(self) -> bool:
        """Whether a bearer token was supplied (header or WS init payload)."""
        return self._token() is not None

    async def revalidate(self) -> dict | None:
        """Re-resolve the principal from scratch, bypassing the cache, and refresh it.
        Used to reject a WS connection whose token is bad, and to re-check long-lived
        subscriptions whose session may have been revoked mid-stream."""
        token = self._token()
        self._user = await _principal(self["forge"], self["pool"], token) if token else None
        self._resolved = True
        return self._user


async def _principal(forge, pool, token: str) -> dict | None:
    user_id = None
    try:
        user_id = await forge.validate_session(token)
    except forgelib.ForgeError:
        user_id = None
    revocable = user_id is not None
    if user_id is None:
        try:
            user_id = await forge.verify_api_key(token)
        except forgelib.ForgeError:
            user_id = None
    if not user_id:
        return None
    try:
        uid = uuid.UUID(user_id)
    except ValueError:
        return None
    row = await db.users_by_ids(pool, [uid])
    if not row:
        return None
    r = row[0]
    # Only a session token is revocable; an API-key principal has no session to drop.
    return {
        "id": r["id"],
        "username": r["username"],
        "display_name": r["display_name"],
        "token": token if revocable else "",
    }


def make_context_getter(presence_ttl: float):
    async def getter(connection: HTTPConnection) -> Context:
        state = connection.app.state
        http_token = _bearer(connection.headers.get("authorization"))
        return Context(state.forge, state.pool, presence_ttl, http_token, state.decoy_hash)

    return getter

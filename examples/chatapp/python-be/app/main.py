"""chatapp Python backend: a pure GraphQL API over Forge primitives.

Forge primitives go through the natively-async `forge_py` binding; the chat tables go
through an asyncpg pool. Realtime rides Forge pubsub via the binding's Subscription async
iterator, so there is no separate broker."""

from __future__ import annotations

import asyncio
import os
import uuid
from contextlib import asynccontextmanager

import asyncpg
import forge_py
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import PlainTextResponse
from strawberry.exceptions import ConnectionRejectionError
from strawberry.fastapi import GraphQLRouter
from strawberry.subscriptions import GRAPHQL_TRANSPORT_WS_PROTOCOL, GRAPHQL_WS_PROTOCOL

from . import db
from .blob_router import router as blob_router
from .context import make_context_getter
from .gql import schema
from .workers import start_workers


class ChatGraphQLRouter(GraphQLRouter):
    async def on_ws_connect(self, context):
        # A socket with no token stays anonymous (subscriptions then fail at
        # require_user, as before). But a token that's present and does NOT validate
        # must not open even an anonymous socket — reject the handshake.
        if context.has_token() and await context.revalidate() is None:
            raise ConnectionRejectionError()
        return await super().on_ws_connect(context)


def env(key: str, default: str) -> str:
    return os.environ.get(key, default)


def env_float(key: str, default: float) -> float:
    try:
        return float(os.environ.get(key, str(default)))
    except ValueError:
        return default


def build_app() -> FastAPI:
    presence_ttl = env_float("APP_PRESENCE_TTL_SECS", 30.0)

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        pg = env(
            "FORGE_POSTGRES_URL",
            "postgres://postgres:forge@127.0.0.1:5432/chatapp_python_be",
        )
        secret = env("FORGE_BLOB_SIGNING_SECRET", "dev-secret-change-me")
        # connect migrates Forge's system tables at startup; it owns its database.
        forge = await forge_py.ForgeClient.connect(pg, secret)
        pool = await asyncpg.create_pool(pg, min_size=1, max_size=10)
        await db.migrate(pool)

        app.state.forge = forge
        app.state.pool = pool
        # Mint the login decoy hash once, via forge's own hasher so its argon2 params
        # always match real password hashes. `login` verifies against it on a username
        # miss to keep that path's timing indistinguishable from a real verify.
        app.state.decoy_hash = await forge.hash_password(str(uuid.uuid4()))

        scheduler_interval = env_float("APP_SCHEDULER_MS", 30000.0) / 1000.0
        stop = asyncio.Event()
        tasks = start_workers(forge, pool, stop, scheduler_interval)
        try:
            yield
        finally:
            stop.set()
            for t in tasks:
                t.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            await pool.close()

    app = FastAPI(lifespan=lifespan)
    app.add_middleware(
        CORSMiddleware,
        allow_origins=[env("CORS_ORIGIN", "*")],
        allow_methods=["*"],
        allow_headers=["*"],
    )

    graphql = ChatGraphQLRouter(
        schema,
        context_getter=make_context_getter(presence_ttl),
        subscription_protocols=[GRAPHQL_TRANSPORT_WS_PROTOCOL, GRAPHQL_WS_PROTOCOL],
        graphql_ide=None,
    )
    app.include_router(graphql, prefix="/graphql")
    app.include_router(blob_router)

    @app.get("/healthz", response_class=PlainTextResponse)
    async def healthz() -> str:
        return "ok"

    return app


app = build_app()

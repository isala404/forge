"""Boots the real ASGI app under uvicorn on a free port so tests drive the GraphQL API
over real HTTP (httpx) and real WS (websockets) against a live Postgres.

Test env shortens presence/disappearing/scheduler timers so TTL-driven scenarios finish
in seconds. Each test database is the dedicated `chatapp_python_be_test`."""

from __future__ import annotations

import asyncio
import os
import re
import socket
import threading
import time
from urllib.parse import urlsplit, urlunsplit

import asyncpg
import pytest
import pytest_asyncio


def _database_url(admin_url: str, database: str) -> str:
    parts = urlsplit(admin_url)
    return urlunsplit(parts._replace(path=f"/{database}"))


TEST_DB = os.environ.get("CHATAPP_TEST_DB", "chatapp_python_be_test")
if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", TEST_DB):
    raise ValueError("CHATAPP_TEST_DB must be a simple PostgreSQL identifier")
ADMIN_URL = os.environ.get(
    "CHATAPP_TEST_ADMIN_URL",
    os.environ.get("TEST_DATABASE_URL", "postgres://postgres:forge@127.0.0.1:5432/postgres"),
)
TEST_URL = os.environ.get("CHATAPP_TEST_DATABASE_URL", _database_url(ADMIN_URL, TEST_DB))

os.environ["FORGE_POSTGRES_URL"] = TEST_URL
os.environ["FORGE_BLOB_SIGNING_SECRET"] = "test-secret"
os.environ["APP_PRESENCE_TTL_SECS"] = "2"
os.environ["APP_DISAPPEARING_SECS"] = "2"
os.environ["APP_SCHEDULER_MS"] = "500"
# "*" = any authenticated user is an admin, so the ops tests can exercise the gated
# mutations without knowing a user id at boot. A real deploy lists actual ids.
os.environ["ADMIN_USER_IDS"] = "*"


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


async def _ensure_db() -> None:
    conn = await asyncpg.connect(ADMIN_URL)
    try:
        await conn.execute(
            """
            SELECT pg_terminate_backend(pid)
            FROM pg_stat_activity
            WHERE datname = $1 AND pid <> pg_backend_pid()
            """,
            TEST_DB,
        )
        await conn.execute(f'DROP DATABASE IF EXISTS "{TEST_DB}"')
        await conn.execute(f'CREATE DATABASE "{TEST_DB}"')
    finally:
        await conn.close()


@pytest.fixture(scope="session")
def server() -> dict:
    asyncio.run(_ensure_db())

    import uvicorn

    from app.main import app

    port = _free_port()
    config = uvicorn.Config(app, host="127.0.0.1", port=port, log_level="warning")
    userver = uvicorn.Server(config)
    thread = threading.Thread(target=userver.run, daemon=True)
    thread.start()

    deadline = time.time() + 30
    while not userver.started and time.time() < deadline:
        time.sleep(0.05)
    if not userver.started:
        raise RuntimeError("uvicorn failed to start")

    yield {
        "http": f"http://127.0.0.1:{port}",
        "ws": f"ws://127.0.0.1:{port}/graphql",
    }

    userver.should_exit = True
    thread.join(timeout=10)


@pytest_asyncio.fixture
async def client(server):
    import httpx

    async with httpx.AsyncClient(base_url=server["http"], timeout=30) as c:
        yield c

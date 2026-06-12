"""Boots the real ASGI app under uvicorn on a free port so tests drive the GraphQL API
over real HTTP (httpx) and real WS (websockets) against a live Postgres.

Test env shortens presence/disappearing/scheduler timers so TTL-driven scenarios finish
in seconds. Each test database is the dedicated `chatapp_python_be_test`."""

from __future__ import annotations

import asyncio
import os
import socket
import threading
import time

import asyncpg
import pytest
import pytest_asyncio

TEST_DB = "chatapp_python_be_test"
ADMIN_URL = "postgres://postgres:forge@127.0.0.1:5432/postgres"
TEST_URL = f"postgres://postgres:forge@127.0.0.1:5432/{TEST_DB}"

os.environ["FORGE_POSTGRES_URL"] = TEST_URL
os.environ["FORGE_BLOB_SIGNING_SECRET"] = "test-secret"
os.environ["APP_PRESENCE_TTL_SECS"] = "2"
os.environ["APP_DISAPPEARING_SECS"] = "2"
os.environ["APP_SCHEDULER_MS"] = "500"


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


async def _ensure_db() -> None:
    conn = await asyncpg.connect(ADMIN_URL)
    try:
        exists = await conn.fetchval("SELECT 1 FROM pg_database WHERE datname=$1", TEST_DB)
        if not exists:
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

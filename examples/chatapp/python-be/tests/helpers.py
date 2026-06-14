"""GraphQL request + graphql-transport-ws helpers for the integration tests."""

from __future__ import annotations

import json
import uuid
from contextlib import asynccontextmanager

from websockets.asyncio.client import connect as ws_connect


async def gql(client, query: str, variables: dict | None = None, token: str | None = None):
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    r = await client.post(
        "/graphql", json={"query": query, "variables": variables or {}}, headers=headers
    )
    r.raise_for_status()
    return r.json()


async def gql_data(client, query, variables=None, token=None):
    body = await gql(client, query, variables, token)
    assert "errors" not in body, body.get("errors")
    return body["data"]


async def signup(client, display_name="User") -> dict:
    username = "u_" + uuid.uuid4().hex[:12]
    data = await gql_data(
        client,
        "mutation($u:String!,$d:String!,$p:String!){"
        "signup(username:$u,displayName:$d,password:$p){token user{id username}}}",
        {"u": username, "d": display_name, "p": "password123"},
    )
    payload = data["signup"]
    return {
        "username": username,
        "token": payload["token"],
        "id": payload["user"]["id"],
    }


@asynccontextmanager
async def ws_subscribe(ws_url: str, token: str, query: str, variables: dict):
    """Open a graphql-transport-ws socket, init with the bearer token, start one
    subscription, and yield an async generator of `next` payloads."""
    async with ws_connect(ws_url, subprotocols=["graphql-transport-ws"]) as sock:
        await sock.send(
            json.dumps({"type": "connection_init", "payload": {"authorization": f"Bearer {token}"}})
        )
        ack = json.loads(await sock.recv())
        assert ack["type"] == "connection_ack", ack
        await sock.send(
            json.dumps(
                {
                    "id": "1",
                    "type": "subscribe",
                    "payload": {"query": query, "variables": variables},
                }
            )
        )

        async def events():
            async for raw in sock:
                msg = json.loads(raw)
                if msg["type"] == "next":
                    yield msg["payload"]
                elif msg["type"] == "error":
                    raise AssertionError(f"subscription error: {msg['payload']}")
                elif msg["type"] == "complete":
                    break

        yield events()

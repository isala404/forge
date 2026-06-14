"""End-to-end integration suite driving the GraphQL API over HTTP + WS against a live
Postgres. Covers every SPEC scenario; no skips."""

from __future__ import annotations

import asyncio

import pytest

from .helpers import gql, gql_data, signup, ws_subscribe

pytestmark = pytest.mark.asyncio


async def make_group(client, owner, *members) -> str:
    data = await gql_data(
        client,
        "mutation($t:String,$m:[String!]!){"
        "createChat(kind:GROUP,title:$t,memberUsernames:$m){id members{id}}}",
        {"t": "Room", "m": [m["username"] for m in members]},
        token=owner["token"],
    )
    return data["createChat"]["id"]


async def send(client, token, chat_id, body, media_key=None):
    data = await gql_data(
        client,
        "mutation($c:ID!,$b:String!,$k:String){"
        "sendMessage(chatId:$c,body:$b,mediaKey:$k){id body media{key downloadUrl contentType}}}",
        {"c": chat_id, "b": body, "k": media_key},
        token=token,
    )
    return data["sendMessage"]


async def test_signup_returns_session_and_me(client):
    alice = await signup(client, "Alice")
    assert alice["token"]
    me = await gql_data(client, "{me{id username}}", token=alice["token"])
    assert me["me"]["id"] == alice["id"]
    # Unauthenticated me() is null, not an error.
    anon = await gql_data(client, "{me{id}}")
    assert anon["me"] is None


async def test_signup_validation_and_duplicate(client):
    short = await gql(
        client,
        "mutation($u:String!,$d:String!,$p:String!){signup(username:$u,displayName:$d,password:$p){token}}",
        {"u": "ab", "d": "X", "p": "password123"},
    )
    assert short["errors"][0]["extensions"]["code"] == "INVALID"

    alice = await signup(client)
    dup = await gql(
        client,
        "mutation($u:String!,$d:String!,$p:String!){signup(username:$u,displayName:$d,password:$p){token}}",
        {"u": alice["username"], "d": "X", "p": "password123"},
    )
    assert dup["errors"][0]["extensions"]["code"] == "PRECONDITION"


async def test_unauthenticated_query_errors(client):
    res = await gql(client, "{chats{id}}")
    assert res["errors"][0]["extensions"]["code"] == "UNAUTHENTICATED"


async def test_group_chat_visible_to_both_members(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)

    a_chats = await gql_data(client, "{chats{id members{username}}}", token=alice["token"])
    b_chats = await gql_data(client, "{chats{id}}", token=bob["token"])
    assert chat_id in [c["id"] for c in a_chats["chats"]]
    assert chat_id in [c["id"] for c in b_chats["chats"]]
    assert len(a_chats["chats"][0]["members"]) == 2


async def test_direct_chat_requires_exactly_two(client):
    alice = await signup(client)
    bob = await signup(client)
    carol = await signup(client)
    bad = await gql(
        client,
        "mutation($m:[String!]!){createChat(kind:DIRECT,memberUsernames:$m){id}}",
        {"m": [bob["username"], carol["username"]]},
        token=alice["token"],
    )
    assert bad["errors"][0]["extensions"]["code"] == "INVALID"

    ok = await gql_data(
        client,
        "mutation($m:[String!]!){createChat(kind:DIRECT,memberUsernames:$m){id kind}}",
        {"m": [bob["username"]]},
        token=alice["token"],
    )
    assert ok["createChat"]["kind"] == "DIRECT"


async def test_chat_membership_checked(client):
    alice = await signup(client)
    bob = await signup(client)
    outsider = await signup(client)
    chat_id = await make_group(client, alice, bob)
    res = await gql(
        client, "query($c:ID!){chat(id:$c){id}}", {"c": chat_id}, token=outsider["token"]
    )
    assert res["errors"][0]["extensions"]["code"] == "NOT_FOUND"


async def test_create_chat_unknown_member(client):
    alice = await signup(client)
    res = await gql(
        client,
        "mutation($m:[String!]!){createChat(kind:GROUP,memberUsernames:$m){id}}",
        {"m": ["nobody_here"]},
        token=alice["token"],
    )
    assert res["errors"][0]["extensions"]["code"] == "NOT_FOUND"


async def test_send_receive_message_live_subscription(client, server):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)

    async with ws_subscribe(
        server["ws"],
        bob["token"],
        "subscription($c:ID!){messageAdded(chatId:$c){id body sender{username}}}",
        {"c": chat_id},
    ) as events:
        await asyncio.sleep(0.3)
        await send(client, alice["token"], chat_id, "hello bob")
        payload = await asyncio.wait_for(events.__anext__(), timeout=10)
        msg = payload["data"]["messageAdded"]
        assert msg["body"] == "hello bob"
        assert msg["sender"]["username"] == alice["username"]


async def test_typing_event_suppresses_own(client, server):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)

    async with ws_subscribe(
        server["ws"],
        bob["token"],
        "subscription($c:ID!){typing(chatId:$c){user{username} typing}}",
        {"c": chat_id},
    ) as events:
        await asyncio.sleep(0.3)
        # Bob's own typing is suppressed; only Alice's reaches Bob.
        await gql_data(
            client,
            "mutation($c:ID!){setTyping(chatId:$c,typing:true)}",
            {"c": chat_id},
            token=bob["token"],
        )
        await gql_data(
            client,
            "mutation($c:ID!){setTyping(chatId:$c,typing:true)}",
            {"c": chat_id},
            token=alice["token"],
        )
        payload = await asyncio.wait_for(events.__anext__(), timeout=10)
        ev = payload["data"]["typing"]
        assert ev["user"]["username"] == alice["username"]
        assert ev["typing"] is True


async def test_presence_online_then_offline_via_kv_ttl(client):
    alice = await signup(client, "Alice")
    await gql_data(client, "mutation{heartbeat}", token=alice["token"])
    on = await gql_data(
        client,
        "query($u:[ID!]!){presence(userIds:$u){id online}}",
        {"u": [alice["id"]]},
        token=alice["token"],
    )
    assert on["presence"][0]["online"] is True
    # APP_PRESENCE_TTL_SECS=2 in tests; wait for the key to expire.
    await asyncio.sleep(3.0)
    off = await gql_data(
        client,
        "query($u:[ID!]!){presence(userIds:$u){id online}}",
        {"u": [alice["id"]]},
        token=alice["token"],
    )
    assert off["presence"][0]["online"] is False


async def test_attachment_upload_presign_put_send_download(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)

    ticket = await gql_data(
        client,
        "mutation($c:ID!){requestUpload(chatId:$c){key uploadUrl maxBytes}}",
        {"c": chat_id},
        token=alice["token"],
    )
    up = ticket["requestUpload"]
    body = b"hello-bytes-payload"
    put = await client.put(up["uploadUrl"], content=body, headers={"content-type": "text/plain"})
    assert put.status_code == 200

    msg = await send(client, alice["token"], chat_id, "see file", media_key=up["key"])
    assert msg["media"]["key"] == up["key"]
    assert msg["media"]["contentType"] == "text/plain"

    dl = await client.get(msg["media"]["downloadUrl"])
    assert dl.status_code == 200
    assert dl.content == body


async def test_send_empty_without_media_rejected(client):
    alice = await signup(client)
    bob = await signup(client)
    chat_id = await make_group(client, alice, bob)
    res = await gql(
        client,
        'mutation($c:ID!){sendMessage(chatId:$c,body:"   "){id}}',
        {"c": chat_id},
        token=alice["token"],
    )
    assert res["errors"][0]["extensions"]["code"] == "INVALID"


async def test_unread_increments_then_clears_on_mark_read(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)

    msg = await send(client, alice["token"], chat_id, "unread me")

    # Fanout worker bumps Bob's unread counter; poll until it lands.
    async def bob_unread():
        d = await gql_data(client, "{chats{id unread}}", token=bob["token"])
        for c in d["chats"]:
            if c["id"] == chat_id:
                return c["unread"]
        return 0

    for _ in range(40):
        if await bob_unread() >= 1:
            break
        await asyncio.sleep(0.25)
    assert await bob_unread() >= 1

    await gql_data(
        client,
        "mutation($c:ID!,$m:ID!){markRead(chatId:$c,messageId:$m)}",
        {"c": chat_id, "m": msg["id"]},
        token=bob["token"],
    )
    assert await bob_unread() == 0


async def test_read_receipt_turns_read(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)
    msg = await send(client, alice["token"], chat_id, "read me")
    await gql_data(
        client,
        "mutation($c:ID!,$m:ID!){markRead(chatId:$c,messageId:$m)}",
        {"c": chat_id, "m": msg["id"]},
        token=bob["token"],
    )
    data = await gql_data(
        client,
        "query($c:ID!){messages(chatId:$c){id receipts{user{username} readAt}}}",
        {"c": chat_id},
        token=alice["token"],
    )
    receipts = data["messages"][0]["receipts"]
    bob_receipt = next(r for r in receipts if r["user"]["username"] == bob["username"])
    assert bob_receipt["readAt"] is not None


async def test_receipt_changed_subscription(client, server):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)
    msg = await send(client, alice["token"], chat_id, "receipt sub")

    async with ws_subscribe(
        server["ws"],
        alice["token"],
        "subscription($c:ID!){receiptChanged(chatId:$c){messageId user{username} readAt}}",
        {"c": chat_id},
    ) as events:
        await asyncio.sleep(0.3)
        await gql_data(
            client,
            "mutation($c:ID!,$m:ID!){markRead(chatId:$c,messageId:$m)}",
            {"c": chat_id, "m": msg["id"]},
            token=bob["token"],
        )
        payload = await asyncio.wait_for(events.__anext__(), timeout=10)
        rc = payload["data"]["receiptChanged"]
        assert rc["user"]["username"] == bob["username"]
        assert rc["readAt"] is not None


async def test_logout_all_revokes_other_sessions(client):
    alice = await signup(client, "Alice")
    # Second session for the same user via login.
    second = await gql_data(
        client,
        "mutation($u:String!,$p:String!){login(username:$u,password:$p){token}}",
        {"u": alice["username"], "p": "password123"},
    )
    token2 = second["login"]["token"]
    assert (await gql_data(client, "{me{id}}", token=token2))["me"]["id"] == alice["id"]

    await gql_data(client, "mutation{logoutAll}", token=alice["token"])
    after = await gql_data(client, "{me{id}}", token=token2)
    assert after["me"] is None


async def test_rate_limit_throttles_send_burst(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)
    # SEND_LIMIT is 5/10s keyed by user; a tight burst must eventually 429-equivalent.
    limited = False
    for i in range(12):
        res = await gql(
            client,
            "mutation($c:ID!,$b:String!){sendMessage(chatId:$c,body:$b){id}}",
            {"c": chat_id, "b": f"burst {i}"},
            token=alice["token"],
        )
        if "errors" in res and res["errors"][0]["extensions"]["code"] == "LIMIT":
            limited = True
            break
    assert limited


async def test_disappearing_message_vanishes(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    chat_id = await make_group(client, alice, bob)
    await gql_data(
        client,
        "mutation($c:ID!){setDisappearing(chatId:$c,enabled:true){disappearingSeconds}}",
        {"c": chat_id},
        token=alice["token"],
    )
    msg = await send(client, alice["token"], chat_id, "self destruct")

    async def visible():
        d = await gql_data(
            client, "query($c:ID!){messages(chatId:$c){id}}", {"c": chat_id}, token=alice["token"]
        )
        return msg["id"] in [m["id"] for m in d["messages"]]

    assert await visible()
    # APP_DISAPPEARING_SECS=2 + scheduler tick 0.5s; reap removes the row.
    for _ in range(40):
        if not await visible():
            break
        await asyncio.sleep(0.25)
    assert not await visible()


async def test_reactions_feature_flag_toggles(client):
    alice = await signup(client)
    ok = await gql_data(
        client,
        "mutation($p:Int!){setReactionsRollout(percent:$p)}",
        {"p": 100},
        token=alice["token"],
    )
    assert ok["setReactionsRollout"] is True
    again = await gql_data(
        client,
        "mutation($p:Int!){setReactionsRollout(percent:$p)}",
        {"p": 0},
        token=alice["token"],
    )
    assert again["setReactionsRollout"] is True


async def test_api_key_authenticates_request(client):
    alice = await signup(client, "Alice")
    key = await gql_data(
        client,
        "mutation($l:String!){createApiKey(label:$l){id secret}}",
        {"l": "cli"},
        token=alice["token"],
    )
    secret = key["createApiKey"]["secret"]
    me = await gql_data(client, "{me{id username}}", token=secret)
    assert me["me"]["id"] == alice["id"]


async def test_ops_stats_reflects_online_and_dlq(client):
    alice = await signup(client, "Alice")
    await gql_data(client, "mutation{heartbeat}", token=alice["token"])
    await gql_data(client, "mutation{triggerFailingJob}", token=alice["token"])

    # Fail worker nacks until the job dead-letters into fail.dlq; poll opsStats.
    async def stats():
        d = await gql_data(client, "{opsStats{onlineCount dlqCount}}", token=alice["token"])
        return d["opsStats"]

    s = await stats()
    assert s["onlineCount"] >= 1

    dlq_seen = False
    for _ in range(40):
        if (await stats())["dlqCount"] >= 1:
            dlq_seen = True
            break
        await asyncio.sleep(0.25)
    assert dlq_seen


async def test_add_member(client):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    carol = await signup(client, "Carol")
    chat_id = await make_group(client, alice, bob)
    data = await gql_data(
        client,
        "mutation($c:ID!,$u:String!){addMember(chatId:$c,username:$u){members{username}}}",
        {"c": chat_id, "u": carol["username"]},
        token=alice["token"],
    )
    usernames = {m["username"] for m in data["addMember"]["members"]}
    assert carol["username"] in usernames


async def test_presence_changed_subscription(client, server):
    alice = await signup(client, "Alice")
    bob = await signup(client, "Bob")
    await make_group(client, alice, bob)

    async with ws_subscribe(
        server["ws"],
        bob["token"],
        "subscription($u:[ID!]!){presenceChanged(userIds:$u){id online}}",
        {"u": [alice["id"]]},
    ) as events:
        await asyncio.sleep(0.3)
        await gql_data(client, "mutation{heartbeat}", token=alice["token"])
        payload = await asyncio.wait_for(events.__anext__(), timeout=10)
        assert payload["data"]["presenceChanged"]["id"] == alice["id"]

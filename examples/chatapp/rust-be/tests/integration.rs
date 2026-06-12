//! End-to-end suite: boots the real backend (axum + async-graphql + Forge) against a
//! freshly created Postgres database, drives the GraphQL API over HTTP and over the
//! graphql-transport-ws socket, then drops the database. Covers every scenario in the
//! SPEC "Per-backend test suite". No skips: a single ordered run shares one server so
//! realtime fan-out between two principals is exercised against the live process.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

mod common;
use common::Backend;

/// A connected graphql-transport-ws subscription, yielding `next` payloads.
struct Sub {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Sub {
    /// Open the socket, authenticate via `connectionParams.authorization`, await the
    /// ack, and start one subscription operation.
    async fn start(ws_url: &str, token: &str, query: &str, variables: Value) -> Self {
        let mut req = ws_url.into_client_request().unwrap();
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static("graphql-transport-ws"),
        );
        let (mut stream, _) = tokio_tungstenite::connect_async(req).await.unwrap();

        send(
            &mut stream,
            json!({"type":"connection_init","payload":{"authorization": format!("Bearer {token}")}}),
        )
        .await;
        let ack = recv_json(&mut stream).await;
        assert_eq!(ack["type"], "connection_ack", "expected ack, got {ack}");

        send(
            &mut stream,
            json!({"id":"1","type":"subscribe","payload":{"query":query,"variables":variables}}),
        )
        .await;
        Sub { stream }
    }

    /// Next `next` payload's `data`, or panic on timeout. Ignores ka/ping frames.
    async fn next_data(&mut self) -> Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), recv_json(&mut self.stream))
                .await
                .expect("subscription event did not arrive in time");
            match msg["type"].as_str() {
                Some("next") => return msg["payload"]["data"].clone(),
                Some("ping") => {
                    send(&mut self.stream, json!({"type":"pong"})).await;
                }
                Some("ka") | Some("pong") => continue,
                Some("error") | Some("complete") => panic!("subscription ended early: {msg}"),
                _ => continue,
            }
        }
    }
}

async fn send<S>(stream: &mut S, v: Value)
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Debug,
{
    stream.send(Message::text(v.to_string())).await.unwrap();
}

async fn recv_json<S>(stream: &mut S) -> Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => panic!("ws closed unexpectedly"),
            Some(Ok(_)) => continue,
            Some(Err(e)) => panic!("ws error: {e}"),
        }
    }
}

#[tokio::test]
async fn full_suite() {
    let be = Backend::start().await;

    // --- signup -> session (two users) ---
    let alice = be.signup("alice", "Alice", "hunter2").await;
    let bob = be.signup("bob", "Bob", "hunter2").await;
    assert!(!alice.token.is_empty() && !bob.token.is_empty());

    // signup rejects taken username (PRECONDITION) and short credentials (INVALID).
    let dup = be
        .gql(
            &alice.token,
            "mutation($u:String!,$d:String!,$p:String!){signup(username:$u,displayName:$d,password:$p){token}}",
            json!({"u":"alice","d":"A","p":"hunter2"}),
        )
        .await;
    assert_eq!(error_code(&dup), "PRECONDITION");

    // me returns the user; unauthenticated me is null, not an error.
    let me = be
        .gql(&alice.token, "{me{username displayName}}", json!({}))
        .await;
    assert_eq!(me["data"]["me"]["username"], "alice");
    let anon = be.gql("", "{me{id}}", json!({})).await;
    assert!(anon["data"]["me"].is_null() && anon.get("errors").is_none());

    // --- create group chat; both members see it ---
    let chat = be
        .gql(
            &alice.token,
            "mutation($t:String!,$m:[String!]!){createChat(kind:GROUP,title:$t,memberUsernames:$m){id members{username}}}",
            json!({"t":"Team","m":["bob"]}),
        )
        .await;
    let chat_id = chat["data"]["createChat"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let members = chat["data"]["createChat"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);

    let bob_chats = be.gql(&bob.token, "{chats{id}}", json!({})).await;
    assert!(
        bob_chats["data"]["chats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == chat_id.as_str())
    );

    // non-member access is NOT_FOUND.
    let carol = be.signup("carol", "Carol", "hunter2").await;
    let denied = be
        .gql(
            &carol.token,
            "query($id:ID!){chat(id:$id){id}}",
            json!({"id": chat_id}),
        )
        .await;
    assert_eq!(error_code(&denied), "NOT_FOUND");

    // --- send + receive a message live over a subscription ---
    let mut msg_sub = Sub::start(
        &be.ws_url,
        &bob.token,
        "subscription($id:ID!){messageAdded(chatId:$id){id body sender{username} receipts{user{username}}}}",
        json!({"id": chat_id}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let sent = be
        .send_message(&alice.token, &chat_id, "hello bob", None)
        .await;
    let live = msg_sub.next_data().await;
    assert_eq!(live["messageAdded"]["body"], "hello bob");
    assert_eq!(live["messageAdded"]["sender"]["username"], "alice");
    assert_eq!(live["messageAdded"]["id"], sent.as_str());

    // --- typing event (suppresses the caller's own) ---
    let mut typing_sub = Sub::start(
        &be.ws_url,
        &bob.token,
        "subscription($id:ID!){typing(chatId:$id){user{username} typing}}",
        json!({"id": chat_id}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    be.gql(
        &alice.token,
        "mutation($id:ID!){setTyping(chatId:$id,typing:true)}",
        json!({"id": chat_id}),
    )
    .await;
    let typing = typing_sub.next_data().await;
    assert_eq!(typing["typing"]["user"]["username"], "alice");
    assert_eq!(typing["typing"]["typing"], true);

    // --- unread increments via fanout worker, then clears on markRead ---
    wait_until(Duration::from_secs(5), || async {
        let v = be
            .gql(
                &bob.token,
                "query($id:ID!){chat(id:$id){unread}}",
                json!({"id": chat_id}),
            )
            .await;
        v["data"]["chat"]["unread"].as_i64() == Some(1)
    })
    .await;

    let mut receipt_sub = Sub::start(
        &be.ws_url,
        &alice.token,
        "subscription($id:ID!){receiptChanged(chatId:$id){messageId user{username} readAt}}",
        json!({"id": chat_id}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    be.gql(
        &bob.token,
        "mutation($c:ID!,$m:ID!){markRead(chatId:$c,messageId:$m)}",
        json!({"c": chat_id, "m": sent}),
    )
    .await;
    // read receipt turns read, live.
    let rc = receipt_sub.next_data().await;
    assert_eq!(rc["receiptChanged"]["user"]["username"], "bob");
    assert!(rc["receiptChanged"]["readAt"].is_string());
    // unread cleared.
    let cleared = be
        .gql(
            &bob.token,
            "query($id:ID!){chat(id:$id){unread}}",
            json!({"id": chat_id}),
        )
        .await;
    assert_eq!(cleared["data"]["chat"]["unread"], 0);

    // --- presence: online via heartbeat, offline after kv TTL expiry ---
    be.gql(&alice.token, "mutation{heartbeat}", json!({})).await;
    let online = be
        .gql(
            &alice.token,
            "query($ids:[ID!]!){presence(userIds:$ids){username online}}",
            json!({"ids":[alice.id]}),
        )
        .await;
    assert_eq!(online["data"]["presence"][0]["online"], true);
    // APP_PRESENCE_TTL_SECS=1 in the harness, so the key lapses quickly.
    wait_until(Duration::from_secs(6), || async {
        let v = be
            .gql(
                &alice.token,
                "query($ids:[ID!]!){presence(userIds:$ids){online}}",
                json!({"ids":[alice.id]}),
            )
            .await;
        v["data"]["presence"][0]["online"] == Value::Bool(false)
    })
    .await;

    // --- attachment upload: presign -> PUT -> send -> media.downloadUrl fetch ---
    let ticket = be
        .gql(
            &alice.token,
            "mutation($id:ID!){requestUpload(chatId:$id){key uploadUrl maxBytes}}",
            json!({"id": chat_id}),
        )
        .await;
    let key = ticket["data"]["requestUpload"]["key"]
        .as_str()
        .unwrap()
        .to_string();
    let upload_url = ticket["data"]["requestUpload"]["uploadUrl"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ticket["data"]["requestUpload"]["maxBytes"]
            .as_i64()
            .unwrap()
            > 0
    );

    let put = be
        .client
        .put(be.abs(&upload_url))
        .header("content-type", "text/plain")
        .body("attachment-bytes")
        .send()
        .await
        .unwrap();
    assert!(put.status().is_success(), "PUT failed: {}", put.status());

    let with_media = be
        .gql(
            &alice.token,
            "mutation($c:ID!,$k:String){sendMessage(chatId:$c,body:\"\",mediaKey:$k){id media{key downloadUrl contentType}}}",
            json!({"c": chat_id, "k": key}),
        )
        .await;
    let media = &with_media["data"]["sendMessage"]["media"];
    assert_eq!(media["key"], key.as_str());
    assert_eq!(media["contentType"], "text/plain");
    let dl = be
        .client
        .get(be.abs(media["downloadUrl"].as_str().unwrap()))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.text().await.unwrap(), "attachment-bytes");

    // sending empty with no media is INVALID.
    let empty = be
        .gql(
            &alice.token,
            "mutation($c:ID!){sendMessage(chatId:$c,body:\"   \"){id}}",
            json!({"c": chat_id}),
        )
        .await;
    assert_eq!(error_code(&empty), "INVALID");

    // --- rate limit throttles a send burst (5 / 10s, fail-open) ---
    let dave = be.signup("dave", "Dave", "hunter2").await;
    let solo = be
        .gql(
            &dave.token,
            "mutation($m:[String!]!){createChat(kind:GROUP,title:\"solo\",memberUsernames:$m){id}}",
            json!({"m":[]}),
        )
        .await;
    let solo_id = solo["data"]["createChat"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut limited = false;
    for i in 0..12 {
        let r = be
            .gql(
                &dave.token,
                "mutation($c:ID!,$b:String!){sendMessage(chatId:$c,body:$b){id}}",
                json!({"c": solo_id, "b": format!("burst {i}")}),
            )
            .await;
        if error_code(&r) == "LIMIT" {
            limited = true;
            break;
        }
    }
    assert!(limited, "send burst was never rate-limited");

    // --- disappearing message vanishes after its ttl ---
    be.gql(
        &alice.token,
        "mutation($id:ID!){setDisappearing(chatId:$id,enabled:true){disappearingSeconds}}",
        json!({"id": chat_id}),
    )
    .await;
    // APP_DISAPPEARING_SECS=1 in the harness.
    let vanishing = be
        .send_message(&alice.token, &chat_id, "self-destruct", None)
        .await;
    wait_until(Duration::from_secs(15), || async {
        let v = be
            .gql(
                &alice.token,
                "query($id:ID!){messages(chatId:$id){id}}",
                json!({"id": chat_id}),
            )
            .await;
        v["data"]["messages"]
            .as_array()
            .map(|a| a.iter().all(|m| m["id"] != vanishing.as_str()))
            .unwrap_or(false)
    })
    .await;
    be.gql(
        &alice.token,
        "mutation($id:ID!){setDisappearing(chatId:$id,enabled:false){id}}",
        json!({"id": chat_id}),
    )
    .await;

    // --- reactions feature flag toggles (forge config) ---
    let toggled = be
        .gql(
            &alice.token,
            "mutation{setReactionsRollout(percent:100)}",
            json!({}),
        )
        .await;
    assert_eq!(toggled["data"]["setReactionsRollout"], true);

    // --- api key authenticates a request ---
    let apikey = be
        .gql(
            &alice.token,
            "mutation($l:String!){createApiKey(label:$l){id secret}}",
            json!({"l":"cli"}),
        )
        .await;
    let secret = apikey["data"]["createApiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(secret.starts_with("fk_"));
    let via_key = be.gql(&secret, "{me{username}}", json!({})).await;
    assert_eq!(via_key["data"]["me"]["username"], "alice");

    // --- opsStats reflects online + DLQ ---
    be.gql(&bob.token, "mutation{heartbeat}", json!({})).await;
    be.gql(&alice.token, "mutation{triggerFailingJob}", json!({}))
        .await;
    wait_until(Duration::from_secs(8), || async {
        let v = be
            .gql(&alice.token, "{opsStats{onlineCount dlqCount}}", json!({}))
            .await;
        let stats = &v["data"]["opsStats"];
        stats["onlineCount"].as_i64().unwrap_or(0) >= 1
            && stats["dlqCount"].as_i64().unwrap_or(0) >= 1
    })
    .await;

    // --- logoutAll revokes other sessions ---
    let alice2 = be.login("alice", "hunter2").await;
    be.gql(&alice.token, "mutation{logoutAll}", json!({})).await;
    let revoked = be.gql(&alice2.token, "{me{id}}", json!({})).await;
    assert!(
        revoked["data"]["me"].is_null(),
        "old session should be revoked"
    );

    be.teardown().await;
}

fn error_code(resp: &Value) -> &str {
    resp.get("errors")
        .and_then(|e| e.get(0))
        .and_then(|e| e["extensions"]["code"].as_str())
        .unwrap_or("")
}

/// Poll `cond` until it returns true or the deadline elapses (then panic). Used for
/// the asynchronous paths: worker fan-out, kv TTL expiry, scheduled reaping, DLQ.
async fn wait_until<F, Fut>(timeout: Duration, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cond().await {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("condition not met within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

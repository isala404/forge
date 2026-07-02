use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

fn admin_url() -> String {
    std::env::var("CHATAPP_TEST_ADMIN_URL")
        .or_else(|_| std::env::var("TEST_DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://postgres:forge@127.0.0.1:5432/postgres".to_string())
}

fn database_url(name: &str) -> String {
    let admin = admin_url();
    let base = admin
        .rsplit_once('/')
        .map_or(admin.as_str(), |(base, _)| base);
    format!("{base}/{name}")
}

pub struct Session {
    pub id: String,
    pub token: String,
}

pub struct Backend {
    pub client: reqwest::Client,
    pub base: String,
    pub http_url: String,
    pub ws_url: String,
    db_name: String,
    child: std::process::Child,
}

impl Backend {
    pub async fn start() -> Self {
        let db_name = format!("chatapp_rust_test_{}", Uuid::new_v4().simple());
        create_database(&db_name).await;
        let db_url = database_url(&db_name);

        let port = free_port();
        let bin = env!("CARGO_BIN_EXE_chatapp-rust-be");
        let child = std::process::Command::new(bin)
            .env("FORGE_POSTGRES_URL", &db_url)
            .env("BIND", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("FORGE_BLOB_SIGNING_SECRET", "test-secret")
            // "*" = any authenticated user is an admin, so the ops tests can exercise the
            // gated mutations without knowing a user id at boot. Real deploys list ids.
            .env("ADMIN_USER_IDS", "*")
            // Short TTLs so presence-offline and disappearing-message paths finish fast.
            .env("APP_PRESENCE_TTL_SECS", "1")
            .env("APP_DISAPPEARING_SECS", "1")
            .env("APP_SCHEDULER_MS", "500")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn backend binary");

        let base = format!("http://127.0.0.1:{port}");
        let be = Backend {
            client: reqwest::Client::new(),
            http_url: format!("{base}/graphql"),
            ws_url: format!("ws://127.0.0.1:{port}/graphql"),
            base,
            db_name,
            child,
        };
        be.await_ready().await;
        be
    }

    async fn await_ready(&self) {
        let health = format!("{}/healthz", self.base);
        for _ in 0..200 {
            if let Ok(r) = self.client.get(&health).send().await
                && r.status().is_success()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("backend did not become ready");
    }

    /// Run a GraphQL operation. `token` empty => no Authorization header.
    pub async fn gql(&self, token: &str, query: &str, variables: Value) -> Value {
        let mut req = self
            .client
            .post(&self.http_url)
            .json(&json!({"query": query, "variables": variables}));
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        req.send().await.unwrap().json().await.unwrap()
    }

    pub async fn signup(&self, username: &str, display: &str, password: &str) -> Session {
        let v = self
            .gql(
                "",
                "mutation($u:String!,$d:String!,$p:String!){signup(username:$u,displayName:$d,password:$p){token user{id}}}",
                json!({"u": username, "d": display, "p": password}),
            )
            .await;
        Session {
            id: v["data"]["signup"]["user"]["id"]
                .as_str()
                .unwrap()
                .to_string(),
            token: v["data"]["signup"]["token"].as_str().unwrap().to_string(),
        }
    }

    pub async fn login(&self, username: &str, password: &str) -> Session {
        let v = self
            .gql(
                "",
                "mutation($u:String!,$p:String!){login(username:$u,password:$p){token user{id}}}",
                json!({"u": username, "p": password}),
            )
            .await;
        Session {
            id: v["data"]["login"]["user"]["id"]
                .as_str()
                .unwrap()
                .to_string(),
            token: v["data"]["login"]["token"].as_str().unwrap().to_string(),
        }
    }

    pub async fn send_message(
        &self,
        token: &str,
        chat_id: &str,
        body: &str,
        media: Option<&str>,
    ) -> String {
        let v = self
            .gql(
                token,
                "mutation($c:ID!,$b:String!,$m:String){sendMessage(chatId:$c,body:$b,mediaKey:$m){id}}",
                json!({"c": chat_id, "b": body, "m": media}),
            )
            .await;
        v["data"]["sendMessage"]["id"].as_str().unwrap().to_string()
    }

    /// Resolve a possibly-relative URL (presigned blob URLs point at `/api/files/...`)
    /// against the backend origin.
    pub fn abs(&self, url: &str) -> String {
        if url.starts_with("http") {
            url.to_string()
        } else {
            format!("{}{}", self.base, url)
        }
    }

    pub async fn teardown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop_database(&self.db_name).await;
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // If a test panics before `teardown`, still reap the child.
        let _ = self.child.kill();
    }
}

async fn create_database(name: &str) {
    let admin = admin_url();
    let mut conn = PgConnection::connect(&admin)
        .await
        .expect("connect to admin postgres db");
    // name is a generated uuid suffix, safe to interpolate; CREATE DATABASE cannot bind.
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await
        .expect("create test database");
    conn.close().await.ok();
}

async fn drop_database(name: &str) {
    let admin = admin_url();
    if let Ok(mut conn) = PgConnection::connect(&admin).await {
        let _ = conn
            .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
            .await;
        conn.close().await.ok();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

//! Linkly — a tiny URL shortener, the "real SaaS app" dogfood for Forge.
//!
//! An axum service that uses Forge as its backend library: `kv` stores the links
//! and click counters, `queue` carries click events a background worker aggregates.
//! The frontend (src/index.html, vendored axios) talks to the JSON API.
//!
//! Run: `docker compose up -d db` then `cargo run`, and open http://127.0.0.1:8787.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Json, Router};
use forge::{Bytes, EnqueueOpts, Forge, ForgeConfig, QueueExt, SetOpts};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct CreateReq {
    url: String,
}

#[derive(Serialize)]
struct CreateResp {
    code: String,
    short_url: String,
}

#[derive(Serialize)]
struct LinkView {
    code: String,
    url: String,
    clicks: i64,
}

#[derive(Serialize)]
struct StatsResp {
    total_clicks: i64,
}

/// Queue payload for an async click event.
#[derive(Serialize, Deserialize)]
struct Click {
    code: String,
}

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn internal(e: forge::ForgeError) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pg = std::env::var("FORGE_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:forge@localhost:5432/forge_dev".to_string());
    let forge = Forge::init(ForgeConfig::new(pg)).await?;

    // Background analytics worker: drain the click queue, aggregate into kv.
    spawn_click_worker(forge.clone());

    let app = Router::new()
        .route("/", get(index))
        .route("/axios.min.js", get(axios_js))
        .route("/api/links", post(create_link).get(list_links))
        .route("/api/stats", get(stats))
        .route("/r/{code}", get(redirect))
        .with_state(forge);

    let addr = "127.0.0.1:8787";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("Linkly running at http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn spawn_click_worker(forge: Forge) {
    tokio::spawn(async move {
        let handler_forge = forge.clone();
        forge
            .worker("clicks")
            .concurrency(4)
            .run_until(std::future::pending::<()>(), move |job| {
                let forge = handler_forge.clone();
                async move {
                    let click: Click = job.payload_json().map_err(|e| e.to_string())?;
                    forge
                        .kv()
                        .incr(&format!("clicks:{}", click.code), 1)
                        .await
                        .map_err(|e| e.to_string())?;
                    forge
                        .kv()
                        .incr("stats:total_clicks", 1)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok::<(), String>(())
                }
            })
            .await;
    });
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn axios_js() -> impl IntoResponse {
    (
        [("content-type", "application/javascript")],
        include_str!("axios.min.js"),
    )
}

async fn create_link(
    State(forge): State<Forge>,
    Json(req): Json<CreateReq>,
) -> ApiResult<Json<CreateResp>> {
    let url = req.url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err((
            StatusCode::BAD_REQUEST,
            "url must start with http:// or https://".to_string(),
        ));
    }
    // kv INCR as an atomic id sequence; base62 for a short code.
    let seq = forge.kv().incr("linkly:seq", 1).await.map_err(internal)?;
    let code = base62(seq as u64);
    forge
        .kv()
        .set(&format!("link:{code}"), Bytes::from(url), SetOpts::new())
        .await
        .map_err(internal)?;
    Ok(Json(CreateResp {
        short_url: format!("/r/{code}"),
        code,
    }))
}

async fn list_links(State(forge): State<Forge>) -> ApiResult<Json<Vec<LinkView>>> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let (keys, next) = forge
            .kv()
            .scan("link:", cursor, 100)
            .await
            .map_err(internal)?;
        for key in keys {
            let code = key.strip_prefix("link:").unwrap_or(&key).to_string();
            let url = forge
                .kv()
                .get(&key)
                .await
                .map_err(internal)?
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let clicks = forge
                .kv()
                .incr(&format!("clicks:{code}"), 0)
                .await
                .map_err(internal)?;
            out.push(LinkView { code, url, clicks });
        }
        let Some(c) = next else { break };
        cursor = Some(c);
    }
    out.sort_by(|a, b| b.clicks.cmp(&a.clicks).then_with(|| a.code.cmp(&b.code)));
    Ok(Json(out))
}

async fn stats(State(forge): State<Forge>) -> ApiResult<Json<StatsResp>> {
    let total_clicks = forge
        .kv()
        .incr("stats:total_clicks", 0)
        .await
        .map_err(internal)?;
    Ok(Json(StatsResp { total_clicks }))
}

async fn redirect(State(forge): State<Forge>, Path(code): Path<String>) -> ApiResult<Redirect> {
    match forge
        .kv()
        .get(&format!("link:{code}"))
        .await
        .map_err(internal)?
    {
        Some(bytes) => {
            let url = String::from_utf8_lossy(&bytes).into_owned();
            // Fire the analytics event onto the queue; the worker counts it.
            forge
                .queue()
                .enqueue_json("clicks", &Click { code }, EnqueueOpts::new())
                .await
                .map_err(internal)?;
            Ok(Redirect::to(&url))
        }
        None => Err((StatusCode::NOT_FOUND, "no such link".to_string())),
    }
}

/// Base62-encode a counter into a short URL code.
fn base62(mut n: u64) -> String {
    const ALPHABET: &[u8; 62] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        if let Some(&c) = ALPHABET.get((n % 62) as usize) {
            out.push(c);
        }
        n /= 62;
    }
    out.reverse();
    String::from_utf8(out).expect("base62 output is ASCII")
}

# Direct upload/download with presigned blob URLs

When a browser needs to push a file straight to storage (or pull one back) without streaming the bytes through your request handlers, mint a presigned URL. `presign_upload` / `presign_download` return HMAC-SHA256-signed URLs scoped to one key, one method, and one expiry (plus a `max_bytes` cap on upload). On the Postgres and filesystem backends those URLs resolve through `forge.blob_router()`, an axum router you mount at the same path the URLs point to. Presigning is opt-in: it needs `blob_signing_secret` set, and any presign call without it returns `ForgeError::Config`, never a silent failure.

## The flow

1. Configure `blob_signing_secret` and (optionally) `blob_base_url`.
2. Mount `forge.blob_router()` under `blob_base_url`.
3. Sign an upload URL server-side, hand it to the client, client `PUT`s the bytes directly.
4. Sign a download URL when serving the object back.

```rust
use std::sync::Arc;
use std::time::Duration;

use axum::{routing::post, Json, Router};
use forge::{Forge, ForgeConfig};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    forge: Forge,
}

#[derive(Serialize)]
struct UploadTicket {
    key: String,
    upload_url: String,
    max_bytes: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The secret enables presign_*; without it those calls + blob_router() return Config.
    let forge = Forge::init(
        ForgeConfig::new("postgres://localhost/myapp")
            .with_blob_signing_secret(std::env::var("FORGE_BLOB_SIGNING_SECRET")?)
            .with_blob_base_url("/_forge/blob"),
    )
    .await?;

    let state = AppState {
        forge: forge.clone(),
    };

    let app = Router::new()
        .route("/uploads", post(request_upload))
        .route("/downloads", post(request_download))
        // Mount the router at exactly the configured blob_base_url; the signed URLs
        // point here. blob_router() errors with Config if no signing secret is set.
        .nest("/_forge/blob", forge.blob_router()?)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// Client asks for an upload slot; server picks the key and the size cap, then signs.
async fn request_upload(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<UploadTicket>, axum::http::StatusCode> {
    let key = format!("media/{}", Uuid::new_v4());
    let max_bytes: u64 = 5 * 1024 * 1024; // 5 MiB ceiling for this PUT

    let upload_url = state
        .forge
        .blob()
        // (key, expires, max_bytes). expires in (0, 7 days]; max_bytes <= 50 MiB.
        .presign_upload(&key, Duration::from_secs(600), max_bytes)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(UploadTicket {
        key,
        upload_url,
        max_bytes,
    }))
}

#[derive(Deserialize)]
struct DownloadReq {
    key: String,
}

// Sign a short-lived GET URL for an object you already authorized this caller to read.
async fn request_download(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(req): Json<DownloadReq>,
) -> Result<Json<String>, axum::http::StatusCode> {
    let url = state
        .forge
        .blob()
        // (key, expires). No max_bytes on download.
        .presign_download(&req.key, Duration::from_secs(3600))
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(url))
}
```

The client then uploads with a plain `PUT` to `upload_url` (set `Content-Type`; the router stores it on the object), and downloads with a `GET` to the download URL. No bytes pass through your handlers.

The `blob-router` and `postgres` features must be enabled on the `forge` crate for `blob_router()` to exist.

## Authorize *before* you sign

A presigned URL is a bearer capability for one key. Forge signs whatever key you pass; it does not know whether this caller may touch it. Do the authorization and (for uploads) rate-limiting in your handler before calling `presign_*`. The chatapp example fails the upload rate-limit bucket *closed* (a backend hiccup must not let an abuser mint unlimited upload URLs) and namespaces keys by chat (`media/<chatId>/<uuid>`) so a key minted for one chat can't be replayed against another. Picking the key server-side, rather than trusting a client-supplied one, is what keeps the capability scoped.

## Serving the URLs yourself: `verify_presigned`

If a language binding or a custom handler serves the presigned URLs instead of mounting `blob_router()`, run the same check the router runs:

```rust
// expires (epoch seconds), max_bytes, and sig come straight off the URL's query params.
let ok = forge
    .blob()
    .verify_presigned("GET", &key, expires_epoch, max_bytes, &sig)
    .await?;
```

It returns `Ok(true)` only when the signature matches the configured secret **and** the URL has not expired; `Ok(false)` for a bad/tampered signature or an expired URL; `Err(Config)` if no secret is set; `Err(Invalid)` if `method` isn't `GET` or `PUT`. The router maps these to `403` (false), `500` (Config — a server misconfig, not a client error), and `400` (anything else). On upload it also re-checks the body length against the signed `max_bytes` and returns `413` if it overflows — the signed cap is a fence, not a hint.

## The attachment + nosniff defense

Stored objects are caller-supplied bytes served from your own origin. The download handler answers with the stored `content_type` but always adds `Content-Disposition: attachment` and `X-Content-Type-Options: nosniff`. That stops a browser from rendering a stored HTML or SVG payload inline (which would be stored XSS) or sniffing it into an active type. Subresource loads (`<img>`, `<video>`) ignore `Content-Disposition`, so legitimate inline media still works; only top-level navigation to the URL is forced to download. If you serve presigned URLs yourself, set both headers.

## Contract guarantees and gotchas

- **No secret => `Config`, consistently.** `presign_upload`, `presign_download`, `verify_presigned`, and `blob_router()` all return `ForgeError::Config` when `blob_signing_secret` is unset. Init still succeeds and the CRUD surface (`put`/`get`/`head`/`delete`/`list`) works fully without a secret — only presigning needs it. `blob_router()` checks `presign_ready()` and fails early so you don't mount a router that can't verify anything.
- **Single-key, single-method, time-bound.** A URL is bound to one key, one method (`GET` xor `PUT`), one expiry, and (upload) one `max_bytes`. It can't be replayed against a different key, the other method, or a larger body.
- **No snapshot.** A download serves the object's bytes *at fetch time*. If the object was overwritten or deleted after signing, the fetch reflects current state — current bytes, or `404` at the router.
- **Limits.** `expires` must be in `(0, 7 days]` (zero/negative => `Invalid`, over => `Limit`); upload `max_bytes` must be `<= 50 MiB` (the `put` ceiling). The router raises axum's default 2 MiB body limit to the 50 MiB cap so uploads up to the cap go through, with the signed `max_bytes` still fencing each `PUT`.
- **Mount path must match `blob_base_url`.** The signed URLs are built relative to `blob_base_url` (default `/_forge/blob`); nest the router at that exact path or the URLs won't resolve.

## Large objects: filesystem backend

The default Postgres backend stores object bytes in a `BYTEA` column, atomic with your surrounding app SQL but heavy on the WAL for large files. To keep big objects out of the database, switch byte storage to a local directory (metadata stays in Postgres) via `BlobBackendConfig::Filesystem`:

```rust
use forge::{BlobBackendConfig, ForgeConfig};

let cfg = ForgeConfig::new("postgres://localhost/myapp")
    .with_blob_signing_secret(secret)
    .with_filesystem_blob("/var/lib/app/blobs"); // == BlobBackendConfig::Filesystem { root }
```

Or pick it without touching code, the same way across all three language bindings: set `FORGE_BLOB_BACKEND=filesystem` and `FORGE_BLOB_FS_ROOT=/var/lib/app/blobs` and build with `ForgeConfig::from_env()`. (`from_env()` errors with `Config` if `filesystem` is selected without `FORGE_BLOB_FS_ROOT`.) The presign API, the router, and all app code are identical across backends — only where the bytes live changes.

Tradeoffs to weigh before choosing it: a filesystem `put` is **not atomic with your app SQL** (the Postgres backend's `BYTEA` write commits in the same transaction context; a filesystem write doesn't), so a crash between the file write and a related row commit can leave them out of sync. And the directory must be a **shared mount** for multi-replica deploys — every replica that serves the blob router needs to see the same files, or a download lands on a replica without the bytes. `forge.maintain()` reclaims orphaned files on the filesystem backend.

## Binding notes (Node / Python)

Rust is canonical. The signing scheme is backend- and binding-agnostic, so a presigned URL minted in any language verifies the same way. The key difference: a binding that serves presigned URLs through its own HTTP handler rather than mounting the Rust `blob_router()` must call `verify_presigned(method, key, expires_epoch, max_bytes, sig)` itself and set the same `Content-Disposition: attachment` + `X-Content-Type-Options: nosniff` headers on downloads — the defense lives in the handler, not the signer. Backend selection via `FORGE_BLOB_BACKEND` / `FORGE_BLOB_FS_ROOT` is identical across all three.

//! Optional axum router that resolves presigned blob URLs against the Postgres
//! backend (feature `blob-router`). Mount it under the same path the presigned URLs
//! point at, e.g. `app.nest("/_forge/blob", forge.blob_router()?)`.
//!
//! Each request carries the key, expiry, size cap, and HMAC signature as query
//! params; the router verifies the signature (same [`super::sign`] code as the
//! signer), enforces the expiry and size cap, then performs the equivalent get/put.

// Returning the axum `Response` in a `Result::Err` is the idiomatic short-circuit for
// these handlers; its size is axum's, not ours to box away.
#![allow(clippy::result_large_err)]

use super::pg::PgBlob;
use super::sign::{self, Method};
use super::{Blob, MAX_OBJECT_BYTES, PutOpts};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// The signed query parameters on every presigned request.
#[derive(Debug, Deserialize)]
struct Params {
    key: String,
    expires: i64,
    #[serde(default)]
    max_bytes: u64,
    sig: String,
}

/// Build the router. Returns the configured signing secret too, so the caller
/// (`Forge::blob_router`) can fail at init if presigning is unconfigured.
pub(crate) fn router(blob: Arc<PgBlob>) -> axum::Router {
    axum::Router::new()
        .route("/", get(download).put(upload))
        // axum's default body limit is 2 MiB; raise it to the object cap so uploads up
        // to MAX_OBJECT_BYTES go through (the signed `max_bytes` still fences each PUT).
        .layer(DefaultBodyLimit::max(MAX_OBJECT_BYTES))
        .with_state(blob)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Verify expiry + signature for `method`. Returns `Err(response)` on any failure.
fn check(blob: &PgBlob, method: Method, p: &Params) -> Result<(), Response> {
    let secret = blob
        .signing_secret()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    if p.expires <= now_secs() {
        return Err((StatusCode::FORBIDDEN, "presigned url expired").into_response());
    }
    if sign::verify(secret, method, &p.key, p.expires, p.max_bytes, &p.sig) {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "invalid signature").into_response())
    }
}

async fn download(State(blob): State<Arc<PgBlob>>, Query(p): Query<Params>) -> Response {
    if let Err(resp) = check(&blob, Method::Get, &p) {
        return resp;
    }
    match blob.get(&p.key).await {
        Ok(Some(bytes)) => {
            let ct = blob
                .head(&p.key)
                .await
                .ok()
                .flatten()
                .map(|i| i.content_type)
                .unwrap_or_else(|| super::DEFAULT_CONTENT_TYPE.to_string());
            // Objects are caller-supplied bytes served from the app's own origin, so
            // never let the browser render them inline or sniff a different type — that
            // turns a stored HTML/SVG payload into stored XSS. `attachment` + `nosniff`
            // still allow `<img>`/`<video>` subresource loads, which ignore Content-Disposition.
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, ct),
                    (header::CONTENT_DISPOSITION, "attachment".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn upload(
    State(blob): State<Arc<PgBlob>>,
    Query(p): Query<Params>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = check(&blob, Method::Put, &p) {
        return resp;
    }
    // The signed cap fences the body size (Precondition, not just a kindness).
    if body.len() as u64 > p.max_bytes {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "body exceeds signed max_bytes",
        )
            .into_response();
    }
    let mut opts = PutOpts::new();
    if let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        opts = opts.with_content_type(ct);
    }
    match blob.put(&p.key, body, opts).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(crate::ForgeError::Limit(_)) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "object too large").into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

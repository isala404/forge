//! Serves Forge presigned blob URLs. Forge ships no built-in HTTP router, so this
//! mounts the equivalent route at the configured presign prefix (`/api/files`). Each
//! request carries the key, expiry, size cap, and HMAC signature as query params; we
//! verify them with `Blob::verify_presigned` (the exact check the signer enforces) and
//! then do the get/put against blob storage. The node and python backends carry the
//! same hand-rolled route. See their `server.ts` / `blob_router.py`.

// Returning the axum `Response` in a `Result::Err` is the idiomatic short-circuit for
// these handlers.
#![allow(clippy::result_large_err)]

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use forge::ForgeError;
use forge::blob::{DEFAULT_CONTENT_TYPE, MAX_CONTENT_TYPE_BYTES, MAX_OBJECT_BYTES};
use serde::Deserialize;

use crate::context::Ctx;

/// Cache directive on presigned downloads. A key can be overwritten, so the client may
/// cache the bytes but must revalidate each use; the `ETag` makes that a cheap `304`.
/// `private` keeps a shared proxy from caching one user's signed object.
const CACHE_CONTROL_VALUE: &str = "private, no-cache";

/// The signed query parameters on every presigned request.
#[derive(Debug, Deserialize)]
struct Params {
    key: String,
    expires: i64,
    #[serde(default)]
    max_bytes: u64,
    sig: String,
}

/// Build the router. State is the app `Ctx`; each handler resolves the blob backend
/// through `ctx.forge.blob()`.
pub fn router(ctx: Ctx) -> axum::Router {
    axum::Router::new()
        .route("/", get(download).put(upload))
        // axum's default body limit is 2 MiB; raise it to the object cap so uploads up
        // to MAX_OBJECT_BYTES go through (the signed `max_bytes` still fences each PUT).
        .layer(DefaultBodyLimit::max(MAX_OBJECT_BYTES))
        .with_state(ctx)
}

/// Insert a header, skipping it if the value isn't a valid header value (a stored
/// content type that somehow holds control bytes shouldn't 500 the whole download).
fn insert_header(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(v) = value.parse() {
        headers.insert(name, v);
    }
}

/// Does an `If-None-Match` header value match `etag` (already quoted)? Supports the
/// comma-separated list form and the `*` wildcard, per RFC 9110.
fn etag_matches(if_none_match: Option<&str>, etag: &str) -> bool {
    match if_none_match {
        None => false,
        Some(inm) => inm
            .split(',')
            .map(str::trim)
            .any(|candidate| candidate == "*" || candidate == etag),
    }
}

/// Verify expiry + signature for `method`. Returns `Err(response)` on any failure.
async fn check(ctx: &Ctx, method: &str, p: &Params) -> Result<(), Response> {
    match ctx
        .forge
        .blob()
        .verify_presigned(method, &p.key, p.expires, p.max_bytes, &p.sig)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => {
            Err((StatusCode::FORBIDDEN, "invalid or expired presigned url").into_response())
        }
        // No signing secret configured (Config) is a server misconfig, not a client error.
        Err(ForgeError::Config(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => Err((StatusCode::BAD_REQUEST, "malformed presigned url").into_response()),
    }
}

async fn download(State(ctx): State<Ctx>, Query(p): Query<Params>, headers: HeaderMap) -> Response {
    if let Err(resp) = check(&ctx, "GET", &p).await {
        return resp;
    }
    let blob = ctx.forge.blob();
    // One head() gives both the content type and the ETag for conditional requests.
    let info = blob.head(&p.key).await.ok().flatten();
    let etag = info.as_ref().map(|i| format!("\"{}\"", i.etag));

    // Honour `If-None-Match`: a cached client that already holds this exact object
    // (same ETag) gets a bodyless 304 instead of the full payload re-sent.
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    if let Some(etag) = etag.as_deref()
        && etag_matches(if_none_match, etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag.to_string()),
                (header::CACHE_CONTROL, CACHE_CONTROL_VALUE.to_string()),
            ],
        )
            .into_response();
    }

    match blob.get(&p.key).await {
        Ok(Some(bytes)) => {
            let ct = info
                .map(|i| i.content_type)
                .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
            // Objects are caller-supplied bytes served from the app's own origin, so
            // never let the browser render them inline or sniff a different type: that
            // turns a stored HTML/SVG payload into stored XSS. `attachment` + `nosniff`
            // still allow `<img>`/`<video>` subresource loads, which ignore Content-Disposition.
            let mut resp_headers = HeaderMap::new();
            insert_header(&mut resp_headers, header::CONTENT_TYPE, &ct);
            insert_header(&mut resp_headers, header::CONTENT_DISPOSITION, "attachment");
            insert_header(&mut resp_headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
            insert_header(
                &mut resp_headers,
                header::CACHE_CONTROL,
                CACHE_CONTROL_VALUE,
            );
            if let Some(etag) = etag.as_deref() {
                insert_header(&mut resp_headers, header::ETAG, etag);
            }
            (StatusCode::OK, resp_headers, bytes).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn upload(
    State(ctx): State<Ctx>,
    Query(p): Query<Params>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = check(&ctx, "PUT", &p).await {
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
    let mut opts = forge::PutOpts::new();
    if let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        // An over-long Content-Type is a bad request (the client controls this header),
        // distinct from an over-large body (413 below).
        if ct.len() > MAX_CONTENT_TYPE_BYTES {
            return (StatusCode::BAD_REQUEST, "Content-Type header is too long").into_response();
        }
        opts = opts.with_content_type(ct);
    }
    match ctx.forge.blob().put(&p.key, body, opts).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(ForgeError::Limit(_)) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "object too large").into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::etag_matches;

    #[test]
    fn etag_matches_exact_and_list_and_wildcard() {
        let etag = "\"abc123\"";
        assert!(etag_matches(Some("\"abc123\""), etag), "exact match → 304");
        assert!(
            etag_matches(Some("\"x\", \"abc123\" , \"y\""), etag),
            "match anywhere in the comma list"
        );
        assert!(etag_matches(Some("*"), etag), "wildcard always matches");
    }

    #[test]
    fn etag_does_not_match_when_absent_or_different() {
        let etag = "\"abc123\"";
        assert!(!etag_matches(None, etag), "no If-None-Match → full body");
        assert!(
            !etag_matches(Some("\"other\""), etag),
            "different etag → full body"
        );
        assert!(
            !etag_matches(Some("abc123"), etag),
            "unquoted is not a match"
        );
    }
}

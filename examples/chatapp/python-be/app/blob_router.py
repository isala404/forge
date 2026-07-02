from __future__ import annotations

from fastapi import APIRouter, Request, Response
from starlette.responses import PlainTextResponse

router = APIRouter()

# Forge's blob hard cap; mirrors the Rust router's DefaultBodyLimit(MAX_OBJECT_BYTES).
MAX_OBJECT_BYTES = 50 * 1024 * 1024

# Cache directive on presigned downloads, matching the Rust router: the client may
# cache the bytes but must revalidate each use (the ETag makes that a cheap 304);
# `private` keeps a shared proxy from caching one user's signed object.
CACHE_CONTROL = "private, no-cache"


def _etag_matches(if_none_match: str, etag: str) -> bool:
    """Does an If-None-Match value match `etag` (already quoted)? Supports the
    comma-separated list form and the `*` wildcard, per RFC 9110."""
    return any(c.strip() in ("*", etag) for c in if_none_match.split(","))


def _params(request: Request):
    q = request.query_params
    try:
        expires = int(q["expires"])
        max_bytes = int(q.get("max_bytes", "0"))
    except (KeyError, ValueError):
        return None
    key = q.get("key")
    sig = q.get("sig")
    if key is None or sig is None:
        return None
    return key, expires, max_bytes, sig


@router.get("/api/files")
async def download(request: Request):
    parsed = _params(request)
    if parsed is None:
        return PlainTextResponse("malformed presigned request", status_code=400)
    key, expires, max_bytes, sig = parsed
    forge = request.app.state.forge
    if not await forge.blob_verify_presign("GET", key, expires, max_bytes, sig):
        return PlainTextResponse("invalid or expired signature", status_code=403)

    # One head() gives both the content type and the ETag for conditional requests,
    # matching the Rust router. The ETag is the storage etag, quoted per RFC 9110.
    info = await forge.blob_head(key)
    ct = (info.content_type if info else None) or "application/octet-stream"
    etag = f'"{info.etag}"' if info else None

    # Honour If-None-Match: a cached client that already holds this exact object gets
    # a bodyless 304 instead of the full payload re-sent.
    inm = request.headers.get("if-none-match")
    if etag and inm and _etag_matches(inm, etag):
        return Response(
            status_code=304,
            headers={"ETag": etag, "Cache-Control": CACHE_CONTROL},
        )

    data = await forge.blob_get(key)
    if data is None:
        return PlainTextResponse("not found", status_code=404)
    # Match Forge's own router: never let a served blob render inline or be MIME-sniffed.
    headers = {
        "Content-Disposition": "attachment",
        "X-Content-Type-Options": "nosniff",
        "Cache-Control": CACHE_CONTROL,
    }
    if etag:
        headers["ETag"] = etag
    return Response(content=data, media_type=ct, headers=headers)


@router.put("/api/files")
async def upload(request: Request):
    parsed = _params(request)
    if parsed is None:
        return PlainTextResponse("malformed presigned request", status_code=400)
    key, expires, max_bytes, sig = parsed
    forge = request.app.state.forge
    if not await forge.blob_verify_presign("PUT", key, expires, max_bytes, sig):
        return PlainTextResponse("invalid or expired signature", status_code=403)
    # Stream with a hard ceiling so a valid-presign holder can't OOM the process by
    # sending gigabytes before the size check runs. The Rust router caps the body the
    # same way (DefaultBodyLimit); `await request.body()` would buffer it all first.
    cap = min(max_bytes, MAX_OBJECT_BYTES) if max_bytes > 0 else MAX_OBJECT_BYTES
    chunks: list[bytes] = []
    total = 0
    async for chunk in request.stream():
        total += len(chunk)
        if total > cap:
            return PlainTextResponse("upload exceeds signed max_bytes", status_code=413)
        chunks.append(chunk)
    body = b"".join(chunks)
    ct = request.headers.get("content-type") or "application/octet-stream"
    await forge.blob_put(key, body, ct)
    return Response(status_code=200)

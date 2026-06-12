"""Serves Forge presigned blob URLs.

forge-py does not expose `blob_router()`, so this mounts the equivalent route at the
default presign prefix (`/_forge/blob`). It verifies the HMAC signature and expiry with
`blob_verify_presign` (the exact check the built-in Rust router performs), then does the
get/put against blob storage. Without this, presigned URLs would not resolve."""

from __future__ import annotations

from fastapi import APIRouter, Request, Response
from starlette.responses import PlainTextResponse

router = APIRouter()

# Forge's blob hard cap; mirrors the Rust router's DefaultBodyLimit(MAX_OBJECT_BYTES).
MAX_OBJECT_BYTES = 50 * 1024 * 1024


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


@router.get("/_forge/blob")
async def download(request: Request):
    parsed = _params(request)
    if parsed is None:
        return PlainTextResponse("malformed presigned request", status_code=400)
    key, expires, max_bytes, sig = parsed
    forge = request.app.state.forge
    if not await forge.blob_verify_presign("GET", key, expires, max_bytes, sig):
        return PlainTextResponse("invalid or expired signature", status_code=403)
    data = await forge.blob_get(key)
    if data is None:
        return PlainTextResponse("not found", status_code=404)
    ct = await forge.blob_content_type(key) or "application/octet-stream"
    # Match Forge's own router: never let a served blob render inline or be MIME-sniffed.
    return Response(
        content=data,
        media_type=ct,
        headers={
            "Content-Disposition": "attachment",
            "X-Content-Type-Options": "nosniff",
        },
    )


@router.put("/_forge/blob")
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

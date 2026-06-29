from __future__ import annotations

import io
import json
import random
import string
import uuid
from datetime import UTC, datetime, timedelta
from typing import Annotated, Any

import segno
from fastapi import APIRouter, Header, HTTPException, Request, Response
from fastapi.responses import PlainTextResponse, RedirectResponse, StreamingResponse

from .types import Credentials, LinkCreate, OwnedLink, UserRecord
from .utils import (
    CLICKS_QUEUE,
    DEFAULT_MAX_LINKS,
    EXPIRE_QUEUE,
    RESERVED_SLUGS,
    SESSION_ABSOLUTE_SECS,
    SESSION_IDLE_SECS,
    SLUG_RE,
    bearer_token,
    click_topic,
    clicks_key,
    link_slug_key,
    now_iso,
    owner_key,
    public_user,
    qr_key,
    user_email_key,
    user_id_key,
    validate_credentials,
    validate_slug,
    validate_url,
)
from .worker import delete_link

_SLUG_CHARS = string.ascii_letters + string.digits

api = APIRouter()


def _random_slug(length: int = 7) -> str:
    return "".join(random.choices(_SLUG_CHARS, k=length))


@api.get("/healthz", response_class=PlainTextResponse)
async def healthz() -> str:
    return "ok"


@api.get("/api/meta")
async def meta(request: Request) -> dict[str, Any]:
    forge = request.app.state.forge
    custom_slugs = await forge.flag("custom_slugs", False)
    depth = await forge.queue_depth(CLICKS_QUEUE)
    return {
        "backend": "python",
        "forge": [
            {
                "primitive": line.primitive,
                "provider": line.provider,
                "durable": line.durable,
                "caveats": line.caveats,
            }
            for line in forge.backend_report()
        ],
        "features": {"customSlugs": custom_slugs},
        "clicksQueueDepth": {
            "visible": depth.visible,
            "inFlight": depth.in_flight,
            "delayed": depth.delayed,
        },
    }


@api.post("/api/signup", status_code=201)
async def signup(request: Request, body: Credentials) -> dict[str, Any]:
    forge = request.app.state.forge
    email, password = validate_credentials(body)

    limit = await forge.rate_limit_check("links-auth", email, 20, 60.0, True)
    if not limit.allowed:
        raise HTTPException(status_code=429, detail="too many auth attempts; try again soon")

    user = UserRecord(
        id=str(uuid.uuid4()),
        email=email,
        password_hash=await forge.hash_password(password),
    )

    inserted = await forge.kv_set(user_email_key(email), user.model_dump_json(), None, True)
    if not inserted:
        raise HTTPException(status_code=409, detail="email already registered")

    await forge.kv_set(user_id_key(user.id), user.model_dump_json())

    token = await forge.create_session(
        user.id, float(SESSION_IDLE_SECS), float(SESSION_ABSOLUTE_SECS)
    )
    return {"token": token, "user": public_user(user)}


@api.post("/api/login")
async def login(request: Request, body: Credentials) -> dict[str, Any]:
    forge = request.app.state.forge
    email, password = validate_credentials(body)

    limit = await forge.rate_limit_check("links-auth", email, 20, 60.0, True)
    if not limit.allowed:
        raise HTTPException(status_code=429, detail="too many auth attempts; try again soon")

    raw_user = await forge.kv_get(user_email_key(email))
    if raw_user is None:
        raise HTTPException(status_code=401, detail="invalid email or password")
    user = UserRecord.model_validate(json.loads(raw_user))

    if not await forge.verify_password(password, user.password_hash):
        raise HTTPException(status_code=401, detail="invalid email or password")

    token = await forge.create_session(
        user.id, float(SESSION_IDLE_SECS), float(SESSION_ABSOLUTE_SECS)
    )
    return {"token": token, "user": public_user(user)}


@api.post("/api/logout", status_code=204)
async def logout(
    request: Request,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> Response:
    await request.app.state.forge.revoke_session(bearer_token(authorization))
    return Response(status_code=204)


@api.get("/api/me")
async def me(
    request: Request,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> dict[str, Any]:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_user = await forge.kv_get(user_id_key(user_id))
    if raw_user is None:
        raise HTTPException(status_code=401, detail="authentication required")

    return {"user": public_user(UserRecord.model_validate(json.loads(raw_user)))}


@api.get("/api/links")
async def list_links(
    request: Request,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> dict[str, Any]:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_owner = await forge.kv_get(owner_key(user_id))
    owned = [OwnedLink.model_validate(item) for item in json.loads(raw_owner or "[]")]

    click_keys = [clicks_key(link.slug) for link in owned]
    click_vals: list[str | None] = await forge.kv_mget(click_keys) if click_keys else []

    links = []
    for link, raw_count in zip(owned, click_vals, strict=False):
        links.append({
            **link.model_dump(by_alias=True),
            "clicks": int(raw_count) if raw_count is not None else 0,
        })

    return {"links": links}


@api.post("/api/links", status_code=201)
async def create_link(
    request: Request,
    body: LinkCreate,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> dict[str, Any]:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    url = validate_url(body.url)

    max_links_raw = await forge.config_get("max_links_per_user")
    max_links = int(max_links_raw) if max_links_raw is not None else DEFAULT_MAX_LINKS
    raw_owner = await forge.kv_get(owner_key(user_id))
    owned_raw: list = json.loads(raw_owner or "[]")
    if len(owned_raw) >= max_links:
        raise HTTPException(status_code=409, detail="link limit reached")

    custom_slugs_on = await forge.flag("custom_slugs", False, user_id)
    if body.slug and custom_slugs_on:
        slug = validate_slug(body.slug)
    else:
        slug = None

    now = now_iso()
    expires_at: str | None = None
    if body.ttl_seconds and body.ttl_seconds > 0:
        expires_at = (
            (datetime.now(UTC) + timedelta(seconds=body.ttl_seconds))
            .isoformat()
            .replace("+00:00", "Z")
        )

    def _link_record_json(s: str) -> str:
        return json.dumps(
            {"slug": s, "url": url, "ownerId": user_id, "createdAt": now, "expiresAt": expires_at},
            separators=(",", ":"),
        )

    if slug is not None:
        reserved = await forge.kv_set(link_slug_key(slug), _link_record_json(slug), None, True)
        if not reserved:
            raise HTTPException(status_code=409, detail="slug already taken")
    else:
        for _ in range(5):
            candidate = _random_slug()
            record_json = _link_record_json(candidate)
            if await forge.kv_set(link_slug_key(candidate), record_json, None, True):
                slug = candidate
                break
        else:
            raise HTTPException(status_code=409, detail="slug already taken")

    # Owner lists are stored newest-first for the dashboard.
    owned_raw.insert(0, {"slug": slug, "url": url, "createdAt": now, "expiresAt": expires_at})
    await forge.kv_set(
        owner_key(user_id),
        json.dumps(owned_raw, separators=(",", ":")),
    )

    # segno's SVG writer emits bytes, so render into a BytesIO buffer.
    buf = io.BytesIO()
    segno.make(f"/{slug}", error="m").save(buf, kind="svg", scale=4, border=1)
    await forge.blob_put(qr_key(slug), buf.getvalue(), "image/svg+xml")

    if expires_at is not None:
        exp_epoch_ms = (
            datetime.fromisoformat(expires_at.replace("Z", "+00:00")).timestamp() * 1000
        )
        await forge.schedule_at(
            exp_epoch_ms,
            EXPIRE_QUEUE,
            json.dumps({"slug": slug}, separators=(",", ":")),
        )

    return {"slug": slug, "url": url, "createdAt": now, "expiresAt": expires_at, "clicks": 0}


@api.delete("/api/links/{slug}", status_code=204)
async def delete_link_route(
    request: Request,
    slug: str,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> Response:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_link = await forge.kv_get(link_slug_key(slug))
    if raw_link is None or json.loads(raw_link).get("ownerId") != user_id:
        raise HTTPException(status_code=404, detail="link not found")

    await delete_link(forge, slug)
    return Response(status_code=204)


@api.get("/api/links/{slug}/qr.svg")
async def get_qr(request: Request, slug: str) -> Response:
    svg_str = await request.app.state.forge.blob_get(qr_key(slug))
    if svg_str is None:
        raise HTTPException(status_code=404, detail="not found")
    return Response(content=svg_str, media_type="image/svg+xml")


@api.get("/api/links/{slug}/live")
async def live_clicks(request: Request, slug: str) -> StreamingResponse:
    forge = request.app.state.forge
    topic = click_topic(slug)

    async def gen():
        sub = await forge.pubsub_subscribe(topic)
        try:
            async for payload in sub:
                data = payload.decode("utf-8") if isinstance(payload, bytes) else payload
                yield f"data: {data}\n\n"
        finally:
            try:
                await sub.aclose()
            except Exception:  # noqa: BLE001
                pass

    return StreamingResponse(gen(), media_type="text/event-stream")


# Registered last so it never shadows /api/... or /healthz.
@api.get("/{slug}")
async def redirect_slug(request: Request, slug: str) -> Response:
    if not SLUG_RE.match(slug) or slug in RESERVED_SLUGS:
        raise HTTPException(status_code=404, detail="link not found")

    forge = request.app.state.forge

    raw_link = await forge.kv_get(link_slug_key(slug))
    if raw_link is None:
        raise HTTPException(status_code=404, detail="link not found")

    link = json.loads(raw_link)
    expires_at = link.get("expiresAt")
    if expires_at is not None:
        exp_dt = datetime.fromisoformat(expires_at.replace("Z", "+00:00"))
        if exp_dt <= datetime.now(UTC):
            raise HTTPException(status_code=404, detail="link not found")

    rl = await forge.rate_limit_check("redirect", slug, 600, 60.0, True)
    if not rl.allowed:
        raise HTTPException(status_code=429, detail="too many requests")

    await forge.kv_incr(clicks_key(slug), 1)
    await forge.queue_enqueue(
        CLICKS_QUEUE,
        json.dumps({"slug": slug}, separators=(",", ":")),
        3,    # max_attempts
        None, # no dedup, every click counts
        None, # no delay
    )

    return RedirectResponse(url=link["url"], status_code=302)

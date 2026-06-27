from __future__ import annotations

import os
import re
from datetime import UTC, datetime

from fastapi import HTTPException

from .types import Credentials, UserRecord

SESSION_IDLE_SECS = 30 * 60
SESSION_ABSOLUTE_SECS = 7 * 24 * 60 * 60
CLICKS_QUEUE = "clicks"
EXPIRE_QUEUE = "link-expire"
DEFAULT_MAX_LINKS = 100

SLUG_RE = re.compile(r"^[A-Za-z0-9_-]{3,32}$")
RESERVED_SLUGS: frozenset[str] = frozenset({"api", "healthz", "favicon.ico"})


def env(key: str, default: str) -> str:
    return os.environ.get(key, default)


def now_iso() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def click_topic(slug: str) -> str:
    return "clicks:" + slug


def user_email_key(email: str) -> str:
    return f"link:user:email:{email}"


def user_id_key(user_id: str) -> str:
    return f"link:user:id:{user_id}"


def link_slug_key(slug: str) -> str:
    return f"link:slug:{slug}"


def owner_key(user_id: str) -> str:
    return f"link:owner:{user_id}"


def clicks_key(slug: str) -> str:
    return f"clicks:{slug}"


def qr_key(slug: str) -> str:
    return f"qr:{slug}"


def validate_credentials(creds: Credentials) -> tuple[str, str]:
    email = creds.email.strip().lower()
    password = creds.password.strip()
    if "@" not in email or len(email) > 254:
        raise HTTPException(status_code=400, detail="enter a valid email")
    if len(password) < 8:
        raise HTTPException(status_code=400, detail="password must be at least 8 characters")
    return email, password


def validate_url(raw: str) -> str:
    url = raw.strip()
    if not (url.startswith("http://") or url.startswith("https://")) or len(url) > 2048:
        raise HTTPException(status_code=400, detail="enter a valid http(s) url")
    return url


def validate_slug(slug: str) -> str:
    if not SLUG_RE.match(slug) or slug in RESERVED_SLUGS:
        raise HTTPException(status_code=400, detail="invalid slug")
    return slug


def bearer_token(authorization: str | None) -> str:
    if not authorization:
        raise HTTPException(status_code=401, detail="authentication required")
    token = authorization.removeprefix("Bearer ").removeprefix("bearer ").strip()
    if not token:
        raise HTTPException(status_code=401, detail="authentication required")
    return token


def public_user(user: UserRecord) -> dict[str, str]:
    return {"id": user.id, "email": user.email}

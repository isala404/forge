from __future__ import annotations

import os
from datetime import UTC, datetime

from fastapi import HTTPException

from .types import Credentials, UserRecord

AUDIT_QUEUE = "todo-audit"
SESSION_IDLE_SECS = 30 * 60
SESSION_ABSOLUTE_SECS = 7 * 24 * 60 * 60


def env(key: str, default: str) -> str:
    return os.environ.get(key, default)


def now_iso() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def validate_credentials(input: Credentials) -> tuple[str, str]:
    email = input.email.strip().lower()
    password = input.password.strip()
    if "@" not in email or len(email) > 254:
        raise HTTPException(status_code=400, detail="enter a valid email")
    if len(password) < 8:
        raise HTTPException(status_code=400, detail="password must be at least 8 characters")
    return email, password


def validate_title(raw: str) -> str:
    title = str(raw or "").strip()
    if len(title) == 0 or len(title) > 160:
        raise HTTPException(status_code=400, detail="title must be 1 to 160 characters")
    return title


def bearer_token(authorization: str | None) -> str:
    if not authorization:
        raise HTTPException(status_code=401, detail="authentication required")
    token = authorization.removeprefix("Bearer ").removeprefix("bearer ").strip()
    if not token:
        raise HTTPException(status_code=401, detail="authentication required")
    return token


def public_user(user: UserRecord) -> dict[str, str]:
    return {"id": user.id, "email": user.email}


def user_email_key(email: str) -> str:
    return f"todo:user:email:{email}"


def user_id_key(user_id: str) -> str:
    return f"todo:user:id:{user_id}"


def todos_key(user_id: str) -> str:
    return f"todo:todos:{user_id}"

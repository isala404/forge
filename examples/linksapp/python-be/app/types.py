from __future__ import annotations

from pydantic import BaseModel, Field


class Credentials(BaseModel):
    email: str
    password: str


class UserRecord(BaseModel):
    id: str
    email: str
    password_hash: str


class LinkRecord(BaseModel):
    slug: str
    url: str
    owner_id: str = Field(alias="ownerId")
    created_at: str = Field(alias="createdAt")
    expires_at: str | None = Field(default=None, alias="expiresAt")


class OwnedLink(BaseModel):
    slug: str
    url: str
    created_at: str = Field(alias="createdAt")
    expires_at: str | None = Field(default=None, alias="expiresAt")


class LinkCreate(BaseModel):
    url: str
    slug: str | None = None
    ttl_seconds: int | None = Field(default=None, alias="ttlSeconds")

    model_config = {"populate_by_name": True}

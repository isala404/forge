from __future__ import annotations

from pydantic import BaseModel, Field


class Credentials(BaseModel):
    email: str
    password: str


class TodoCreate(BaseModel):
    title: str


class TodoPatch(BaseModel):
    title: str | None = None
    completed: bool | None = None


class Todo(BaseModel):
    id: str
    title: str
    completed: bool
    created_at: str = Field(alias="createdAt")
    updated_at: str = Field(alias="updatedAt")


class UserRecord(BaseModel):
    id: str
    email: str
    password_hash: str

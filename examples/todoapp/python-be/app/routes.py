from __future__ import annotations

import json
import uuid
from typing import Annotated, Any

from fastapi import APIRouter, Header, HTTPException, Request, Response
from fastapi.responses import PlainTextResponse

from .types import Credentials, Todo, TodoCreate, TodoPatch, UserRecord
from .utils import (
    AUDIT_QUEUE,
    SESSION_ABSOLUTE_SECS,
    SESSION_IDLE_SECS,
    bearer_token,
    now_iso,
    public_user,
    todos_key,
    user_email_key,
    user_id_key,
    validate_credentials,
    validate_title,
)

api = APIRouter()


@api.get("/healthz", response_class=PlainTextResponse)
async def healthz() -> str:
    return "ok"


@api.get("/api/meta")
async def meta(request: Request) -> dict[str, Any]:
    forge = request.app.state.forge
    depth = await forge.queue_depth(AUDIT_QUEUE)
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
        "auditDepth": {
            "visible": depth.visible,
            "inFlight": depth.in_flight,
            "delayed": depth.delayed,
        },
    }


@api.post("/api/signup", status_code=201)
async def signup(request: Request, input: Credentials) -> dict[str, Any]:
    forge = request.app.state.forge
    email, password = validate_credentials(input)

    auth_limit = await forge.rate_limit_check("todo-auth", email, 20, 60.0, True)
    if not auth_limit.allowed:
        raise HTTPException(status_code=429, detail="too many auth attempts; try again soon")

    user = UserRecord(
        id=str(uuid.uuid4()),
        email=email,
        password_hash=await forge.hash_password(password),
    )

    inserted = await forge.kv_set(
        user_email_key(email),
        user.model_dump_json(),
        None,
        True,
    )
    if not inserted:
        raise HTTPException(status_code=409, detail="email already registered")

    await forge.kv_set(user_id_key(user.id), user.model_dump_json())

    token = await forge.create_session(
        user.id,
        float(SESSION_IDLE_SECS),
        float(SESSION_ABSOLUTE_SECS),
    )
    return {"token": token, "user": public_user(user)}


@api.post("/api/login")
async def login(request: Request, input: Credentials) -> dict[str, Any]:
    forge = request.app.state.forge
    email, password = validate_credentials(input)

    auth_limit = await forge.rate_limit_check("todo-auth", email, 20, 60.0, True)
    if not auth_limit.allowed:
        raise HTTPException(status_code=429, detail="too many auth attempts; try again soon")

    raw_user = await forge.kv_get(user_email_key(email))
    if raw_user is None:
        raise HTTPException(status_code=401, detail="invalid email or password")
    user = UserRecord.model_validate(json.loads(raw_user))

    if not await forge.verify_password(password, user.password_hash):
        raise HTTPException(status_code=401, detail="invalid email or password")

    token = await forge.create_session(
        user.id,
        float(SESSION_IDLE_SECS),
        float(SESSION_ABSOLUTE_SECS),
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

    user = UserRecord.model_validate(json.loads(raw_user))
    return {"user": public_user(user)}


@api.get("/api/todos")
async def list_todos(
    request: Request,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> dict[str, Any]:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_todos = await forge.kv_get(todos_key(user_id))
    todos = [Todo.model_validate(item) for item in json.loads(raw_todos or "[]")]
    return {"todos": [todo.model_dump(by_alias=True) for todo in todos]}


@api.post("/api/todos", status_code=201)
async def create_todo(
    request: Request,
    input: TodoCreate,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> dict[str, Any]:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_todos = await forge.kv_get(todos_key(user_id))
    todos = [Todo.model_validate(item) for item in json.loads(raw_todos or "[]")]

    now = now_iso()
    todo = Todo(
        id=str(uuid.uuid4()),
        title=validate_title(input.title),
        completed=False,
        createdAt=now,
        updatedAt=now,
    )
    todos.insert(0, todo)

    await forge.kv_set(
        todos_key(user_id),
        json.dumps([item.model_dump(by_alias=True) for item in todos], separators=(",", ":")),
    )
    await forge.queue_enqueue(
        AUDIT_QUEUE,
        json.dumps(
            {"userId": user_id, "action": "created", "todoId": todo.id, "at": now_iso()},
            separators=(",", ":"),
        ),
        3,
        f"created:{todo.id}",
        None,
    )

    return todo.model_dump(by_alias=True)


@api.patch("/api/todos/{todo_id}")
async def update_todo(
    request: Request,
    todo_id: str,
    input: TodoPatch,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> dict[str, Any]:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_todos = await forge.kv_get(todos_key(user_id))
    todos = [Todo.model_validate(item) for item in json.loads(raw_todos or "[]")]

    todo = next((item for item in todos if item.id == todo_id), None)
    if todo is None:
        raise HTTPException(status_code=404, detail="todo not found")
    if input.title is not None:
        todo.title = validate_title(input.title)
    if input.completed is not None:
        todo.completed = bool(input.completed)
    todo.updated_at = now_iso()

    await forge.kv_set(
        todos_key(user_id),
        json.dumps([item.model_dump(by_alias=True) for item in todos], separators=(",", ":")),
    )
    await forge.queue_enqueue(
        AUDIT_QUEUE,
        json.dumps(
            {"userId": user_id, "action": "updated", "todoId": todo.id, "at": now_iso()},
            separators=(",", ":"),
        ),
        3,
        f"updated:{todo.id}",
        None,
    )

    return todo.model_dump(by_alias=True)


@api.delete("/api/todos/{todo_id}", status_code=204)
async def delete_todo(
    request: Request,
    todo_id: str,
    authorization: Annotated[str | None, Header(alias="Authorization")] = None,
) -> Response:
    forge = request.app.state.forge
    user_id = await forge.validate_session(bearer_token(authorization))
    if user_id is None:
        raise HTTPException(status_code=401, detail="authentication required")

    raw_todos = await forge.kv_get(todos_key(user_id))
    todos = [Todo.model_validate(item) for item in json.loads(raw_todos or "[]")]
    next_todos = [todo for todo in todos if todo.id != todo_id]
    if len(next_todos) == len(todos):
        raise HTTPException(status_code=404, detail="todo not found")

    await forge.kv_set(
        todos_key(user_id),
        json.dumps(
            [item.model_dump(by_alias=True) for item in next_todos],
            separators=(",", ":"),
        ),
    )
    await forge.queue_enqueue(
        AUDIT_QUEUE,
        json.dumps(
            {"userId": user_id, "action": "deleted", "todoId": todo_id, "at": now_iso()},
            separators=(",", ":"),
        ),
        3,
        f"deleted:{todo_id}",
        None,
    )

    return Response(status_code=204)

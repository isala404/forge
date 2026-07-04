from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from typing import Any

import forgelib
from fastapi import FastAPI, HTTPException, Request
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse

from .routes import api
from .utils import env


async def maintenance(forge: Any, stop: asyncio.Event) -> None:
    while not stop.is_set():
        try:
            await forge.run_scheduler_once()
            await forge.maintain()
        except Exception as exc:  # noqa: BLE001 - background task should keep running.
            print(f"maintenance sweep failed: {exc}", flush=True)
        try:
            await asyncio.wait_for(stop.wait(), timeout=30.0)
        except TimeoutError:
            pass


def build_app() -> FastAPI:
    @asynccontextmanager
    async def lifespan(app: FastAPI):
        # Forge instantiates from ./forge.toml: FORGE_POSTGRES_URL when set,
        # else an embedded Postgres.
        forge = await forgelib.ForgeClient.init()
        stop = asyncio.Event()
        task = asyncio.create_task(maintenance(forge, stop))
        app.state.forge = forge
        try:
            yield
        finally:
            stop.set()
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)

    app = FastAPI(lifespan=lifespan)
    app.add_middleware(
        CORSMiddleware,
        allow_origins=[env("CORS_ORIGIN", "*")],
        allow_methods=["*"],
        allow_headers=["*"],
    )

    @app.exception_handler(HTTPException)
    async def http_error(_request: Request, exc: HTTPException) -> JSONResponse:
        return JSONResponse({"error": str(exc.detail)}, status_code=exc.status_code)

    @app.exception_handler(RequestValidationError)
    async def validation_error(_request: Request, _exc: RequestValidationError) -> JSONResponse:
        return JSONResponse({"error": "invalid request"}, status_code=422)

    app.include_router(api)
    return app


app = build_app()

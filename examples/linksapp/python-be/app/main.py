from __future__ import annotations

import asyncio
import os
from contextlib import asynccontextmanager

import forge_py
from fastapi import FastAPI, HTTPException, Request
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse

from .routes import api
from .utils import env
from .worker import clicks_worker, expire_worker, scheduler_loop


def build_app() -> FastAPI:
    @asynccontextmanager
    async def lifespan(app: FastAPI):
        os.environ.setdefault(
            "FORGE_POSTGRES_URL",
            "postgres://postgres:forge@127.0.0.1:5432/linksapp_python",
        )
        os.environ.setdefault("FORGE_BLOB_SIGNING_SECRET", "dev-secret-change-me")

        forge = await forge_py.ForgeClient.connect_from_env()
        stop = asyncio.Event()
        tasks = [
            asyncio.create_task(clicks_worker(forge, stop)),
            asyncio.create_task(expire_worker(forge, stop)),
            asyncio.create_task(scheduler_loop(forge, stop)),
        ]
        app.state.forge = forge
        try:
            yield
        finally:
            stop.set()
            for t in tasks:
                t.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)

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

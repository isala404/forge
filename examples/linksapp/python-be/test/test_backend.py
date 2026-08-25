import unittest

import forgelib
from fastapi import FastAPI, HTTPException
from starlette.requests import Request

from app.routes import (
    create_link,
    delete_link_route,
    get_qr,
    healthz,
    link_state,
    list_links,
    login,
    logout,
    me,
    meta,
    redirect_slug,
    signup,
)
from app.types import Credentials, LinkCreate
from app.utils import CLICKS_QUEUE, validate_slug, validate_url
from app.worker import delete_link

MEMORY = '[forge]\nmode = "memory"\nenvironment = "test"\n'


class BackendTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.forge = await forgelib.ForgeClient.init_from_string(MEMORY)
        app = FastAPI()
        app.state.forge = self.forge
        self.request = Request(
            {
                "type": "http",
                "method": "GET",
                "path": "/",
                "headers": [],
                "app": app,
            }
        )

    async def asyncTearDown(self) -> None:
        await self.forge.close(1.0)

    async def create_user(self, email: str) -> tuple[str, str]:
        result = await signup(self.request, Credentials(email=email, password="correct horse"))
        return result["token"], result["user"]["id"]

    async def test_health_meta_and_input_limits(self) -> None:
        self.assertEqual(await healthz(), "ok")
        report = await meta(self.request)
        self.assertEqual(report["backend"], "python")
        self.assertEqual(len(report["forge"]), 8)

        with self.assertRaises(HTTPException) as invalid_url:
            validate_url("ftp://example.com")
        self.assertEqual(invalid_url.exception.status_code, 400)

        with self.assertRaises(HTTPException) as invalid_slug:
            validate_slug("api")
        self.assertEqual(invalid_slug.exception.status_code, 400)

        with self.assertRaises(HTTPException) as invalid_credentials:
            await signup(self.request, Credentials(email="bad", password="short"))
        self.assertEqual(invalid_credentials.exception.status_code, 400)

    async def test_auth_links_isolation_redirect_and_idempotent_delete(self) -> None:
        first_token, _ = await self.create_user("first@example.com")
        second_token, _ = await self.create_user("second@example.com")

        with self.assertRaises(HTTPException) as duplicate:
            await signup(
                self.request,
                Credentials(email="FIRST@example.com", password="correct horse"),
            )
        self.assertEqual(duplicate.exception.status_code, 409)

        with self.assertRaises(HTTPException) as bad_login:
            await login(
                self.request,
                Credentials(email="first@example.com", password="wrong password"),
            )
        self.assertEqual(bad_login.exception.status_code, 401)

        await self.forge.set_flag_on("custom_slugs")
        created = await create_link(
            self.request,
            LinkCreate(url="https://example.com/path", slug="first-link"),
            f"Bearer {first_token}",
        )
        self.assertEqual(created["slug"], "first-link")

        second_links = await list_links(self.request, f"Bearer {second_token}")
        self.assertEqual(second_links, {"links": []})
        first_links = await list_links(self.request, f"Bearer {first_token}")
        self.assertEqual(len(first_links["links"]), 1)

        qr = await get_qr(self.request, "first-link")
        self.assertEqual(qr.media_type, "image/svg+xml")

        redirect = await redirect_slug(self.request, "first-link")
        self.assertEqual(redirect.status_code, 302)
        self.assertEqual(redirect.headers["location"], "https://example.com/path")
        self.assertEqual(await link_state(self.request, "first-link"), {"clicks": 1})
        depth = await self.forge.queue_depth(CLICKS_QUEUE)
        self.assertEqual(depth.visible, 1)

        deleted = await delete_link_route(
            self.request,
            "first-link",
            f"Bearer {first_token}",
        )
        self.assertEqual(deleted.status_code, 204)
        await delete_link(self.forge, "first-link")
        with self.assertRaises(HTTPException) as missing:
            await link_state(self.request, "first-link")
        self.assertEqual(missing.exception.status_code, 404)

        logged_out = await logout(self.request, f"Bearer {first_token}")
        self.assertEqual(logged_out.status_code, 204)
        with self.assertRaises(HTTPException) as unauthenticated:
            await me(self.request, f"Bearer {first_token}")
        self.assertEqual(unauthenticated.exception.status_code, 401)


if __name__ == "__main__":
    unittest.main()

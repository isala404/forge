from __future__ import annotations

import asyncio
import hashlib
import json
import os
import sys
import tempfile
import time
import urllib.request

from forgelib import ForgeClient


def required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"{name} is required")
    return value


def quoted(value: str) -> str:
    return json.dumps(value)


def request(ticket, body: bytes | None = None) -> bytes:
    req = urllib.request.Request(
        ticket.url,
        data=body,
        method=ticket.method,
        headers=dict(ticket.required_headers),
    )
    with urllib.request.urlopen(req, timeout=10) as response:
        return response.read()


async def main() -> None:
    namespace = f"s3_python_{time.time_ns()}"
    config = f"""
[forge]
mode = "postgres"
environment = "test"
namespace = {quoted(namespace)}

[postgres]
url = {quoted(required("TEST_DATABASE_URL"))}
auto_migrate = true

[blob]
backend = "s3"
bucket = {quoted(required("S3_TEST_BUCKET"))}
region = {quoted(os.environ.get("S3_TEST_REGION", "us-east-1"))}
endpoint = {quoted(required("S3_TEST_ENDPOINT"))}
prefix = "binding-smoke"
access_key = {quoted(required("S3_TEST_ACCESS_KEY"))}
secret_key = {quoted(required("S3_TEST_SECRET_KEY"))}
path_style = true
signing_secret = "binding-proxy-secret"
"""
    forge = await ForgeClient.init_from_string(config)
    path = ""
    try:
        with tempfile.NamedTemporaryFile(prefix="forge-s3-", suffix=".bin", delete=False) as file:
            path = file.name
            file.truncate(51 * 1024 * 1024)
        await forge.blob_put_file("streamed.bin", path, "application/octet-stream", None, True)
        info = await forge.blob_head("streamed.bin")
        if info is None or info.size != 51 * 1024 * 1024:
            raise RuntimeError("streamed upload size mismatch")

        checksum = hashlib.sha256(b"hello").hexdigest()
        await forge.blob_put_object(
            "source.txt",
            b"hello",
            content_type="text/plain",
            metadata={"owner": "python"},
            create_only=True,
            cache_control="public, max-age=60",
            content_disposition="attachment; filename=source.txt",
            checksum_sha256=checksum,
        )
        source = await forge.blob_head("source.txt")
        if source is None or source.checksum_sha256 != checksum:
            raise RuntimeError("checksum metadata mismatch")
        if (await forge.blob_get_if("source.txt", if_match=source.etag)).state != "found":
            raise RuntimeError("conditional GET mismatch")
        if (await forge.blob_get_if("source.txt", if_none_match=source.etag)).state != "not_modified":
            raise RuntimeError("not-modified GET mismatch")
        copied = await forge.blob_copy("source.txt", "copy.txt")
        if copied.cache_control != "public, max-age=60" or not await forge.blob_verify_checksum_sha256("copy.txt", checksum):
            raise RuntimeError("copy metadata or checksum mismatch")

        upload = await forge.blob_create_multipart(
            "handled.bin",
            content_type="application/octet-stream",
            create_only=True,
            cache_control="private, max-age=0",
        )
        first = await forge.blob_upload_part(upload, 1, b"3" * (5 * 1024 * 1024))
        second = await forge.blob_upload_part(upload, 2, b"tail")
        completed = await forge.blob_complete_multipart(upload, [first, second])
        if completed.size != 5 * 1024 * 1024 + 4:
            raise RuntimeError("multipart handle size mismatch")
        abandoned = await forge.blob_create_multipart("abandoned.bin")
        await forge.blob_abort_multipart(abandoned)
        await forge.blob_abort_multipart(abandoned)

        encrypted = await forge.blob_presign_native_put(
            "encrypted.txt", 60, "text/plain", sse_algorithm="AES256"
        )
        if not any(
            name.lower() == "x-amz-server-side-encryption" and value == "AES256"
            for name, value in encrypted.required_headers.items()
        ):
            raise RuntimeError("S3 encryption header was not signed")
        put = await forge.blob_presign_native_put("native.txt", 60, "text/plain")
        await asyncio.to_thread(request, put, b"python-native")
        get = await forge.blob_presign_native_get("native.txt", 60)
        if await asyncio.to_thread(request, get) != b"python-native":
            raise RuntimeError("native GET mismatch")

        proxy = await forge.blob_presign_upload("proxy.txt", 60, 1024)
        if not await forge.blob_verify_presign(
            proxy.method,
            proxy.key,
            proxy.expires_epoch,
            proxy.max_bytes,
            proxy.signature,
        ):
            raise RuntimeError("proxy ticket did not verify")
        if await forge.blob_delete("native.txt") is not None:
            raise RuntimeError("blob_delete must return None")
    finally:
        await forge.close()
        if path:
            os.unlink(path)
    print("PASS  s3/python_package_object_ergonomics")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except Exception as error:
        print(f"Python S3 smoke failed: {type(error).__name__}", file=sys.stderr)
        raise SystemExit(1)

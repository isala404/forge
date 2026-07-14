import asyncio
import sys
import types
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Optional


# The convenience layer imports the compiled extension. Worker tests need only the
# Python helper, so provide the minimum extension surface instead of building native
# code for this unit test.
native = types.ModuleType("forgelib.forgelib")


class ForgeError(Exception):
    pass


class ForgeClient:
    pass


native.ForgeError = ForgeError
native.ForgeClient = ForgeClient
native.__all__ = ["ForgeError", "ForgeClient"]
sys.modules["forgelib.forgelib"] = native
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from forgelib import run_worker  # noqa: E402


class WorkerShutdownTests(unittest.IsolatedAsyncioTestCase):
    async def test_releases_job_returned_after_stop_during_long_poll(self) -> None:
        stop = asyncio.Event()
        handled: list[str] = []
        nacked: list[tuple[str, Optional[float]]] = []
        raw = SimpleNamespace(
            id="j1",
            receipt="r1",
            payload='{"n": 1}',
            attempt=1,
            max_attempts=5,
            leased_until_ms=0.0,
            queue="q",
        )

        class Client:
            async def queue_dequeue(self, *_args):
                stop.set()
                return raw

            async def queue_nack(self, receipt, retry_seconds=None):
                nacked.append((receipt, retry_seconds))

        async def handler(_job):
            handled.append("called")

        await run_worker(Client(), "q", handler, stop=stop)

        self.assertEqual(handled, [])
        self.assertEqual(nacked, [("r1", 0.0)])


if __name__ == "__main__":
    unittest.main()

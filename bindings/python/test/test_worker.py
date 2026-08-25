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
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

class ForgeError(Exception):
    pass


class ForgeClient:
    async def close(self, _timeout_seconds: float = 30.0) -> None:
        import forgelib

        await forgelib._close_managed_tasks(self, _timeout_seconds)


if "forgelib" not in sys.modules:
    native = types.ModuleType("forgelib.forgelib")
    native.ForgeError = ForgeError
    native.ForgeClient = ForgeClient
    native.__all__ = ["ForgeError", "ForgeClient"]
    sys.modules["forgelib.forgelib"] = native

from forgelib import (  # noqa: E402
    decode_invalidation_event,
    encode_invalidation_event,
    run_worker,
)


class InvalidationTests(unittest.TestCase):
    def test_round_trip_discards_unknown_fields(self) -> None:
        decoded = decode_invalidation_event(
            '{"schema_version":1,"tags":["links"],"query_keys":[["link",{"owner":"u1"}]],"revision":"42","future":true}'
        )
        self.assertEqual(decoded["tags"], ["links"])
        self.assertNotIn(b"future", encode_invalidation_event(decoded))

    def test_rejects_unbounded_or_empty_events(self) -> None:
        with self.assertRaisesRegex(ValueError, "target"):
            encode_invalidation_event({"schema_version": 1, "tags": [], "query_keys": []})
        with self.assertRaisesRegex(ValueError, "unique"):
            encode_invalidation_event({"schema_version": 1, "tags": ["x", "x"]})
        with self.assertRaisesRegex(ValueError, "4096"):
            decode_invalidation_event("x" * 4097)


class WorkerShutdownTests(unittest.IsolatedAsyncioTestCase):
    async def test_worker_stops_on_non_retryable_dequeue_error(self) -> None:
        calls = 0

        class Client:
            async def queue_dequeue(self, *_args):
                nonlocal calls
                calls += 1
                error = ForgeError("invalid queue")
                error.retryable = False
                raise error

        with self.assertRaises(ForgeError):
            await run_worker(Client(), "q", lambda _job: None)
        self.assertEqual(calls, 1)

    async def test_worker_honors_bounded_concurrency(self) -> None:
        stop = asyncio.Event()
        release = asyncio.Event()
        all_started = asyncio.Event()
        active = 0
        peak = 0
        jobs = [
            SimpleNamespace(
                id=f"j{index}", receipt=f"r{index}", payload='{"n": 1}',
                attempt=1, max_attempts=5, leased_until_ms=0.0, queue="q",
            )
            for index in range(3)
        ]
        acked: list[str] = []

        class Client:
            async def queue_dequeue(self, *_args):
                if jobs:
                    return jobs.pop()
                await stop.wait()
                return None

            async def queue_heartbeat(self, _receipt):
                pass

            async def queue_ack(self, receipt):
                acked.append(receipt)

        async def handler(_job):
            nonlocal active, peak
            active += 1
            peak = max(peak, active)
            if active == 3:
                all_started.set()
            await release.wait()
            active -= 1

        worker = asyncio.create_task(
            run_worker(
                Client(), "q", handler, stop=stop, concurrency=3,
                visibility_seconds=1, heartbeat_seconds=0.1,
            )
        )
        await asyncio.wait_for(all_started.wait(), timeout=1)
        self.assertEqual(peak, 3)
        release.set()
        stop.set()
        await asyncio.wait_for(worker, timeout=1)
        self.assertEqual(len(acked), 3)

    async def test_lease_loss_cancels_handler_without_settling(self) -> None:
        stop = asyncio.Event()
        cancelled = asyncio.Event()
        settled: list[str] = []
        diagnostics: list[tuple[str, str]] = []
        raw = SimpleNamespace(
            id="j1", receipt="r1", payload='{"n": 1}', attempt=1,
            max_attempts=5, leased_until_ms=0.0, queue="q",
        )

        class Client:
            sent = False

            async def queue_dequeue(self, *_args):
                if not self.sent:
                    self.sent = True
                    return raw
                await stop.wait()
                return None

            async def queue_heartbeat(self, _receipt):
                raise ForgeError("PRECONDITION: lease lost")

            async def queue_ack(self, _receipt):
                settled.append("ack")

            async def queue_nack(self, *_args):
                settled.append("nack")

        async def handler(job):
            try:
                await asyncio.Future()
            finally:
                if job.cancelled is not None and job.cancelled.is_set():
                    cancelled.set()
                    stop.set()

        async def on_error(exc, _job):
            diagnostics.append((exc.worker_identity, exc.worker_state))

        await asyncio.wait_for(
            run_worker(
                Client(), "q", handler, stop=stop,
                visibility_seconds=0.03, heartbeat_seconds=0.01,
                identity="lease-test", on_error=on_error,
            ),
            timeout=1,
        )
        self.assertTrue(cancelled.is_set())
        self.assertEqual(settled, [])
        self.assertEqual(diagnostics, [("lease-test", "heartbeating")])

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

            async def queue_nack(self, receipt, retry_seconds=None, failure_summary=None):
                nacked.append((receipt, retry_seconds))

        async def handler(_job):
            handled.append("called")

        await run_worker(Client(), "q", handler, stop=stop)

        self.assertEqual(handled, [])
        self.assertEqual(nacked, [("r1", 0.0)])

    async def test_client_close_cancels_handler_and_releases_lease(self) -> None:
        started = asyncio.Event()
        cancelled = asyncio.Event()
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

        class Client(ForgeClient):
            def __init__(self) -> None:
                self.sent = False

            async def queue_dequeue(self, *_args):
                if self.sent:
                    await asyncio.Future()
                self.sent = True
                return raw

            async def queue_nack(self, receipt, retry_seconds=None, failure_summary=None):
                nacked.append((receipt, retry_seconds))

            async def queue_heartbeat(self, _receipt):
                pass

        client = Client()

        async def handler(job):
            started.set()
            assert job.cancelled is not None
            try:
                await asyncio.Future()
            finally:
                if job.cancelled.is_set():
                    cancelled.set()

        worker = asyncio.create_task(run_worker(client, "q", handler))
        await asyncio.wait_for(started.wait(), timeout=1)
        await client.close(1)
        await asyncio.wait_for(worker, timeout=1)

        self.assertTrue(cancelled.is_set())
        self.assertEqual(nacked, [("r1", 0.0)])

    async def test_cancelling_worker_task_releases_active_lease(self) -> None:
        started = asyncio.Event()
        cancelled = asyncio.Event()
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
                return raw

            async def queue_nack(self, receipt, retry_seconds=None, failure_summary=None):
                nacked.append((receipt, retry_seconds))

            async def queue_heartbeat(self, _receipt):
                pass

        async def handler(job):
            started.set()
            try:
                await asyncio.Future()
            finally:
                assert job.cancelled is not None and job.cancelled.is_set()
                cancelled.set()

        worker = asyncio.create_task(run_worker(Client(), "q", handler))
        await asyncio.wait_for(started.wait(), timeout=1)
        worker.cancel()
        await asyncio.gather(worker, return_exceptions=True)

        self.assertTrue(cancelled.is_set())
        self.assertEqual(nacked, [("r1", 0.0)])


if __name__ == "__main__":
    unittest.main()

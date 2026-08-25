"""Smoke the deterministic APIs from an installed native wheel."""

import asyncio
import hashlib

import forgelib


CONFIG = '[forge]\nmode = "memory"\nenvironment = "test"\n'


async def main() -> None:
    first = await forgelib.ForgeClient.init_memory_for_testing(
        CONFIG, 1_700_000_000_000, 42
    )
    second = await forgelib.ForgeClient.init_memory_for_testing(
        CONFIG, 1_700_000_000_000, 42
    )
    assert first.is_live()
    assert len(first.backend_capabilities()) == 8
    assert first.pubsub_channel("updates") == first.pubsub_channel("updates")
    assert await first.kv_set("ttl", "value", 10.0)
    assert await first.kv_set("other", "second")
    assert await first.kv_mget(["other", "missing", "ttl"]) == [
        "second",
        None,
        "value",
    ]
    assert await first.kv_expire("other", 1.0)
    first_token = await first.create_token("user", "test", 60.0)
    second_token = await second.create_token("user", "test", 60.0)
    assert first_token == second_token
    first.advance_test_clock(1.0)
    await first.maintain()
    assert await first.kv_get("other") is None
    first.advance_test_clock(9.0)
    assert await first.kv_get("ttl") is None

    session_one = await first.create_session("owner")
    session_two = await first.create_session("owner")
    assert await first.revoke_all_sessions("owner") == 2
    assert await first.validate_session(session_one) is None
    assert await first.validate_session(session_two) is None
    api_key = await first.create_api_key("owner", "test")
    assert await first.revoke_api_key(api_key.id)
    assert await first.verify_api_key(api_key.secret) is None

    await first.config_set("color", "blue")
    await first.set_flag_value("theme", '"dark"', "theme-v1")
    values = await first.config_get_many(["missing", "color", "color"])
    assert [entry.value for entry in values] == [None, "blue", "blue"]
    requests = [
        forgelib.FlagEvaluationRequest(
            "theme-user", "theme", '"light"', "user-1", '{"tenant":"acme"}'
        )
    ]
    details = await first.flag_details_many(requests)
    assert details[0].evaluation.value_json == '"dark"'
    assert details[0].evaluation.variant == "theme-v1"
    snapshot = await first.config_snapshot(["color"], requests, 60, "no_secrets")
    snapshot = forgelib.decode_config_snapshot(forgelib.encode_config_snapshot(snapshot))
    assert forgelib.config_snapshot_get(snapshot, "color", snapshot.created_at_ms) == "blue"
    assert forgelib.config_snapshot_flag_details(snapshot, "theme-user", snapshot.created_at_ms).value_json == '"dark"'
    try:
        forgelib.config_snapshot_get(snapshot, "color", snapshot.expires_at_ms + 1)
    except ValueError:
        pass
    else:
        raise AssertionError("stale snapshot was accepted")

    await first.schedule_cron(
        "minute", "* * * * *", "scheduled", "x", None, "catch_up", 3
    )
    assert await first.schedule_pause("minute")
    first.advance_test_clock(20 * 60)
    assert (await first.scheduler_diagnostics()).due_count == 0
    schedule = await first.schedule_inspect("minute")
    assert schedule is not None and schedule.paused
    assert schedule.misfire_policy == "catch_up" and schedule.max_catch_up == 3
    assert await first.schedule_resume("minute")
    assert await first.run_scheduler_once() == 3
    scheduler = await first.scheduler_diagnostics()
    assert scheduler.due_count == 0 and scheduler.last_successful_tick_ms is not None

    queue = first.queue("long", loads=forgelib.bytes_loads, dumps=forgelib.bytes_dumps)
    first_id = await queue.enqueue(
        b"first", priority="high", concurrency_key="tenant-a"
    )
    await queue.enqueue(b"blocked", priority="high", concurrency_key="tenant-a")
    other_id = await queue.enqueue(b"other", concurrency_key="tenant-b")
    first_job = await queue.dequeue(wait_seconds=0, concurrency_limit_per_key=1)
    other_job = await queue.dequeue(wait_seconds=0, concurrency_limit_per_key=1)
    assert first_job is not None and first_job.id == first_id
    assert other_job is not None and other_job.id == other_id
    assert (await queue.cancel(first_id))["state"] == "cancel_requested"
    assert await first.queue_cancellation_requested(first_job.receipt)
    await first.queue_finish_cancellation(first_job.receipt)
    await queue.ack(other_job.receipt)
    assert (await queue.status(first_id))["state"] == "cancelled"

    operator = first.queue(
        "operator-batch", loads=forgelib.bytes_loads, dumps=forgelib.bytes_dumps
    )
    batch = await operator.enqueue_batch(
        [
            (b"one", "11111111-1111-4111-8111-111111111111"),
            (b"two", None),
        ]
    )
    assert batch[0].job_id == "11111111-1111-4111-8111-111111111111"
    await operator.pause()
    assert await operator.is_paused()
    assert await operator.dequeue(wait_seconds=0) is None
    await operator.resume()
    jobs = await operator.dequeue_batch(10, wait_seconds=0)
    assert len(jobs) == 2
    for job in jobs:
        await operator.ack(job.receipt)
    stats = await operator.stats()
    assert stats.enqueued_total == 2 and stats.settled_total == 2

    dead = first.queue(
        "batch-redrive", loads=forgelib.bytes_loads, dumps=forgelib.bytes_dumps
    )
    for payload in (b"one", b"two"):
        await dead.enqueue(payload, max_attempts=1)
        job = await dead.dequeue(wait_seconds=0)
        assert job is not None
        await dead.nack(job.receipt, retry_seconds=0, failure_summary="safe")
    statuses = await first.queue("batch-redrive.dlq").statuses(limit=10)
    assert len(statuses["items"]) == 2
    assert all(item["state"] == "queued" for item in statuses["items"])
    redriven = await dead.redrive_batch(
        destination="recovered", dedup_policy="clear", limit=10
    )
    assert redriven.redriven == 2
    assert (await first.queue("recovered").depth()).visible == 2

    try:
        await first.run_outbox_once()
    except forgelib.ForgeError as error:
        assert error.code == "NOT_CONFIGURED"
    else:
        raise AssertionError("memory outbox relay was accepted")

    diagnostics = await first.diagnostics(1.0)
    assert diagnostics.ready
    assert any(check.name == "backend_reachability" for check in diagnostics.checks)
    assert (await first.probe(1.0)).ready
    assert any(metric.name == "forge_operations_total" for metric in first.metrics_snapshot())
    assert "forge_operations_total" in first.render_prometheus()

    body = b"hello"
    checksum = hashlib.sha256(body).hexdigest()
    await first.blob_put_object(
        "source",
        body,
        content_type="text/plain",
        metadata={"purpose": "test"},
        cache_control="public, max-age=60",
        content_disposition='attachment; filename="hello.txt"',
        checksum_sha256=checksum,
    )
    info = await first.blob_head("source")
    assert info is not None and info.checksum_sha256 == checksum
    found = await first.blob_get_if("source", if_match=info.etag)
    assert found.state == "found" and found.body == body
    not_modified = await first.blob_get_if("source", if_none_match=info.etag)
    assert not_modified.state == "not_modified" and not_modified.body is None
    copied = await first.blob_copy("source", "copy")
    assert copied.cache_control == "public, max-age=60"
    assert copied.content_disposition == 'attachment; filename="hello.txt"'
    assert await first.blob_verify_checksum_sha256("copy", checksum)
    try:
        await first.blob_create_multipart("large")
    except forgelib.ForgeError as error:
        assert error.code == "NOT_CONFIGURED"
    else:
        raise AssertionError("memory multipart upload was accepted")

    encoded = forgelib.encode_queue_envelope(
        {
            "schema": "example.task.v1",
            "content_type": "application/octet-stream",
            "body": b"\x00\xff",
            "artifacts": [{"uri": "blob://generated/result"}],
        }
    )
    assert forgelib.decode_queue_envelope(encoded)["body"] == b"\x00\xff"
    try:
        forgelib.encode_queue_envelope(
            {
                "schema": "v1",
                "content_type": "application/octet-stream",
                "body": b"",
                "artifacts": [{"uri": ""}],
            }
        )
    except ValueError:
        pass
    else:
        raise AssertionError("empty artifact URI was accepted")

    budget = first.rate_limit("tokens", "tenant")
    reservation = await budget.reserve(
        max=10, per_seconds=3600, cost=5, ttl_seconds=60
    )
    assert reservation is not None
    committed = await budget.commit(reservation["id"], 2)
    assert committed["committed_units"] == 2
    assert await budget.commit(reservation["id"], 2) == committed
    decision = await budget.check(max=10, per_seconds=3600, cost=8)
    assert decision.allowed and decision.remaining == 0
    await first.close(1.0)
    assert not first.is_live()
    await second.close(1.0)


if __name__ == "__main__":
    asyncio.run(main())

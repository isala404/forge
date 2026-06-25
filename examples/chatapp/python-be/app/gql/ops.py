from __future__ import annotations

import forge_py
import strawberry
from strawberry.types import Info

from .helpers import FAIL_QUEUE, current_user, map_forge, require_admin, require_user
from .types import OpsStats


@strawberry.type
class OpsQuery:
    @strawberry.field(
        description="Whether the `reactions_v2` feature flag is enabled for the current user"
        " (forge config)."
    )
    async def reactions_enabled(self, info: Info) -> bool:
        u = await current_user(info)
        if u is None:
            return False
        return await info.context["forge"].flag("reactions_v2", False, str(u["id"]))

    @strawberry.field(
        description="Developer-tools gauges (kv scan + DLQ depth) for the settings page."
    )
    async def ops_stats(self, info: Info) -> OpsStats:
        await require_user(info)
        forge = info.context["forge"]
        try:
            online = len(await forge.kv_scan("online:", 1000))
        except forge_py.ForgeError:
            online = 0
        depth = await forge.queue_depth(f"{FAIL_QUEUE}.dlq")
        dlq_count = depth.visible + depth.in_flight + depth.delayed
        return OpsStats(online_count=online, dlq_count=dlq_count)


@strawberry.type
class OpsMutation:
    @strawberry.mutation(
        description="Set the `reactions_v2` feature-flag rollout percentage (forge config)."
    )
    async def set_reactions_rollout(self, info: Info, percent: int) -> bool:
        await require_admin(info)
        pct = max(0, min(100, percent))
        try:
            await info.context["forge"].set_flag_percent("reactions_v2", pct)
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        return True

    @strawberry.mutation(
        description="Enqueue a job destined to dead-letter (forge queue DLQ demo)."
    )
    async def trigger_failing_job(self, info: Info) -> bool:
        await require_admin(info)
        try:
            await info.context["forge"].queue_enqueue(FAIL_QUEUE, "boom")
        except forge_py.ForgeError as e:
            raise map_forge(e) from e
        return True

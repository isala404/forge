import asyncio

import forgelib


async def run() -> int:
    reports = await forgelib.ForgeClient.migrate()
    for report in reports:
        print(f"{report.target}: {report.state} ({report.message})")
    return 0 if all(report.state == "applied" for report in reports) else 1


def main() -> None:
    raise SystemExit(asyncio.run(run()))

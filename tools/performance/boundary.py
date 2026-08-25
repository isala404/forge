#!/usr/bin/env python3
"""Measure the installed Python package boundary with a portable CloudEvent round trip."""

from __future__ import annotations

import argparse
import json
import math
import time
from pathlib import Path

import forgelib


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=1000)
    parser.add_argument("--output")
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("iterations must be positive")
    event = {"id": "benchmark", "source": "urn:forge:performance", "type": "forge.benchmark", "data": b"boundary"}
    samples = []
    for _ in range(args.iterations):
        started = time.perf_counter()
        forgelib.decode_cloud_event(forgelib.encode_cloud_event(event))
        samples.append((time.perf_counter() - started) * 1000)
    samples.sort()
    rank = max(0, math.ceil(len(samples) * 0.95) - 1)
    report = {
        "schema_version": 1,
        "kind": "language_boundary",
        "language": "python",
        "iterations": args.iterations,
        "metrics": [{"name": "cloudevent_roundtrip_p95_ms", "value": samples[rank], "unit": "ms"}],
    }
    encoded = json.dumps(report, indent=2) + "\n"
    if args.output:
        Path(args.output).write_text(encoded)
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()

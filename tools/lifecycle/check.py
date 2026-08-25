#!/usr/bin/env python3
"""Reject process-signal ownership in the Forge library packages."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SEARCH = [ROOT / "src", ROOT / "bindings" / "node", ROOT / "bindings" / "python", ROOT / "bindings" / "go"]
FORBIDDEN = ("tokio::signal", "process.on(\"SIG", "process.on('SIG", "add_signal_handler", "signal.Notify")

problems: list[str] = []
for directory in SEARCH:
    for path in directory.rglob("*"):
        if not path.is_file() or path.suffix not in {".rs", ".js", ".py", ".go"}:
            continue
        if any(part in {"node_modules", "target", "dist", "examples", "test", "tests"} for part in path.parts):
            continue
        text = path.read_text(encoding="utf-8")
        for token in FORBIDDEN:
            if token in text:
                problems.append(f"{path.relative_to(ROOT)} installs or references {token!r}")

if problems:
    raise SystemExit("library signal ownership is forbidden:\n  " + "\n  ".join(problems))

print("lifecycle-check: library packages install no process signal handlers")

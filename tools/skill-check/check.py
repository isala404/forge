#!/usr/bin/env python3
"""Drift guard for the forge-idiomatic-developer skill.

The skill's whole value is that its method names are the *real* ones, so a stale name
is worse than none. This checks every Node/Python method the skill names against the
committed binding surface, and fails (exit 1) on any name that no longer exists.

What counts as a "named method":
  * an inline code token written as `name()` (the reference tables use this form), and
  * a `forge.NAME(` / `client.NAME(` call inside a ts/js or python fenced code block.

Rust is compiler-checked (and uses namespaced accessors like `forge.kv()` that don't
map to a flat method list), so rust.md and rust fences are not machine-checked here.

Sources of truth:
  * Node   -> bindings/node/client.d.ts + client.js + generated index.d.ts
  * Python -> bindings/python/src/lib.rs (the PyO3 #[pymethods]) + package root helpers
    (the generated _generated.pyi only declares data classes, not client methods, so
    the PyO3 source is the real method surface.)
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SKILL_DIR = ROOT / "skills" / "forge-idiomatic-developer"

NODE_SOURCES = [
    ROOT / "bindings" / "node" / "client.d.ts",
    ROOT / "bindings" / "node" / "client.js",
    ROOT / "bindings" / "node" / "index.d.ts",
]
PY_SOURCES = [
    ROOT / "bindings" / "python" / "src" / "lib.rs",
    ROOT / "bindings" / "python" / "python" / "forgelib" / "__init__.py",
]

NODE_FENCE_LANGS = {"ts", "tsx", "typescript", "js", "jsx", "javascript"}
PY_FENCE_LANGS = {"python", "py"}

# `word(` — any identifier at a call/definition site. An over-broad known set is safe:
# it only ever adds unrelated names, never hides one that was removed or renamed.
DECL = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
# PyO3 methods are declared `fn kv_set<'py>(...)`, so the generic hides them from DECL;
# grab every `fn NAME` in the Rust binding directly.
RUST_FN = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
INLINE_METHOD = re.compile(r"`([A-Za-z_][A-Za-z0-9_]*)\(\)`")
CLIENT_CALL = re.compile(r"\b(?:forge|client)\.([A-Za-z_][A-Za-z0-9_]*)\s*\(")
FENCE = re.compile(r"^\s*```(\S*)")


def known_names(sources: list[Path]) -> set[str]:
    names: set[str] = set()
    for path in sources:
        text = path.read_text(encoding="utf-8")
        for m in DECL.finditer(text):
            names.add(m.group(1))
        if path.suffix == ".rs":
            for m in RUST_FN.finditer(text):
                names.add(m.group(1))
    return names


def load_known() -> tuple[set[str], set[str]]:
    return known_names(NODE_SOURCES), known_names(PY_SOURCES)


def scan(md: Path, node: set[str], py: set[str], inline_scope: str) -> list[str]:
    """Return a list of failure messages for one markdown file.

    `inline_scope` picks which set an inline `name()` token is checked against:
    "node", "python", "any" (SKILL.md mixes both languages), or "skip" (rust.md, whose
    inline tokens are namespaced accessors, not a flat method list). Fenced ts/python
    `forge.NAME(` calls are always checked regardless of `inline_scope`.
    """
    problems: list[str] = []
    fence_lang: str | None = None
    for lineno, line in enumerate(md.read_text(encoding="utf-8").splitlines(), 1):
        fence = FENCE.match(line)
        if fence:
            fence_lang = None if fence_lang is not None else (fence.group(1).lower() or "")
            continue

        if fence_lang is None:  # prose line: check inline `name()` tokens
            for m in INLINE_METHOD.finditer(line):
                name = m.group(1)
                if inline_scope == "node" and name not in node:
                    problems.append(f"{md.relative_to(ROOT)}:{lineno}: `{name}()` not a Node method")
                elif inline_scope == "python" and name not in py:
                    problems.append(f"{md.relative_to(ROOT)}:{lineno}: `{name}()` not a Python method")
                elif inline_scope == "any" and name not in node and name not in py:
                    problems.append(f"{md.relative_to(ROOT)}:{lineno}: `{name}()` not a Node or Python method")
            continue

        # inside a fenced block: check flat `forge.NAME(` / `client.NAME(` calls
        if fence_lang in NODE_FENCE_LANGS:
            expected, label = node, "Node"
        elif fence_lang in PY_FENCE_LANGS:
            expected, label = py, "Python"
        else:
            continue  # rust/toml/sh: not machine-checked here
        for m in CLIENT_CALL.finditer(line):
            name = m.group(1)
            if name not in expected:
                problems.append(f"{md.relative_to(ROOT)}:{lineno}: forge.{name}() not a {label} method")
    return problems


def main() -> int:
    node, py = load_known()
    if not node or not py:
        print("skill-check: could not read the binding sources", file=sys.stderr)
        return 2

    targets = [
        (SKILL_DIR / "SKILL.md", "any"),
        (SKILL_DIR / "references" / "node.md", "node"),
        (SKILL_DIR / "references" / "python.md", "python"),
        (SKILL_DIR / "references" / "rust.md", "skip"),  # inline tokens not checked
    ]
    problems: list[str] = []
    checked = 0
    for md, scope in targets:
        if not md.exists():
            problems.append(f"missing skill file: {md.relative_to(ROOT)}")
            continue
        checked += 1
        problems.extend(scan(md, node, py, scope))

    if problems:
        print("skill-check: the skill names methods that are not in the bindings:\n", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        print(
            "\nUpdate skills/forge-idiomatic-developer to match the current API, "
            "or fix the binding.",
            file=sys.stderr,
        )
        return 1

    print(f"skill-check: OK — {checked} skill files verified against the bindings")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""GraphQL surface (Strawberry, code-first): types, queries, mutations, subscriptions.

Every relational field resolves through a per-request DataLoader (see loaders.py), so a
query selecting N messages never issues N per-row lookups. Auth is Bearer-only and read
through the request Context."""

from __future__ import annotations

from .schema import schema

__all__ = ["schema"]

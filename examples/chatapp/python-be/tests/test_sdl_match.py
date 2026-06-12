"""The parity guarantee: the Strawberry-emitted SDL equals the canonical schema.graphql
under the normalized comparison (sort types + fields, ignore descriptions, treat an
absent default as equal to `= null`)."""

from pathlib import Path

from app.gql import schema
from app.sdl import normalize

CANONICAL = Path(__file__).resolve().parent.parent / "app" / "schema.graphql"


def test_emitted_sdl_matches_canonical():
    canonical = normalize(CANONICAL.read_text())
    emitted = normalize(schema.as_str())
    assert emitted == canonical

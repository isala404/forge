"""Forge Python bindings.

maturin builds the compiled extension into this package; this re-export keeps
``import forgelib; forgelib.ForgeClient`` (and the typed exception hierarchy) working.
The pure-Python typed projection ships as the sibling ``forge_typed`` module.
"""

from .forgelib import *  # noqa: F401,F403

"""Forge Python bindings.

maturin builds the compiled extension into this package; this re-export keeps
``import forge_py; forge_py.ForgeClient`` (and the typed exception hierarchy) working.
The pure-Python typed projection ships as the sibling ``forge_typed`` module.
"""

from .forge_py import *  # noqa: F401,F403

"""Forge Python bindings.

The compiled extension is built into this package by maturin; this re-export keeps
``import forge_py; forge_py.ForgeClient`` (and the typed exception hierarchy) working
exactly as before, while the pure-Python typed projection ships as the sibling
``forge_typed`` module.
"""

from .forge_py import *  # noqa: F401,F403  (re-export the compiled extension surface)

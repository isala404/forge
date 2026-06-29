from __future__ import annotations

from graphql import build_schema, lexicographic_sort_schema, parse, print_ast, print_schema
from graphql.language import NullValueNode, Visitor, visit


class _Strip(Visitor):
    def enter(self, node, *args):
        if getattr(node, "description", None) is not None:
            node.description = None
        if node.__class__.__name__ == "InputValueDefinitionNode" and isinstance(
            node.default_value, NullValueNode
        ):
            node.default_value = None
        return node


def normalize(sdl: str) -> str:
    sorted_schema = lexicographic_sort_schema(build_schema(sdl))
    ast = parse(print_schema(sorted_schema))
    return print_ast(visit(ast, _Strip()))

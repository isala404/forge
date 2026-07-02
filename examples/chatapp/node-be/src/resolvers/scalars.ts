import { GraphQLScalarType, Kind, type ValueNode } from "graphql";

import { err } from "../errors.ts";

export const DateTime = new GraphQLScalarType<Date, string>({
  name: "DateTime",
  description: "RFC3339 / ISO-8601 UTC timestamp.",
  serialize(value: unknown): string {
    if (value instanceof Date) return value.toISOString();
    return new Date(value as string | number).toISOString();
  },
  parseValue(value: unknown): Date {
    return new Date(value as string | number);
  },
  parseLiteral(ast: ValueNode): Date {
    if (ast.kind === Kind.STRING) return new Date(ast.value);
    throw err("INVALID", "DateTime must be a string");
  },
});

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { makeExecutableSchema } from "@graphql-tools/schema";
import { printSchema, type GraphQLSchema } from "graphql";

import { resolvers } from "./resolvers/index.ts";

export const SCHEMA_PATH = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "schema.graphql",
);

const typeDefs = readFileSync(SCHEMA_PATH, "utf8");

export function buildSchema(): GraphQLSchema {
  return makeExecutableSchema({ typeDefs, resolvers });
}

// The exact SDL the server serves, printed with the same `graphql` instance that
// built the schema. The parity test re-parses this to avoid cross-realm issues.
export function servedSDL(): string {
  return printSchema(buildSchema());
}

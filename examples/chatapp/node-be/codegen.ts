import type { CodegenConfig } from "@graphql-codegen/cli";

const config: CodegenConfig = {
  schema: "../schema.graphql",
  generates: {
    "src/generated/graphql.ts": {
      plugins: ["typescript", "typescript-resolvers"],
      config: {
        useIndexSignature: true,
        useTypeImports: true,
        enumsAsTypes: true,
        contextType: "../context.ts#GqlContext",
        scalars: { DateTime: "Date" },
        mappers: {
          User: "../db.ts#UserRow",
          Chat: "../db.ts#ChatRow",
          Message: "../db.ts#MessageRow",
          Receipt: "../db.ts#ReceiptRow",
        },
      },
    },
  },
};

export default config;

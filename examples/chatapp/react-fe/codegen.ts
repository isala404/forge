import type { CodegenConfig } from '@graphql-codegen/cli'

// Client preset against the canonical SDL. Every operation in src/ is collected
// from the typed `graphql()` tag and turned into a generated TypedDocumentNode.
const config: CodegenConfig = {
  schema: '../schema.graphql',
  documents: ['src/**/*.{ts,tsx}', '!src/gql/**/*'],
  ignoreNoDocuments: true,
  generates: {
    './src/gql/': {
      preset: 'client',
      config: {
        // The project uses verbatimModuleSyntax; emit type-only imports so the
        // generated files typecheck under tsc -b.
        useTypeImports: true,
        scalars: {
          DateTime: 'string',
          ID: 'string',
        },
      },
    },
  },
}

export default config

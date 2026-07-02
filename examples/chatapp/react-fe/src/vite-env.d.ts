/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_GRAPHQL_HTTP?: string
  readonly VITE_GRAPHQL_WS?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}

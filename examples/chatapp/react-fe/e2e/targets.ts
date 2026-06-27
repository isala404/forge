export const TARGETS = {
  rust: { fe: 'http://127.0.0.1:8091', be: 'http://127.0.0.1:8081/graphql' },
  node: { fe: 'http://127.0.0.1:8092', be: 'http://127.0.0.1:8082/graphql' },
  python: { fe: 'http://127.0.0.1:8093', be: 'http://127.0.0.1:8083/graphql' },
} as const

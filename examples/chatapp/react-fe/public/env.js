// Runtime backend config. In dev this stays empty and the app falls back to Vite
// env vars; in the Docker image docker-entrypoint.sh overwrites it from the
// container's VITE_GRAPHQL_HTTP / VITE_GRAPHQL_WS.
window.__ENV__ = {}

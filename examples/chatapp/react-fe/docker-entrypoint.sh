#!/bin/sh
# Write runtime backend config from the container env. Runs via nginx's
# /docker-entrypoint.d before the server starts. Falls back to node-be defaults.
set -e

HTTP="${VITE_GRAPHQL_HTTP:-http://localhost:8082/graphql}"
WS="${VITE_GRAPHQL_WS:-ws://localhost:8082/graphql}"

cat > /usr/share/nginx/html/env.js <<EOF
window.__ENV__ = {
  VITE_GRAPHQL_HTTP: "${HTTP}",
  VITE_GRAPHQL_WS: "${WS}"
};
EOF

echo "react-fe: GraphQL HTTP=${HTTP} WS=${WS}"

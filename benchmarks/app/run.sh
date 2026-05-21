#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INFRA_DIR="$SCRIPT_DIR/infra"
APP_BINARY="$ROOT/target/release/forge-bench"
LOADGEN_BINARY="$ROOT/target/release/loadgen"
JWT_SECRET_VALUE="${JWT_SECRET:-bench-secret-not-for-production}"

FORGE_INSTANCES=2
BASE_PORT=9081
BASE_GRPC_PORT=9000
POOL_SIZE=40
GATEWAY_MAX_CONNECTIONS=16000
SSE_MAX_SESSIONS=12000

LOCAL_PRIMARY_URL="postgres://postgres:postgres@localhost:5432/app"
LOCAL_REPLICA_URLS=(
  "postgres://postgres:postgres@localhost:5433/app"
)

DB_URL=""
REPLICA_URLS=()
FORGE_URLS=()
APP_PIDS=()
APP_CONFIGS=()
APP_LOGS=()
STARTED_LOCAL_DB=0
MAX_DURATION=""

usage() {
  cat <<'EOF'
Usage:
  ./benchmarks/app/run.sh
  ./benchmarks/app/run.sh --max-duration 30m
  ./benchmarks/app/run.sh --database-url URL [--replica-url URL]
  ./benchmarks/app/run.sh --forge-url URL [--forge-url URL ...]

Modes:
  no args
    Start 1 local primary, 1 local replica, 2 local Forge instances, then ramp until stop.

  --max-duration
    Optional upper bound. By default the run is unlimited and only stops on p90/error thresholds.

  --database-url [+ --replica-url]
    Use that database, start 2 local Forge instances against it, then ramp until stop.

  --forge-url
    Skip local database and local Forge. Hit the supplied Forge URL(s) directly.
EOF
}

cleanup() {
  for pid in "${APP_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in "${APP_PIDS[@]:-}"; do
    wait "$pid" 2>/dev/null || true
  done
  for config in "${APP_CONFIGS[@]:-}"; do
    rm -f "$config"
  done
  for log in "${APP_LOGS[@]:-}"; do
    rm -f "$log"
  done
  if [[ "$STARTED_LOCAL_DB" -eq 1 ]]; then
    docker compose -f "$INFRA_DIR/docker-compose.yml" down -v >/dev/null 2>&1 || true
  fi
}

trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --database-url)
      DB_URL="$2"
      shift
      ;;
    --replica-url)
      REPLICA_URLS+=("$2")
      shift
      ;;
    --forge-url)
      FORGE_URLS+=("$2")
      shift
      ;;
    --max-duration)
      MAX_DURATION="$2"
      shift
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
  shift
done

quote_toml_array() {
  local values=("$@")
  local out="["
  local first=1
  local value
  for value in "${values[@]}"; do
    if [[ "$first" -eq 0 ]]; then
      out+=", "
    fi
    out+="\"$value\""
    first=0
  done
  out+="]"
  printf '%s' "$out"
}

wait_for_postgres() {
  local container="$1"
  echo "Waiting for ${container}..."
  for _ in $(seq 1 90); do
    if docker exec "$container" sh -lc "psql -U postgres -d app -Atqc 'select 1'" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  echo "${container} did not become ready" >&2
  docker logs "$container" || true
  exit 1
}

start_local_database() {
  STARTED_LOCAL_DB=1
  DB_URL="$LOCAL_PRIMARY_URL"
  REPLICA_URLS=("${LOCAL_REPLICA_URLS[@]}")

  echo ""
  echo "=== Starting PostgreSQL (1 primary + 1 replica) ==="
  docker compose -f "$INFRA_DIR/docker-compose.yml" down -v >/dev/null 2>&1 || true
  docker compose -f "$INFRA_DIR/docker-compose.yml" up -d >/dev/null

  wait_for_postgres bench-pg-primary
  wait_for_postgres bench-pg-replica
}

write_config() {
  local file="$1"
  local port="$2"
  local grpc_port="$3"
  local name="$4"

  cat >"$file" <<EOF
[project]
name = "$name"

[node]
roles = ["gateway", "function"]

[database]
url = "$DB_URL"
pool_size = $POOL_SIZE
pool_timeout = "10s"
test_before_acquire = false
read_from_replica = true
replica_urls = $(quote_toml_array "${REPLICA_URLS[@]}")

[gateway]
port = $port
grpc_port = $grpc_port
max_connections = $GATEWAY_MAX_CONNECTIONS
sse_max_sessions = $SSE_MAX_SESSIONS
request_timeout_secs = 60

[auth]
jwt_algorithm = "HS256"
jwt_secret = "$JWT_SECRET_VALUE"

[observability]
enabled = false
EOF
}

start_local_forge() {
  echo ""
  echo "=== Starting Forge instances ==="

  for ((i=0; i<FORGE_INSTANCES; i++)); do
    local port=$((BASE_PORT + i))
    local grpc_port=$((BASE_GRPC_PORT + i))
    local config_file
    local log_file

    config_file="$(mktemp "${TMPDIR:-/tmp}/forge-bench-${port}.toml.XXXXXX")"
    log_file="$(mktemp "${TMPDIR:-/tmp}/forge-bench-${port}.log.XXXXXX")"
    APP_CONFIGS+=("$config_file")
    APP_LOGS+=("$log_file")
    FORGE_URLS+=("http://127.0.0.1:${port}")

    write_config "$config_file" "$port" "$grpc_port" "forge-bench-${port}"

    (
      cd "$SCRIPT_DIR"
      FORGE_CONFIG="$config_file" \
      JWT_SECRET="$JWT_SECRET_VALUE" \
      RUST_LOG=error \
      "$APP_BINARY"
    ) >"$log_file" 2>&1 &
    APP_PIDS+=("$!")
  done

  for url in "${FORGE_URLS[@]}"; do
    echo "Waiting for ${url}..."
    for _ in $(seq 1 60); do
      if curl -sf "${url}/_api/ready" >/dev/null 2>&1; then
        break
      fi
      sleep 1
    done
    curl -sf "${url}/_api/ready" >/dev/null
  done
}

echo "=== Building benchmark binaries ==="
cargo build --release -p forge-bench --bins

if [[ ${#FORGE_URLS[@]} -gt 0 ]]; then
  echo ""
  echo "=== Using external Forge ==="
else
  if [[ -z "$DB_URL" && ${#REPLICA_URLS[@]} -eq 0 ]]; then
    command -v docker >/dev/null 2>&1 || { echo "Missing: docker"; exit 1; }
    start_local_database
  else
    if [[ -z "$DB_URL" ]]; then
      echo "External database mode requires --database-url." >&2
      exit 1
    fi
  fi

  start_local_forge
fi

echo ""
echo "=== Running benchmark ==="
CMD=("$LOADGEN_BINARY")
if [[ -n "$MAX_DURATION" ]]; then
  CMD+=(--max-duration "$MAX_DURATION")
fi
CMD+=("${FORGE_URLS[@]}")
"${CMD[@]}"

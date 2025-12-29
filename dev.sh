#!/bin/bash
set -e

FORGE_ROOT="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="${APP_DIR:-$HOME/Desktop/forge-dev-app}"
APP_NAME="$(basename "$APP_DIR")"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${GREEN}[FORGE]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
err() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# Cleanup on exit
cleanup() {
    log "Shutting down..."
    [ -n "$BACKEND_PID" ] && kill $BACKEND_PID 2>/dev/null
    [ -n "$FRONTEND_PID" ] && kill $FRONTEND_PID 2>/dev/null
    exit 0
}
trap cleanup SIGINT SIGTERM

usage() {
    echo "Usage: $0 [command]"
    echo ""
    echo "Commands:"
    echo "  setup     Build CLI, scaffold app, fix deps, start DB"
    echo "  start     Start backend and frontend (requires setup first)"
    echo "  db        Start/restart PostgreSQL container only"
    echo "  logs      Tail backend and frontend logs"
    echo "  clean     Remove dev app and stop DB"
    echo "  all       Run setup + start (default)"
    echo ""
    echo "Environment:"
    echo "  APP_DIR   App directory (default: ~/Desktop/forge-dev-app)"
}

# Start PostgreSQL
start_db() {
    log "Starting PostgreSQL..."
    docker rm -f forge-dev-db 2>/dev/null || true
    docker run -d \
        --name forge-dev-db \
        -e POSTGRES_DB=forge_dev \
        -e POSTGRES_USER=forge \
        -e POSTGRES_PASSWORD=forge \
        -p 5432:5432 \
        postgres:16-alpine

    log "Waiting for PostgreSQL..."
    for i in {1..30}; do
        if docker exec forge-dev-db pg_isready -U forge -d forge_dev >/dev/null 2>&1; then
            log "PostgreSQL ready"
            return 0
        fi
        sleep 1
    done
    err "PostgreSQL failed to start"
}

# Build and install CLI
build_cli() {
    log "Building FORGE CLI..."
    cd "$FORGE_ROOT"
    LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo install --path crates/forge --force
    log "CLI installed: $(~/.cargo/bin/forge --version)"
}

# Scaffold app
scaffold_app() {
    if [ -d "$APP_DIR" ]; then
        warn "Removing existing app at $APP_DIR"
        rm -rf "$APP_DIR"
    fi

    log "Scaffolding app at $APP_DIR..."
    cd "$(dirname "$APP_DIR")"
    ~/.cargo/bin/forge new "$APP_NAME"
}

# Fix forge dependency to use local source
fix_deps() {
    log "Linking to local forge source..."

    # Update Cargo.toml to use local path
    sed -i '' "s|forge = \"0.1\"|forge = { path = \"$FORGE_ROOT/crates/forge\" }|" "$APP_DIR/Cargo.toml"

    # Install frontend deps
    log "Installing frontend dependencies..."
    cd "$APP_DIR/frontend"
    bun install
}

# Start backend
start_backend() {
    log "Starting backend..."
    cd "$APP_DIR"

    mkdir -p .logs
    LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo run 2>&1 | tee .logs/backend.log &
    BACKEND_PID=$!

    # Wait for backend to be ready
    for i in {1..60}; do
        if curl -s http://localhost:8080/health >/dev/null 2>&1; then
            log "Backend ready at http://localhost:8080"
            log "Dashboard at http://localhost:8080/_dashboard"
            return 0
        fi
        sleep 1
    done
    warn "Backend may not be fully ready yet"
}

# Start frontend
start_frontend() {
    log "Starting frontend..."
    cd "$APP_DIR/frontend"

    mkdir -p ../.logs
    bun dev 2>&1 | tee ../.logs/frontend.log &
    FRONTEND_PID=$!

    sleep 3
    log "Frontend ready at http://localhost:5173"
}

# Tail logs
tail_logs() {
    if [ ! -d "$APP_DIR/.logs" ]; then
        err "No logs found. Run 'start' first."
    fi
    tail -f "$APP_DIR/.logs/backend.log" "$APP_DIR/.logs/frontend.log"
}

# Clean up
clean() {
    log "Cleaning up..."
    docker rm -f forge-dev-db 2>/dev/null || true
    if [ -d "$APP_DIR" ]; then
        rm -rf "$APP_DIR"
        log "Removed $APP_DIR"
    fi
}

# Main
case "${1:-all}" in
    setup)
        start_db
        build_cli
        scaffold_app
        fix_deps
        log "Setup complete. Run '$0 start' to start services."
        ;;
    start)
        [ ! -d "$APP_DIR" ] && err "App not found. Run '$0 setup' first."
        start_backend
        start_frontend
        log ""
        log "Services running:"
        log "  Frontend: http://localhost:5173"
        log "  Backend:  http://localhost:8080"
        log "  Dashboard: http://localhost:8080/_dashboard"
        log ""
        log "Logs: $APP_DIR/.logs/"
        log "Press Ctrl+C to stop"
        wait
        ;;
    db)
        start_db
        ;;
    logs)
        tail_logs
        ;;
    clean)
        clean
        ;;
    all)
        start_db
        build_cli
        scaffold_app
        fix_deps
        start_backend
        start_frontend
        log ""
        log "Services running:"
        log "  Frontend: http://localhost:5173"
        log "  Backend:  http://localhost:8080"
        log "  Dashboard: http://localhost:8080/_dashboard"
        log ""
        log "Logs: $APP_DIR/.logs/"
        log "Press Ctrl+C to stop"
        wait
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        err "Unknown command: $1"
        ;;
esac

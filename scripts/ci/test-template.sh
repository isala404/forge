#!/usr/bin/env bash
# Usage: test-template.sh <template> <forge-binary> <workspace-dir> [playwright-args...]
# Scaffolds a project, validates with forge check, then runs forge test.
set -euo pipefail

TEMPLATE="$1"
FORGE="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
WORKSPACE="$3"
shift 3
PLAYWRIGHT_ARGS=("$@")
SLUG=$(echo "$TEMPLATE" | tr '/' '-')
DIR="/tmp/test-project-$SLUG-$(date +%s)-$$"
ARTIFACT_ROOT="${FORGE_TEST_ARTIFACT_DIR:-/tmp/forge-test-artifacts}"
ARTIFACT_DIR="$ARTIFACT_ROOT/$SLUG-$(date +%s)-$$"

cleanup() {
  if [ -d "$DIR/frontend/test-results" ]; then
    mkdir -p "$ARTIFACT_DIR"
    cp -R "$DIR/frontend/test-results" "$ARTIFACT_DIR/" 2>/dev/null || true
  fi
  if [ -d "$DIR/frontend/playwright-report" ]; then
    mkdir -p "$ARTIFACT_DIR"
    cp -R "$DIR/frontend/playwright-report" "$ARTIFACT_DIR/" 2>/dev/null || true
  fi
  rm -rf "$DIR" 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p "$ARTIFACT_DIR"

echo "=== Scaffold ==="
"$FORGE" new "test-$SLUG" --template "$TEMPLATE" --output "$DIR" --no-lock --include-skill

# Patch Dioxus frontend to use local forge-dioxus source
if [ -f "$DIR/frontend/Cargo.toml" ] && grep -q 'forge-dioxus' "$DIR/frontend/Cargo.toml"; then
  sed -i.bak "s|forge-dioxus = .*|forge-dioxus = { path = \"$WORKSPACE/packages/forge-dioxus\" }|" "$DIR/frontend/Cargo.toml"
  rm -f "$DIR/frontend/Cargo.toml.bak"
fi

# Patch npm packages to local source
if [ -f "$DIR/frontend/package.json" ] && grep -q '@forge-rs/svelte' "$DIR/frontend/package.json"; then
  jq --arg p "file:$WORKSPACE/packages/forge-svelte" '
    if .dependencies["@forge-rs/svelte"] then .dependencies["@forge-rs/svelte"] = $p
    elif .devDependencies["@forge-rs/svelte"] then .devDependencies["@forge-rs/svelte"] = $p
    else . end
  ' "$DIR/frontend/package.json" > "$DIR/frontend/package.json.tmp"
  mv "$DIR/frontend/package.json.tmp" "$DIR/frontend/package.json"
fi

cd "$DIR"

echo "=== Auto-format generated code ==="
cargo fmt 2>/dev/null || true
find "$DIR/frontend" -name '*.rs' -exec rustfmt --edition 2024 {} + 2>/dev/null || true
if [ -d "$DIR/frontend" ] && [ -f "$DIR/frontend/package.json" ]; then
  cd "$DIR/frontend" && bun install --no-save 2>/dev/null && bunx prettier --write . 2>/dev/null || true && cd "$DIR"
fi

echo "=== Forge check ==="
"$FORGE" check

echo "=== Install Playwright (with system deps for CI) ==="
if [ -d "$DIR/frontend" ]; then
  cd "$DIR/frontend" && bun install && bunx playwright install chromium --with-deps && cd "$DIR"
fi

echo "=== Run forge test ==="
PW_ARGS=(--fail-on-flaky-tests)
if [ "${#PLAYWRIGHT_ARGS[@]}" -gt 0 ]; then
  PW_ARGS+=("${PLAYWRIGHT_ARGS[@]}")
fi
CI=true "$FORGE" test -- "${PW_ARGS[@]}"

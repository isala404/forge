#!/usr/bin/env bash
# Usage: typecheck-codegen.sh
#
# Regenerates frontend bindings for every SvelteKit example with the local
# forge CLI, then type-checks the result with svelte-check. This proves the
# generator emits TypeScript that compiles against @forge-rs/svelte without
# starting a browser or a Postgres instance.
#
# Dioxus bindings are plain Rust covered by `cargo build --workspace`, so this
# script only handles the SvelteKit targets. Override the binary with FORGE_BIN.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

FORGE="${FORGE_BIN:-$REPO_ROOT/target/debug/forge}"
if [ ! -x "$FORGE" ]; then
  echo "=== Building forge CLI ==="
  cargo build -p forgex
fi

EXAMPLES=(minimal demo realtime-todo-list)
FAILED=()

for ex in "${EXAMPLES[@]}"; do
  PROJECT="examples/with-svelte/$ex"
  FRONTEND="$PROJECT/frontend"

  if [ ! -d "$FRONTEND/node_modules" ]; then
    echo "=== $ex: bun install ==="
    if ! ( cd "$FRONTEND" && bun install ); then
      FAILED+=("$ex (install)")
      continue
    fi
  fi

  echo "=== $ex: forge generate --force ==="
  if ! ( cd "$PROJECT" && "$FORGE" generate --force ); then
    FAILED+=("$ex (generate)")
    continue
  fi

  # Generated bindings are committed prettier-formatted; reformat so a healthy
  # run leaves no diff and `prettier --check` stays green.
  ( cd "$FRONTEND" && bunx prettier --write src/lib/forge >/dev/null ) || true

  echo "=== $ex: svelte-check ==="
  if ! ( cd "$FRONTEND" && bunx svelte-check --tsconfig ./tsconfig.json ); then
    FAILED+=("$ex (typecheck)")
  fi
done

echo ""
if [ "${#FAILED[@]}" -gt 0 ]; then
  echo "FAILED: ${FAILED[*]}"
  exit 1
fi
echo "All ${#EXAMPLES[@]} SvelteKit examples type-check clean."

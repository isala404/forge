#!/usr/bin/env bash
# Usage: bump-versions.sh <version>
set -euo pipefail

VERSION="$1"
echo "Bumping to $VERSION"

cargo set-version --workspace "$VERSION"

# Runtime packages
[ -f packages/forge-svelte/package.json ] && \
  jq --arg v "$VERSION" '.version = $v' packages/forge-svelte/package.json > packages/forge-svelte/package.json.tmp && \
  mv packages/forge-svelte/package.json.tmp packages/forge-svelte/package.json
[ -f packages/forge-dioxus/Cargo.toml ] && \
  sed -i "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" packages/forge-dioxus/Cargo.toml

# Regenerate lockfiles for Dioxus frontend examples (standalone projects with path deps)
for lockfile in examples/with-dioxus/*/frontend/Cargo.lock; do
  [ -f "$lockfile" ] || continue
  (cd "$(dirname "$lockfile")" && cargo generate-lockfile --quiet)
done

# Docs
[ -f docs/package.json ] && \
  jq --arg v "$VERSION" '.version = $v' docs/package.json > docs/package.json.tmp && \
  mv docs/package.json.tmp docs/package.json
find docs -type d -name node_modules -prune -o -type f \( -name "*.mdx" -o -name "*.md" \) -print \
  | xargs -I {} sed -i "s/forge = { version = \"[^\"]*\"/forge = { version = \"$VERSION\"/g" {} 2>/dev/null || true
find docs -type d -name node_modules -prune -o -type f \( -name "*.mdx" -o -name "*.md" \) -print \
  | xargs -I {} sed -i "s/forgex = { version = \"[^\"]*\"/forgex = { version = \"$VERSION\"/g" {} 2>/dev/null || true

echo "Done. Verify with: git diff --stat"

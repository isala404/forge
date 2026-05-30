#!/usr/bin/env bash
set -euo pipefail

if [ ! -f "CHANGELOG.md" ]; then
  echo "::error::CHANGELOG.md not found"
  exit 1
fi

VERSION=$(grep -E '^\#\# \[[0-9]+\.[0-9]+\.[0-9]+' CHANGELOG.md | head -1 | sed -E 's/^\#\# \[([0-9]+\.[0-9]+\.[0-9]+[^]]*)\].*/\1/')

if [ -z "$VERSION" ]; then
  echo "::error::No version found in CHANGELOG.md"
  exit 1
fi

# Tag-existence check happens later in release.yml via `git ls-remote`; a local
# `git rev-parse` would silently no-op under the workflow's shallow clone
# (actions/checkout defaults to fetch-depth: 1 with no tags fetched).

# Cross-check against the workspace Cargo.toml version
if command -v cargo >/dev/null 2>&1; then
  CARGO_VERSION=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c "import json,sys; m=json.load(sys.stdin); pkgs=[p for p in m['packages'] if p['name']=='forgex']; print(pkgs[0]['version'] if pkgs else '')" \
    2>/dev/null || true)
  if [ -n "$CARGO_VERSION" ] && [ "$CARGO_VERSION" != "$VERSION" ]; then
    echo "::error::CHANGELOG version $VERSION does not match Cargo workspace version $CARGO_VERSION"
    exit 1
  fi
fi

VERSION_LINE=$(grep -E "^\#\# \[$VERSION\]" CHANGELOG.md)
if ! echo "$VERSION_LINE" | grep -qE '\- [0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
  echo "::error::Version $VERSION missing release date"
  exit 1
fi

RELEASE_DATE=$(echo "$VERSION_LINE" | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}$')

UNRELEASED_CONTENT=$(awk '/^\#\# \[Unreleased\]/,/^\#\# \[/ {print}' CHANGELOG.md | tail -n +2 | sed '$d' | grep -v '^$' || true)
if [ -n "$UNRELEASED_CONTENT" ]; then
  echo "::warning::Unreleased section has content"
fi

RELEASE_NOTES=$(awk -v ver="$VERSION" '
  BEGIN { found=0; printing=0 }
  /^## \[/ {
    if (printing) exit
    if (index($0, "["ver"]")) { found=1; printing=1; next }
  }
  printing { print }
' CHANGELOG.md)

if [ -z "$RELEASE_NOTES" ]; then
  echo "::error::No release notes for version $VERSION"
  exit 1
fi

IS_PRERELEASE="false"
echo "$VERSION" | grep -qE '(alpha|beta|rc)' && IS_PRERELEASE="true"

echo "version=$VERSION" >> "$GITHUB_OUTPUT"
echo "release_date=$RELEASE_DATE" >> "$GITHUB_OUTPUT"
echo "is_prerelease=$IS_PRERELEASE" >> "$GITHUB_OUTPUT"
DELIM="EOF_$(openssl rand -hex 16)"
{
  echo "release_notes<<$DELIM"
  echo "$RELEASE_NOTES"
  echo "$DELIM"
} >> "$GITHUB_OUTPUT"

echo "Version: $VERSION | Date: $RELEASE_DATE | Prerelease: $IS_PRERELEASE"

#!/usr/bin/env bash
set -euo pipefail

# Catch runtime sqlx::query() and sqlx::query_as() calls that bypass
# compile-time SQL checking. Only the macro forms (with !) are allowed
# in application handler code.
#
# Allowed exceptions (documented reasons for runtime queries):
#   - testing/       Test infrastructure uses dynamic DDL
#   - signals/       UNNEST with typed arrays unsupported by sqlx macros
#   - migrations/    DDL execution is inherently dynamic
#   - jobs/          SKIP LOCKED claim queries built dynamically
#   - workflow/executor.rs  Saved-state JSON round-trip
#   - sql_extractor.rs     Example strings in comments/tests
#   - cli/check.rs         Test fixture strings
#   - tests.rs             Integration test helpers
#   - realtime/listener.rs Change log replay (system table, not in .sqlx cache)

ALLOWED_FILES=(
  "testing/"
  "signals/"
  "migrations/"
  "jobs/"
  "workflow/executor.rs"
  "sql_extractor.rs"
  "cli/check.rs"
  "tests.rs"
  "webhook/handler.rs"
  "realtime/listener.rs"
)

found=0

while IFS= read -r match; do
  skip=false
  for pattern in "${ALLOWED_FILES[@]}"; do
    if [[ "$match" == *"$pattern"* ]]; then
      skip=true
      break
    fi
  done
  # Skip lines that are comments or doc comments (content is after file:line: prefix)
  content="${match#*:*:}"
  if echo "$content" | grep -qE '^\s*(//|/\*\*)'; then
    skip=true
  fi
  $skip && continue

  echo "::error::Runtime SQL query detected (use sqlx::query!() or sqlx::query_as!() instead): $match"
  found=$((found + 1))
done < <(grep -rn --include='*.rs' -E 'sqlx::query(_as)?\(' crates/ | grep -v 'sqlx::query!' | grep -v 'sqlx::query_as!')

if [ "$found" -gt 0 ]; then
  echo "Found $found unexpected runtime SQL queries. Use compile-time checked macros instead."
  echo "If a file legitimately needs runtime queries, add it to the ALLOWED_FILES list in this script."
  exit 1
fi

echo "No unexpected runtime SQL queries found."

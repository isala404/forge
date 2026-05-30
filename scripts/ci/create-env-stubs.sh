#!/usr/bin/env bash
set -euo pipefail
for dir in examples/with-*/*/frontend; do
  [ -d "$dir" ] || continue
  [ -f "$dir/.env" ] || echo 'PUBLIC_API_URL=http://localhost:9081' > "$dir/.env"
done

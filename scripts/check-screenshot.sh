#!/usr/bin/env bash
set -euo pipefail

config=${PHROURION_SCREENSHOT_CONFIG-}
output=docs/screenshots/dashboard.svg
temporary=$(mktemp)
trap 'rm -f "$temporary"' EXIT

if [[ -n "$config" ]]; then
  scripts/screenshot.sh "$config" "$temporary"
else
  scripts/screenshot.sh "" "$temporary"
fi

if ! cmp -s "$temporary" "$output"; then
  echo "Dashboard screenshot is stale. Run scripts/screenshot.sh and stage $output." >&2
  exit 1
fi

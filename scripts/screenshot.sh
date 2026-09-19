#!/usr/bin/env bash
set -euo pipefail

config=${1-}
output=${2-docs/screenshots/dashboard.svg}

args=(--bin phro-screenshot -- "$config" "$output")
if [[ -n ${PHROURION_SCREENSHOT_LIVE-} ]]; then
  cargo run --quiet "${args[@]}"
else
  screenshot_cache=$(mktemp -d)
  trap 'rmdir "$screenshot_cache" 2>/dev/null || true' EXIT
  XDG_CACHE_HOME=$screenshot_cache cargo run --quiet "${args[@]}"
fi

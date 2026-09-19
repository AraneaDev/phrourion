#!/usr/bin/env bash
set -euo pipefail

config=${PHROURION_SCREENSHOT_CONFIG-}

check_screenshot() {
  local mode=$1
  local output=$2
  local temporary
  temporary=$(mktemp)
  if [[ -n "$config" ]]; then
    scripts/screenshot.sh "$config" "$temporary" "$mode"
  else
    scripts/screenshot.sh "" "$temporary" "$mode"
  fi
  if ! cmp -s "$temporary" "$output"; then
    echo "${mode} screenshot is stale. Run scripts/screenshot.sh and stage $output." >&2
    rm -f "$temporary"
    exit 1
  fi
  rm -f "$temporary"
}

check_screenshot dashboard docs/screenshots/dashboard.svg
check_screenshot help docs/screenshots/help-modal.svg

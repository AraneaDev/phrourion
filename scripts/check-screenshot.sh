#!/usr/bin/env bash
set -euo pipefail

check_screenshot() {
  local mode=$1
  local output=$2
  local temporary
  temporary=$(mktemp)
  scripts/screenshot.sh "" "$temporary" "$mode"
  if ! cmp -s "$temporary" "$output"; then
    echo "${mode} screenshot is stale. Run scripts/screenshot.sh and stage $output." >&2
    rm -f "$temporary"
    exit 1
  fi
  rm -f "$temporary"
}

check_screenshot dashboard docs/screenshots/dashboard.gif
check_screenshot help docs/screenshots/help-modal.gif

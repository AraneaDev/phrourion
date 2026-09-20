#!/usr/bin/env bash
set -euo pipefail

check_screenshot() {
  local mode=$1
  local output=$2
  local temporary
  temporary=$(mktemp)
  scripts/screenshot.sh "" "$temporary" "$mode"
  local generated_signature expected_signature generated_frames expected_frames
  generated_signature=$(identify -format '%c|%wx%h|%T' "$temporary[0]")
  expected_signature=$(identify -format '%c|%wx%h|%T' "$output[0]")
  generated_frames=$(identify "$temporary" | wc -l)
  expected_frames=$(identify "$output" | wc -l)
  if [[ "$generated_signature" != "$expected_signature" || "$generated_frames" != "$expected_frames" ]]; then
    echo "${mode} screenshot is stale. Run scripts/screenshot.sh and stage $output." >&2
    rm -f "$temporary"
    exit 1
  fi
  rm -f "$temporary"
}

check_screenshot dashboard docs/screenshots/dashboard.gif
check_screenshot help docs/screenshots/help-modal.gif

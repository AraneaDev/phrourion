#!/usr/bin/env bash
set -euo pipefail

config=${1-}
output=${2-docs/screenshots/dashboard.svg}

args=(--bin phro-screenshot -- "$output")
if [[ -n "$config" ]]; then
  args=(--bin phro-screenshot -- "$config" "$output")
fi
cargo run --quiet "${args[@]}"

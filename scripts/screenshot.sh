#!/usr/bin/env bash
set -euo pipefail

config=${1-}
output=${2-docs/screenshots/dashboard.svg}

args=(--bin phro-screenshot -- "$config" "$output")
cargo run --quiet "${args[@]}"

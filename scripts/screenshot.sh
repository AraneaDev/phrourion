#!/usr/bin/env bash
set -euo pipefail

config=${1-}
mode=${3-dashboard}
output=${2-}

if [[ -n ${PHROURION_SCREENSHOT_LIVE-} ]]; then
  if [[ -z $output ]]; then
    case "$mode" in
      dashboard) output=docs/screenshots/dashboard.svg ;;
      help) output=docs/screenshots/help-modal.svg ;;
      *) echo "unknown screenshot mode: $mode" >&2; exit 1 ;;
    esac
  fi
  cargo run --quiet --bin phro-screenshot -- "$config" "$output" "$mode"
  exit 0
fi

if [[ -z $output ]]; then
  case "$mode" in
    dashboard) output=docs/screenshots/dashboard.gif ;;
    help) output=docs/screenshots/help-modal.gif ;;
    *) echo "unknown screenshot mode: $mode" >&2; exit 1 ;;
  esac
fi

case "$mode" in
  dashboard|help) frame_count=4 ;;
  *) echo "unknown screenshot mode: $mode" >&2; exit 1 ;;
esac

temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

for ((frame = 0; frame < frame_count; frame++)); do
  svg="$temporary/frame-$frame.svg"
  png="$temporary/frame-$frame.png"
  cargo run --quiet --bin phro-screenshot -- \
    --fixture "$mode" --frame "$frame" "$svg" >/dev/null
  rsvg-convert "$svg" -o "$png"
done

if [[ -n $output ]]; then
  mkdir -p "$(dirname "$output")"
fi
generated="$temporary/output.gif"
magick -delay 100 -loop 0 "$temporary"/frame-*.png "$generated"
mv "$generated" "$output"
echo "Wrote $output"

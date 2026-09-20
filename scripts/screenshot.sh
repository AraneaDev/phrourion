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

if command -v magick >/dev/null 2>&1; then
  image_tool=magick
elif command -v convert >/dev/null 2>&1; then
  image_tool=convert
else
  echo "ImageMagick is required (magick or convert)" >&2
  exit 1
fi

temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

for ((frame = 0; frame < frame_count; frame++)); do
  svg="$temporary/frame-$frame.svg"
  png="$temporary/frame-$frame.png"
  cargo run --quiet --bin phro-screenshot -- \
    --fixture "$mode" --frame "$frame" "$svg" >/dev/null
  rsvg-convert "$svg" -o "$png"
done

package_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)
render_fingerprint=$(sha256sum "$temporary"/frame-*.svg | cut -d' ' -f1 | sha256sum | cut -d' ' -f1)

if [[ -n $output ]]; then
  mkdir -p "$(dirname "$output")"
fi
generated="$temporary/output.gif"
"$image_tool" -delay 100 -loop 0 "$temporary"/frame-*.png \
  -set comment "Phrourion v${package_version} render-${render_fingerprint}" \
  "$generated"
mv "$generated" "$output"
echo "Wrote $output"

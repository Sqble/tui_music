#!/usr/bin/env bash
set -euo pipefail

pages="library,lyrics,stats,online,actions"
resolutions=("120x36" "90x30")
font_scale="1.0"
profile="release"
build=1
output_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pages)
      pages="$2"
      shift 2
      ;;
    --resolution)
      resolutions+=("$2")
      shift 2
      ;;
    --clear-default-resolutions)
      resolutions=()
      shift
      ;;
    --font-scale)
      font_scale="$2"
      shift 2
      ;;
    --debug)
      profile="debug"
      shift
      ;;
    --no-build)
      build=0
      shift
      ;;
    --output-dir)
      output_dir="$2"
      shift 2
      ;;
    -h|--help)
      cat <<'EOF'
Usage: bash scripts/generate-screenshots.sh [options]

Options:
  --pages list                  Comma-separated pages: library,lyrics,stats,online,actions,all
  --resolution COLSxROWS        Add a terminal resolution. Defaults: 120x36 and 90x30
  --clear-default-resolutions   Remove default resolutions before adding custom ones
  --font-scale N                SVG font scale. Default: 1.0
  --debug                       Use target/debug/tune instead of target/release/tune
  --no-build                    Do not run cargo build first
  --output-dir path             Copy generated SVGs and manifest into this directory
EOF
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 1
      ;;
  esac
done

if [[ ${#resolutions[@]} -eq 0 ]]; then
  echo "at least one --resolution is required" >&2
  exit 1
fi

cargo_args=("build")
if [[ "$profile" == "release" ]]; then
  cargo_args+=("--release")
fi

if [[ "$build" -eq 1 ]]; then
  cargo "${cargo_args[@]}"
fi

exe="target/$profile/tune"
if [[ ! -x "$exe" ]]; then
  echo "executable not found: $exe" >&2
  exit 1
fi

run_args=("--screenshots" "--screenshot-pages" "$pages" "--screenshot-font-scale" "$font_scale")
for size in "${resolutions[@]}"; do
  run_args+=("--screenshot-size" "$size")
done

"$exe" "${run_args[@]}"

if [[ -n "$output_dir" ]]; then
  mkdir -p "$output_dir"
  rm -f "$output_dir"/tunetui-*.svg "$output_dir"/tunetui-screenshots-manifest.txt
  cp "target/$profile"/tunetui-*.svg "$output_dir"/
  cp "target/$profile/tunetui-screenshots-manifest.txt" "$output_dir"/
fi

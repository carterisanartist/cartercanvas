#!/usr/bin/env bash
# Builds the CarterCanvas engine (this crate) as a static library for iOS devices and the
# simulator, and packages it with its C header as xcode/Frameworks/CarterCanvasEngine.xcframework.
#
#   build-xcframework.sh [debug|release] [all|sim|device]
#
# Xcode runs this as a pre-build phase (xcode/project.yml); it also runs standalone.
set -euo pipefail

PROFILE="${1:-debug}"
SLICES="${2:-all}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="$HERE/xcode/Frameworks/CarterCanvasEngine.xcframework"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"

# Xcode's build phases run with a minimal PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
# Xcode exports SDK and arch settings that would leak into the host-side build scripts of
# dependencies (cc, cmake): clear them so each target picks its own SDK.
unset SDKROOT PLATFORM_NAME ARCHS CC CXX LD LIBRARY_PATH MACOSX_DEPLOYMENT_TARGET 2>/dev/null || true

case "$PROFILE" in
  debug) CARGO_PROFILE=(); DIR=debug ;;
  release) CARGO_PROFILE=(--release); DIR=release ;;
  *) echo "profile must be debug or release" >&2; exit 2 ;;
esac

targets=()
case "$SLICES" in
  all) targets=(aarch64-apple-ios aarch64-apple-ios-sim) ;;
  sim) targets=(aarch64-apple-ios-sim) ;;
  device) targets=(aarch64-apple-ios) ;;
  *) echo "slices must be all, sim or device" >&2; exit 2 ;;
esac

cd "$ROOT"
for t in "${targets[@]}"; do
  echo "› cargo build $PROFILE for $t"
  cargo build -p cartercanvas-ios --lib --target "$t" ${CARGO_PROFILE[@]+"${CARGO_PROFILE[@]}"}
done

HEADERS="$(mktemp -d)"
trap 'rm -rf "$HEADERS"' EXIT
cp "$HERE/include/cartercanvas.h" "$HERE/include/module.modulemap" "$HEADERS/"

args=()
for t in "${targets[@]}"; do
  lib="$ROOT/target/$t/$DIR/libcartercanvas_ios.a"
  [[ -f "$lib" ]] || { echo "missing $lib" >&2; exit 1; }
  args+=(-library "$lib" -headers "$HEADERS")
done

# Only replace the framework when a library changed, so Xcode doesn't relink for nothing.
STAMP="$HERE/xcode/Frameworks/.engine-stamp"
new_stamp="$(for t in "${targets[@]}"; do stat -f '%m %z' "$ROOT/target/$t/$DIR/libcartercanvas_ios.a"; done; shasum "$HERE/include/"* | cut -d' ' -f1)"
if [[ -d "$OUT" && -f "$STAMP" && "$(cat "$STAMP")" == "$new_stamp" ]]; then
  echo "› $OUT is up to date"
  exit 0
fi
rm -rf "$OUT"
mkdir -p "$(dirname "$OUT")"
xcodebuild -create-xcframework "${args[@]}" -output "$OUT" >/dev/null
echo "$new_stamp" > "$STAMP"
echo "› wrote $OUT"

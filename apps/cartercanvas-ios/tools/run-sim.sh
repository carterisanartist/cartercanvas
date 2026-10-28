#!/usr/bin/env bash
# Build CarterCanvas, install it on a simulator and launch it with the control server on.
#
#   tools/run-sim.sh [ipad|iphone|<simulator UDID or name>] [--no-build]
#
# Then drive it with tools/ccctl.py (same default port and token).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
XCODE="$HERE/../xcode"
WHICH="${1:-ipad}"
BUILD=1
[[ "${2:-}" == "--no-build" ]] && BUILD=0
PORT="${CC_PORT:-47600}"
TOKEN="${CC_TOKEN:-cc$(printf '0%.0s' {1..62})}"

case "$WHICH" in
  ipad) NAME="iPad Pro 13-inch (M5)" ;;
  iphone) NAME="iPhone 17 Pro Max" ;;
  *) NAME="$WHICH" ;;
esac
UDID="$(xcrun simctl list devices available -j | python3 -c "
import json, sys
want = sys.argv[1]
for runtime, devs in json.load(sys.stdin)['devices'].items():
    for d in devs:
        if d['udid'] == want or d['name'] == want:
            print(d['udid']); sys.exit(0)
sys.exit('no simulator named ' + want)
" "$NAME")"

if [[ $BUILD == 1 ]]; then
  (cd "$XCODE" && xcodegen generate --quiet && xcodebuild -project CarterCanvas.xcodeproj -scheme CarterCanvas -configuration Debug \
    -destination "platform=iOS Simulator,id=$UDID" -derivedDataPath build/DerivedData build -quiet)
fi
APP="$XCODE/build/DerivedData/Build/Products/Debug-iphonesimulator/CarterCanvas.app"

xcrun simctl boot "$UDID" 2>/dev/null || true
open -a Simulator
xcrun simctl bootstatus "$UDID" -b >/dev/null
xcrun simctl install "$UDID" "$APP"
SIMCTL_CHILD_CARTERCANVAS_CONTROL_PORT="$PORT" SIMCTL_CHILD_CARTERCANVAS_CONTROL_TOKEN="$TOKEN" \
  xcrun simctl launch --terminate-running-process "$UDID" com.carterware.cartercanvas
echo "UDID=$UDID"
echo "log: $(xcrun simctl get_app_container "$UDID" com.carterware.cartercanvas data)/Library/Application Support/CarterCanvas/Logs/cartercanvas.log"

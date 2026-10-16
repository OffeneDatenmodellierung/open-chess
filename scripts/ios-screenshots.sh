#!/usr/bin/env bash
# App Store screenshots of the iOS app, from the simulator.
#
#   scripts/ios-screenshots.sh <path to open-chess.app (a Debug simulator build)> <out dir>
#
# For the largest iPhone and the 13-inch iPad the installed Xcode has, it
# boots the simulator with Apple's 9:41 status bar, then launches the app
# once per scene with -OCScreenshot <scene> (ios/OpenChess/App/
# ScreenshotMode.swift) and saves <out>/<device>/<n>-<scene>.png, in
# portrait, at the size App Store Connect asks for.
set -euo pipefail

app=${1:?the .app}
out=${2:?the output directory}
bundle=org.offenedatenmodellierung.openchess
scenes=(home game analysis train realboard)
# A scene's first launch does more (the core, the computer's first move).
settle=${SETTLE_SECONDS:-12}

# The newest simulator whose name matches.
device_id() {
  xcrun simctl list devices available -j | python3 -c '
import json, re, sys
want = re.compile(sys.argv[1])
found = []
for runtime, devs in json.load(sys.stdin)["devices"].items():
    if "iOS" not in runtime:
        continue
    for d in devs:
        if want.search(d["name"]):
            found.append((runtime, d["name"], d["udid"]))
found.sort()
if not found:
    sys.exit("no simulator matching " + sys.argv[1])
print(found[-1][2], found[-1][1], sep="\t")
' "$1"
}

shoot() {
  local label=$1 pattern=$2
  local id name
  IFS=$'\t' read -r id name < <(device_id "$pattern")
  echo "== $label: $name ($id)"
  xcrun simctl boot "$id" 2>/dev/null || true
  xcrun simctl bootstatus "$id" -b
  xcrun simctl ui "$id" appearance dark
  xcrun simctl status_bar "$id" override --time "9:41" --dataNetwork wifi --wifiMode active \
    --wifiBars 3 --cellularMode active --cellularBars 4 --batteryState charged --batteryLevel 100
  xcrun simctl install "$id" "$app"
  mkdir -p "$out/$label"
  local n=1
  for scene in "${scenes[@]}"; do
    xcrun simctl terminate "$id" "$bundle" 2>/dev/null || true
    xcrun simctl launch "$id" "$bundle" -OCScreenshot "$scene" >/dev/null
    sleep "$settle"
    xcrun simctl io "$id" screenshot --type=png "$out/$label/$n-$scene.png"
    n=$((n + 1))
  done
  xcrun simctl shutdown "$id" || true
}

shoot iphone '^iPhone [0-9]+ Pro Max$'
shoot ipad '^iPad Pro 13-inch'
for f in "$out"/*/*.png; do
  echo "$f: $(sips -g pixelWidth -g pixelHeight "$f" | awk '/pixel/{printf "%s ", $2}')"
done

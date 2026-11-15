#!/bin/bash
# Builds Consilium (app bundle only, no DMG window) and stages it for the app's Update Now button.
# The staged app appears in one rename, so the button never offers a half-copied build.
# Usage: scripts/stage-update.sh        (from the consilium-ai folder; macOS)
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
DATA="${CONSILIUM_DATA:-$HOME/Library/Application Support/io.github.ag3nt47.consilium}"
APP=src-tauri/target/release/bundle/macos/Consilium.app
start=$(date +%s)
npm run app:build:mac
[ "$(stat -f %m "$APP/Contents/MacOS/consilium")" -ge "$start" ] || { echo "build produced no new app" >&2; exit 1; }
mkdir -p "$DATA/update"
rm -rf "$DATA/update/Consilium.app.tmp"
ditto "$APP" "$DATA/update/Consilium.app.tmp"
rm -rf "$DATA/update/Consilium.app"
mv "$DATA/update/Consilium.app.tmp" "$DATA/update/Consilium.app"
echo "Staged $(defaults read "$DATA/update/Consilium.app/Contents/Info.plist" CFBundleShortVersionString). Click Update Now in Consilium."

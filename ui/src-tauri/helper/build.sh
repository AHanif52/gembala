#!/bin/sh
# FR-20: helper app for the daemon (launched through `open`, see spawn_daemon in ui/src-tauri/src/daemon.rs).
# Signed as a bundle here, before Tauri seals the main bundle. Run from ui/ by `npm run sidecar`.
set -e
app="src-tauri/binaries/Gembala Daemon.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp src-tauri/helper/Info.plist "$app/Contents/"
cp ../target/release/gembala "$app/Contents/MacOS/gembala"
codesign --force -s "Gembala Dev" "$app" 2>/dev/null || codesign --force -s - "$app"

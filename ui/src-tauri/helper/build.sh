#!/bin/sh
# FR-20: helper app daemon (dijalankan lewat `open`, lihat ui/src-tauri/src/daemon.rs spawn_daemon).
# Di-sign sebagai bundle di sini, sebelum Tauri menyegel bundle utama. Dijalankan dari ui/ oleh `npm run sidecar`.
set -e
app="src-tauri/binaries/Gembala Daemon.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp src-tauri/helper/Info.plist "$app/Contents/"
cp ../target/release/gembala "$app/Contents/MacOS/gembala"
codesign --force -s "Gembala Dev" "$app" 2>/dev/null || codesign --force -s - "$app"

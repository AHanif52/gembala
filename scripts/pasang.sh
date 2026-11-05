#!/bin/sh
# Build a release and install it to /Applications, no network (stand-in for `brew upgrade` on one machine).
# The old bundle moves to the Trash, not overwritten: a running daemon (helper inside the bundle) keeps
# using its old files, so agents aren't disturbed. The new version is used after Gembala is closed and opened again.
# usage: scripts/pasang.sh
set -e
cd "$(dirname "$0")/../ui"
PATH="/opt/homebrew/opt/rustup/bin:$PATH" npm run tauri build -- --bundles app
new=../target/release/bundle/macos/Gembala.app
codesign -v --deep --strict "$new"
dst=/Applications/Gembala.app
tmp=/Applications/.Gembala-baru.app
rm -rf "$tmp"
ditto "$new" "$tmp"
if [ -d "$dst" ]; then mv "$dst" "$HOME/.Trash/Gembala-$(date +%Y%m%d-%H%M%S).app"; fi
mv "$tmp" "$dst"
echo "Terpasang di $dst. Tutup Gembala (Cmd+Q) lalu buka lagi untuk memakai versi baru."

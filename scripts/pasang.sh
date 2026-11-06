#!/bin/sh
# Build a release and install it to /Applications, no network (stand-in for `brew upgrade` on one machine).
# The old bundle is parked, not overwritten: a running daemon (helper inside the bundle) keeps
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
# Any `.app` left on disk (Trash, build output) stays registered under dev.gembala.app and notifications may take
# its old icon: the old bundle is parked without the .app extension, the build output is removed.
# ponytail: parked copies are never cleaned; delete ~/Library/Caches/dev.gembala.app/old by hand when it grows
ls=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
if [ -d "$dst" ]; then
  old="$HOME/Library/Caches/dev.gembala.app/old"
  mkdir -p "$old"
  "$ls" -u "$dst"
  mv "$dst" "$old/Gembala-$(date +%Y%m%d-%H%M%S)"
fi
mv "$tmp" "$dst"
"$ls" -u "$new"
rm -rf "$new"
"$ls" -f "$dst"
echo "Terpasang di $dst. Tutup Gembala (Cmd+Q) lalu buka lagi untuk memakai versi baru."

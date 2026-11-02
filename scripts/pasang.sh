#!/bin/sh
# Build rilis lalu pasang ke /Applications, tanpa jaringan (pengganti `brew upgrade` untuk satu mesin).
# Bundle lama dipindah ke Trash, bukan ditimpa: daemon (helper di dalam bundle) yang sedang jalan tetap
# memakai file lamanya, jadi agent tidak terganggu. Versi baru dipakai setelah Gembala ditutup lalu dibuka lagi.
# pakai: scripts/pasang.sh
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

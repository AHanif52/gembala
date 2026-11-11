#!/bin/sh
# Prebuilt release for Homebrew (FR-81): users get the signed app, no Rust or Node on their Mac.
# Builds Gembala.app, zips it, publishes GitHub release v<version> and writes the cask to the tap AHanif52/homebrew-gembala.
# Version = ui/src-tauri/tauri.conf.json; bump it first, the tag must be new.
# usage: scripts/rilis.sh            (scripts/rilis.sh --dry = build and check only, nothing published)
set -e
dry=${1:-}
root=$(cd "$(dirname "$0")/.." && pwd)
v=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['version'])" "$root/ui/src-tauri/tauri.conf.json")
section=$(awk -v h="## $v" '$0 == h { on = 1; next } /^## / { on = 0 } on' "$root/CHANGELOG.md")
set -- $(printf '%s\n' "$section" | awk '
  /^### English[[:space:]]*$/ { lang = "en"; en = 1; next }
  /^### Indonesia[[:space:]]*$/ { lang = "id"; id = 1; next }
  /^### / { lang = "" }
  lang == "en" && /^- +[^[:space:]]/ { en_text = 1 }
  lang == "id" && /^- +[^[:space:]]/ { id_text = 1 }
  END { print en, en_text, id, id_text }
')
[ "$1" = 1 ] && [ "$2" = 1 ] && [ "$3" = 1 ] && [ "$4" = 1 ] || { echo "CHANGELOG.md harus punya poin English dan Indonesia untuk $v."; exit 1; }
notes=$(printf '%s\n' "$section" | awk '/^### English[[:space:]]*$/ { on = 1; next } /^### / { on = 0 } on')
git -C "$root" diff --quiet HEAD || { echo "Ada perubahan belum di-commit."; exit 1; }
[ "$dry" = --dry ] || ! gh release view "v$v" -R AHanif52/gembala >/dev/null 2>&1 || { echo "v$v sudah dirilis, naikkan version dulu."; exit 1; }
# the home folder (= account name) stays out of the binaries' panic paths
export RUSTFLAGS="--remap-path-prefix=$HOME=~"
cd "$root/ui"
PATH="/opt/homebrew/opt/rustup/bin:$PATH" npm run tauri build -- --bundles app
app=$root/target/release/bundle/macos/Gembala.app
codesign -v --deep --strict "$app"
# helper/build.sh falls back to an ad-hoc signature without the certificate: never ship that (SEC-20 checks the signer)
for b in "$app" "$app/Contents/Helpers/Gembala Daemon.app"; do
  codesign -dvv "$b" 2>&1 | grep -qx "Authority=Gembala Dev" || { echo "$b tidak ditandatangani Gembala Dev."; exit 1; }
done
if grep -rlF "$HOME" "$app"; then echo "Path $HOME masih ada di bundle (file di atas)."; exit 1; fi
zip=$root/target/release/Gembala-$v-arm64.zip
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
sha=$(shasum -a 256 "$zip" | cut -d' ' -f1)
echo "$zip $sha"
[ "$dry" = --dry ] && exit 0
# release notes = English subsection, then the install line
gh release create "v$v" "$zip" -R AHanif52/gembala --target "$(git -C "$root" rev-parse HEAD)" --title "Gembala $v" \
  --notes "$(printf '%s\n\n%s' "$notes" "brew install ahanif52/gembala/gembala")"
tap=$(mktemp -d)
gh repo clone AHanif52/homebrew-gembala "$tap" -- -q
mkdir -p "$tap/Casks"
sed -e "s/@VERSION@/$v/" -e "s/@SHA256@/$sha/" "$root/scripts/gembala.rb.in" > "$tap/Casks/gembala.rb"
git -C "$tap" add Casks/gembala.rb
git -C "$tap" -c user.email=AHanif52@users.noreply.github.com -c user.name=AHanif52 commit -qm "gembala $v"
git -C "$tap" push -q
echo "Rilis v$v: brew install ahanif52/gembala/gembala"

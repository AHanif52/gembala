#!/bin/sh
# TECH 48: the app and its daemon helper must be signed with the hardened runtime, otherwise DYLD_INSERT_LIBRARIES
# can load code into them. usage: scripts/check-runtime.sh Gembala.app
app="$1"
for b in "$app" "$app/Contents/Helpers/Gembala Daemon.app"; do
  codesign -dvv "$b" 2>&1 | grep -q "flags=.*(.*runtime" || { echo "$b: no hardened runtime (codesign --options runtime)."; exit 1; }
done

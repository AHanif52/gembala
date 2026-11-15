#!/usr/bin/env bash
# FR-98 stage 2 (TECH 21): checks a built Linux .deb.
#   deb-check.sh contents <file.deb>  what the package ships (daemon, UI binary, desktop file, icon, polkit action)
#   deb-check.sh install  <file.deb>  clean ubuntu:24.04 container: apt install, daemon starts headless and answers
set -euo pipefail
mode=${1:?usage: deb-check.sh contents|install <file.deb>}
deb=$(realpath "${2:?usage: deb-check.sh contents|install <file.deb>}")

case $mode in
contents)
  dpkg-deb --info "$deb"
  list=$(dpkg-deb --contents "$deb")
  echo "$list"
  # dpkg-deb lists paths with or without a leading ./ depending on its version
  for p in ' (\./)?usr/bin/gembala$' ' (\./)?usr/bin/gembala-ui$' ' (\./)?usr/share/applications/.*\.desktop$' \
    ' (\./)?usr/share/icons/hicolor/.*/gembala-ui\.png$' ' (\./)?usr/share/polkit-1/actions/dev\.gembala\.app\.policy$'; do
    grep -Eq "$p" <<<"$list" || { echo "deb-check: missing $p" >&2; exit 1; }
  done
  ;;
install)
  docker run --rm -v "$deb:/pkg/gembala.deb:ro" ubuntu:24.04 bash -euxc '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q && apt-get install -yq /pkg/gembala.deb
    useradd -m tester
    su tester -c "
      set -eu
      export XDG_RUNTIME_DIR=/tmp/xdg-tester; mkdir -m 700 \$XDG_RUNTIME_DIR
      # no --version flag: usage + exit 2 proves the binary resolves and its libraries load
      gembala 2>&1 | grep -q \"^usage: gembala\"
      # fresh account (no ~/.config); the daemon idles out after ~10 s without a UI, so poll for the pane socket
      gembala daemon 2>/tmp/daemon.log & pid=\$!
      h=~/.config/gembala/hooks
      for _ in \$(seq 20); do [ -S \$XDG_RUNTIME_DIR/gembala/pane.sock ] && [ -f \$h/accounts.bash ] && break; sleep 0.5; done
      [ -S \$XDG_RUNTIME_DIR/gembala/pane.sock ] && [ -f \$h/gembala.md ] && [ -f \$h/accounts.bash ] || { cat /tmp/daemon.log; exit 1; }
      ! grep -q \"^gembala daemon:\" /tmp/daemon.log || { cat /tmp/daemon.log; exit 1; }
      kill \$pid
    "'
  ;;
*) echo "unknown mode $mode" >&2; exit 2 ;;
esac

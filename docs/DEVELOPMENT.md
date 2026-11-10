# Development notes

Numbers like FR-61, SEC-20, or "TECH section 17" in code refer to the repo owner's requirements documents, which aren't
public; the context you need lives in the issue. Workflow and PR rules are in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Structure

```
core/                    `gembala` binary: daemon + CLI (one binary, also the app's sidecar)
  src/main.rs            subcommands: daemon, hook, send, wait, read, reply, status, watch
  src/daemon.rs          ui.sock + pane.sock sockets, pane registry, attach, status, message engine (glue)
  src/pty.rs             spawns the login shell (setsid, clean env SEC-35), process busy/name/cwd/args
  src/grid.rs            VT grid (vt100) + attach snapshot + text for read/screen patterns
  src/config.rs          config.toml (toml_edit): per-key validation, set/profile/shortcuts, atomic write
  src/profile.rs         agent profiles, detection, launch command + hook flags + session, hook/screen-pattern status
  src/msg.rs             pure message rules: sanitizer, labels, send decision
  src/bus.rs             queue, history, HMAC audit log (SEC-11)
  src/git.rs             read-only git status/diff + secret redaction (SEC-12, 29)
  src/rundir.rs          private socket folder (SEC-1, 25)
  src/sign.rs            UI channel recognized by the peer's code signature (SEC-20)
  tests/daemon.rs        integration tests against a real daemon over the socket
ui/                      Tauri 2 + SvelteKit static + Svelte 5 + xterm.js (DOM renderer)
  src-tauri/src/lib.rs   daemon client (doesn't open PTYs), native menu + [keys], notifications, dialogs, paste
  src-tauri/src/daemon.rs  connects to / launches the daemon
  src/routes/+page.svelte  workspace → tab → split tree, sidebar, toolbar, cards, restore
  src/lib/               Tile (one xterm per pane), Settings, Changes, Messages, OutputView, layout (pure)
scripts/                 pasang.sh + real-app test tools in a separate run folder + config (don't touch user data)
```

User data: `~/Library/Application Support/Gembala/` (config.toml, layout.json, hooks/, audit/), sockets in
`$TMPDIR/gembala/`. Both can be redirected with `GEMBALA_CONFIG` and `GEMBALA_RUNDIR` (used by tests).

New UI-to-daemon methods must be registered in `ALLOWED` (`ui/src-tauri/src/lib.rs`); otherwise the call is silently
rejected.

## Run and test

You need Rust (`brew install rustup`; PATH `/opt/homebrew/opt/rustup/bin`) and Node.

```
export PATH=/opt/homebrew/opt/rustup/bin:$PATH
cargo test -p gembala                           # unit + daemon integration (~30 s)
cargo test -p ui                                # the app's Rust side
cargo clippy --workspace --all-targets -- -D warnings
cd ui && npm install --ignore-scripts && npm run check && npm test
npm run tauri build                             # release app: target/release/bundle/macos/Gembala.app (+ .dmg)
npm run tauri build -- --debug                  # debug app, for scripts/ below
```

Test tools that drive the real debug app (the secret goes through `GEMBALA_DEV_SECRET`, a path that only exists in
debug builds):

```
python3 -I scripts/restore_check.py target/debug/bundle/macos/Gembala.app   # reopen + new daemon
python3 -I scripts/lost_check.py target/debug/bundle/macos/Gembala.app      # daemon killed
python3 -I scripts/quit_check.py target/debug/bundle/macos/Gembala.app      # Cmd+Q, agents stay alive
python3 -I scripts/ram_ui.py target/debug/bundle/macos/Gembala.app 4        # RAM with 4 panes (needs the screen on)
python3 -I scripts/ukur.py target/release/gembala $TMPDIR/gbl-ukur          # daemon latency + throughput
```

Always install npm packages with `--ignore-scripts` (SEC-16).

## Processes Gembala runs

| Name in Activity Monitor | Role |
|---|---|
| `gembala-ui` (or "Gembala") | The app window |
| `tauri://localhost` | WebKit view: terminal and sidebar |
| WebKit GPU and Networking | Built-in WebKit parts that draw and load the view from inside the app |
| `gembala` | Pane-keeping daemon: shells and agents run under it |
| `zsh`, `claude`, `codex`, … | Processes running in panes |

- The CSP in `ui/src-tauri/tauri.conf.json` only allows the app's internal channel; the daemon only opens Unix sockets
  in a `0700` folder.
- The daemon stays alive after the window closes only while some pane is still running a process (FR-20); the macOS
  notification says why. Otherwise it stops by itself within a few seconds.

## Gotchas we've already hit

- WKWebView doesn't forward many Cmd combinations to JS keydown: all shortcuts go through the native menu;
  Cmd+Shift+Enter is swallowed by WebKit before the menu, so it's caught by keydown instead (with a 150 ms guard).
- Release builds are signed "Gembala Dev" (`bundle.macOS.signingIdentity`, a self-signed certificate in the login
  keychain): designated requirement = identifier + certificate hash, the same on every build. App Keychain items without
  a TeamIdentifier are bound to the build's cdhash, so "Always Allow" doesn't survive across builds. That's why the
  daemon recognizes the UI channel from the peer's signature (`core/src/sign.rs`). Test tools use the debug build +
  `GEMBALA_DEV_SECRET`.
- Releases (FR-63): bump the version, push tag `v<version>`; `.github/workflows/release.yml` imports the same
  certificate from repo secrets and runs `scripts/rilis.sh` (secrets listed at the top of the workflow). `rilis.sh` on
  the Mac still works as the fallback. Both refuse a bundle not signed by "Gembala Dev" (the helper falls back to ad hoc).
- An app opened from inside an agent session inherits that session's env: it's scrubbed in `pty::scrub_env` (SEC-35).
- The std `Mutex` isn't reentrant: don't hold `panes` while calling a function that locks it again.
- RAM: the xterm DOM renderer is lighter and flatter per pane than WebGL.
- The daemon stays alive after the UI closes. A new UI build connected to an old daemon build shows a
  "failed to connect to daemon: unknown_method" error: quit Gembala and open it again so the new daemon runs (live panes
  die with it). `scripts/pasang.sh` moves the old bundle to the Trash, so the old daemon can keep running until the app
  is reopened.
- Claude Code's Bash tool creates a new process session (setsid): pane identity has to go through the parent process
  chain, not just the peer's `getsid` (SEC-3, `pane_of_pid`).
- Manual agent detection uses the process file name (`proc_pidpath`); agents wrapped in an interpreter (process name
  `node`/`bun`) aren't detected.
- A copy of `/bin/sleep` gets killed by macOS (platform signature): the fake agent in tests is a copy of the `gembala`
  binary named `omp` running `watch`.
- A freshly updated Codex rejects third-party plugin hooks ("plugin contents changed; hook approval required"): not
  caused by Gembala; re-approve the hook in Codex.

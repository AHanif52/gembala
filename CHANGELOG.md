# Changelog

`scripts/rilis.sh` puts the section of the version being released on its GitHub release page.

## 0.1.8

- Safety fix for Telegram permission buttons: Claude Code 2.1.311 asks with 4 options in a Bash dialog, and button "3" used to switch the session to auto mode. Buttons now come from the options on screen with their own words ("4. No"); options that change the permission mode never get a button, and an unknown dialog only gets Esc.
- First start asks "Do you usually use a terminal?" and picks Simple or Full mode (Settings › General). Simple uses everyday words and folds technical details.
- "+ Ask an agent" (⌘N) at the top of the sidebar. The welcome card opens each agent you have, or helps install one (types the install command into a new terminal tab for you to run).
- A sentence typed into the terminal by mistake ("command not found") offers to send it to an agent instead.
- The Ask an agent dialog starts with "I steer" for new users and shows agents by name.
- Approval cards in a pane stack instead of covering each other; the message drawer can answer "open an agent" requests too.

## 0.1.7

- Agents can close the helper panes they opened (`gembala close <pane>`), and a helper that sits idle 15 minutes after its reply closes by itself (Settings › General). Fewer idle agents eating RAM.
- Harness: a run that finishes closes its stage panes; a stopped or failed run keeps them, with a "Close stage panes" button.
- The Telegram command menu follows the installed version (`/to <pane> <text>` sends straight to a pane).
- Codex in auto mode uses its own reviewer (`--approve-for-me`) instead of never asking.
- Codex 0.167 and Oh My Pi 18.18 are marked tested; New task reminds you that a new folder asks for trust first.

## 0.1.6

- Codex panes get a status line like Claude's (model, context, 5-hour and weekly limits as bars).
- Telegram: permission buttons for Codex, the question shows on Claude's permission card, `/exit` says the agent left, a folder-trust dialog shows as "needs you".
- Tabs whose panes are gone close properly; Harness fixes (Codex stages, folder trust, fallback models).

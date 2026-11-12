/// FR-56: a resume command from layout.json (untrusted file) is only used when it exactly matches one of the shapes
/// the daemon builds: agent name (Claude may be `claude-<account>`, FR-61) + flag + id without shell characters. Otherwise the card still shows without a command.
const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const RESUME = new RegExp(
  `^(claude(-[a-z0-9-]{1,32})? --resume ${UUID}|codex resume (${UUID}|--last)|opencode --continue|opencode --session [A-Za-z0-9_-]{1,80}|omp --continue|omp --resume [A-Za-z0-9-]{1,80}|commandcode --continue|commandcode --resume [A-Za-z0-9_][A-Za-z0-9_-]{0,79})$`,
);

export const safeResume = (cmd: unknown): string | null => (typeof cmd === "string" && RESUME.test(cmd) ? cmd : null);

/// FR-59 gray suggestion in the input editor. The finished session card's resume command (FR-56) goes first, so typing
/// `claude` in a pane whose session just ended offers `claude --resume <id>` instead of an old `claude -c` from history;
/// then the app's own session history (newest first), then the user's zsh history.
export const suggestion = (typed: string, resume: string | null | undefined, history: string[], zsh: string[]): string => {
  if (!typed || typed.includes("\n") || typed.startsWith("@")) return "";
  const hit = [...(resume ? [resume] : []), ...[...history].reverse(), ...zsh].find(
    (h) => h.startsWith(typed) && h.length > typed.length && !h.includes("\n"),
  );
  return hit ? hit.slice(typed.length) : "";
};

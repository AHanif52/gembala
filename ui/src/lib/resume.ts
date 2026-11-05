/// FR-56: a resume command from layout.json (untrusted file) is only used when it exactly matches one of the shapes
/// the daemon builds: agent name (Claude may be `claude-<account>`, FR-61) + flag + id without shell characters. Otherwise the card still shows without a command.
const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const RESUME = new RegExp(
  `^(claude(-[a-z0-9-]{1,32})? --resume ${UUID}|codex resume (${UUID}|--last)|opencode --continue|opencode --session [A-Za-z0-9_-]{1,80}|omp --continue|omp --resume [A-Za-z0-9-]{1,80})$`,
);

export const safeResume = (cmd: unknown): string | null => (typeof cmd === "string" && RESUME.test(cmd) ? cmd : null);

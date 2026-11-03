/// FR-56: perintah lanjut dari layout.json (file tidak tepercaya) hanya dipakai bila persis salah satu bentuk
/// yang dibuat daemon: nama agent (Claude boleh `claude-<akun>`, FR-61) + flag + id tanpa karakter shell. Selain itu kartu tetap tampil tanpa perintah.
const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const RESUME = new RegExp(
  `^(claude(-[a-z0-9-]{1,32})? --resume ${UUID}|codex resume (${UUID}|--last)|opencode --continue|opencode --session [A-Za-z0-9_-]{1,80}|omp --continue|omp --resume [A-Za-z0-9-]{1,80})$`,
);

export const safeResume = (cmd: unknown): string | null => (typeof cmd === "string" && RESUME.test(cmd) ? cmd : null);

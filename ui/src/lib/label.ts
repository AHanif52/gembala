/// FR-69: label pane dari judul terminal agent. Penanda putar, ikon agent, dan judul generik (nama agent,
/// nama folder) dibuang; "" = tidak ada label yang berguna.
const GENERIC = ["claude code", "claude", "codex", "opencode", "omp", "oh my pi"];

export function cleanTitle(raw: string | undefined, folder = ""): string {
  const t = (raw ?? "")
    .replace(/^OC\s*\|\s*/, "")
    .replace(/^[\s⠀-⣿✳✻✶✢·*•π>|]+/u, "")
    .trim();
  const low = t.toLowerCase();
  if (!t || GENERIC.includes(low) || (folder && low === folder.toLowerCase())) return "";
  return t.length > 60 ? `${t.slice(0, 59)}…` : t;
}

/// FR-69: pane label from the agent's terminal title. Spinner marks, agent icons, and generic titles (agent name,
/// folder name) are dropped; "" = no useful label.
const GENERIC = ["claude code", "claude", "codex", "opencode", "omp", "oh my pi", "command code", "commandcode", "cmd", "cmdc"];

export function cleanTitle(raw: string | undefined, folder = ""): string {
  const t = (raw ?? "")
    .replace(/^OC\s*\|\s*/, "")
    .replace(/^[\s⠀-⣿✳✻✶✢·*•π⌘>|]+/u, "")
    .trim();
  const low = t.toLowerCase();
  if (!t || GENERIC.includes(low) || (folder && low === folder.toLowerCase())) return "";
  return t.length > 60 ? `${t.slice(0, 59)}…` : t;
}

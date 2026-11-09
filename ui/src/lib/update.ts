/// FR-82: result of the daemon's GitHub release check (`update.status` / `update.changed`)
export type Upd = { latest?: string; published?: string; url?: string; notes?: string; at?: number; failed_at?: number; check?: boolean; brew?: boolean };

/// "0.1.10" is newer than "0.1.9"; anything that isn't a dotted number is never newer
export function newer(latest: string | undefined, cur: string): boolean {
  const num = (v: string) => (/^\d+(\.\d+)*$/.test(v) ? v.split(".").map(Number) : null);
  const [a, b] = [num(latest ?? ""), num(cur)];
  if (!a || !b) return false;
  for (let i = 0; i < Math.max(a.length, b.length); i++) if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) > (b[i] ?? 0);
  return false;
}

/// what the user runs: Homebrew install (FR-81) or a source checkout (`pasang.sh`)
export const upgradeCmd = (u: Upd | null) => (u?.brew ? "brew upgrade gembala" : "git pull && scripts/pasang.sh");

/// release notes as a few plain lines (markdown marks dropped; never rendered as HTML)
export const noteLines = (notes = "") =>
  notes
    .split("\n")
    .map((l) => l.replace(/^[\s#*•-]+/, "").replace(/[*`]/g, "").trim())
    .filter(Boolean)
    .slice(0, 5);

import { translate } from "./i18n/index.ts";

export type UsageWindow = { kind: "5h" | "week" | "month" | "day"; used_pct: number; resets_at: number | null; model: string | null };
export type UsageRow = {
  agent: "claude" | "codex" | "omp" | "opencode" | "gemini";
  login: string;
  login_name: string;
  windows: UsageWindow[];
  source: "codex_footer" | "codex_app_server" | "omp_cli" | "gemini_footer" | "claude_statusline";
  read_at: number;
  limited: boolean;
};
export type UsageRing = { key: string; agent: UsageRow["agent"]; login: string; loginName: string; row?: UsageRow; window?: UsageWindow; left: number | null; tone: "red" | "yellow" | "green" | "unknown"; stale: boolean };
type Account = { id: string; agent: string; name: string };
type Active = { agent: string; login: string };

const AGENTS = ["claude", "codex", "omp", "opencode", "gemini"] as const;
const index = (agent: string) => AGENTS.indexOf(agent as UsageRow["agent"]);
export const remaining = (w: UsageWindow) => Math.max(0, Math.min(100, 100 - w.used_pct));
export const validWindows = (row: UsageRow, now: number) => (Array.isArray(row.windows) ? row.windows : [])
  .filter((w) => w && Number.isFinite(w.used_pct) && w.used_pct >= 0 && w.used_pct <= 100 && (w.resets_at === null || Number.isFinite(w.resets_at) && w.resets_at > now));
export const tightest = (row: UsageRow, now: number) => validWindows(row, now).reduce<UsageWindow | undefined>((best, w) => !best || remaining(w) < remaining(best) ? w : best, undefined);
export const toneOf = (left: number | null, limited: boolean): UsageRing["tone"] =>
  limited ? "red" : left === null ? "unknown" : left <= 5 ? "red" : left <= 20 ? "yellow" : "green";

// Keep order within a color band after the first sort; changing percentages won't move rings under the pointer.
export function orderRings(rings: UsageRing[], previous: string[] = []): UsageRing[] {
  const rank = { red: 0, yellow: 1, green: 2, unknown: 3 };
  return [...rings].sort((a, b) => rank[a.tone] - rank[b.tone] ||
    (previous.includes(a.key) && previous.includes(b.key) ? previous.indexOf(a.key) - previous.indexOf(b.key) :
      a.tone === "unknown" ? index(a.agent) - index(b.agent) || a.loginName.localeCompare(b.loginName) :
      (a.left ?? 101) - (b.left ?? 101) || index(a.agent) - index(b.agent)));
}

export function buildRings(installed: string[], accounts: Account[], active: Active[], rows: UsageRow[], now: number): UsageRing[] {
  const by = new Map<string, UsageRing>();
  const add = (agent: string, login: string, loginName: string) => {
    if (index(agent) < 0) return;
    const key = `${agent}:${login}`;
    if (!by.has(key)) by.set(key, { key, agent: agent as UsageRow["agent"], login, loginName, left: null, tone: "unknown", stale: false });
  };
  for (const agent of installed) add(agent, "utama", "Utama");
  for (const a of active) add(a.agent, a.login || "utama", accounts.find((x) => x.agent === a.agent && x.id === a.login)?.name ?? "Utama");
  for (const row of rows) {
    if (!row || typeof row.agent !== "string" || typeof row.login !== "string") continue;
    add(row.agent, row.login, row.login_name || accounts.find((a) => a.agent === row.agent && a.id === row.login)?.name || "Utama");
    const ring = by.get(`${row.agent}:${row.login}`);
    if (!ring) continue;
    if (row.login_name) ring.loginName = row.login_name;
    const window = tightest(row, now);
    ring.row = row;
    ring.window = window;
    ring.left = window ? remaining(window) : null;
    ring.stale = !!row.windows?.length && !window;
    ring.tone = toneOf(ring.left, row.limited && !ring.stale);
  }
  return [...by.values()];
}

export function relativeTime(unixSeconds: number, now: number, lang: "id" | "en"): string {
  const minutes = Math.max(0, Math.ceil((unixSeconds - now) / 60));
  if (minutes < 1) return translate(lang, "usage.soon");
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor(minutes % 1440 / 60);
  const mins = minutes % 60;
  const parts = [days && translate(lang, "usage.dayUnit", { n: days }), hours && translate(lang, "usage.hourUnit", { n: hours }), mins && translate(lang, "usage.minuteUnit", { n: mins })];
  return `${parts.filter(Boolean).slice(0, 2).join(" ")} ${translate(lang, "usage.after")}`;
}


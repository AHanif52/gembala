/// Codex footer (`tui.status_line`, set per launch by the daemon in addon.rs) read back for the pane overlay, shown
/// like Claude's status: bars are the share USED. Only text Codex already drew is read; no account data or session logs.
export type CodexStatus = {
  model: string;
  effort: string;
  context: number; // % used
  fiveHour?: number; // % used; absent without plan limits (API key) or before Codex knows them
  weekly?: number;
  window?: string;
  tokens?: string;
  dir?: string;
};

const EFFORT = /^(minimal|none|low|medium|high|xhigh|max)$/i;
const pct = (n: string) => (Number(n) <= 100 ? Number(n) : undefined);

export function codexStatus(line: string): CodexStatus | null {
  const parts = line.trim().split(/\s+·\s+/);
  // Display names can have spaces ("GPT-6.1 Terra medium"): more than one word only with an effort at the end
  const words = parts[0].split(/\s+/);
  const effort = words.length > 1 ? words.pop()! : "";
  const model = words.join(" ");
  if (!words.every((w) => /^[\w.:/@-]+$/.test(w)) || (effort && !EFFORT.test(effort)) || words.length > 3) return null;
  const s: CodexStatus = { model, effort, context: -1 };
  for (const p of parts.slice(1)) {
    let m: RegExpMatchArray | null;
    if ((m = p.match(/^Context (\d{1,3})% (left|used)$/i))) s.context = pct(m[2] === "used" ? m[1] : String(100 - Number(m[1]))) ?? -1;
    else if ((m = p.match(/^(?:5h|5-hour) (\d{1,3})% left$/i))) s.fiveHour = 100 - Number(m[1]);
    else if ((m = p.match(/^weekly (\d{1,3})% left$/i))) s.weekly = 100 - Number(m[1]);
    else if (/^[\d.]+[KM] window$/i.test(p)) s.window = p;
    else if (/^[\d.]+[KM]? (?:used|tokens)$/i.test(p)) s.tokens = p;
    else s.dir = p; // current-dir is the last item Gembala asks for
  }
  const ok = (n?: number) => n === undefined || (n >= 0 && n <= 100);
  return s.context >= 0 && s.context <= 100 && ok(s.fiveHour) && ok(s.weekly) ? s : null;
}

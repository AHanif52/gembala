// Harness UI helpers (FR-26, FR-73). Pure: the daemon decides, these only shape what the screens show.

export type HStep = {
  id: string; name: string; kind: string; human: boolean; shell: boolean; gate: boolean; agents: string[]; output: string; inputs?: string[];
  pane: number | null; pane_name: string | null; pane_agent: string | null; message: string | null;
};
export type Snap = {
  run: string; recipe: string; name: string; workspace: string; cwd: string; started_at: number; saver: boolean;
  state: "idle" | "running" | "gate" | "taken_over" | "done" | "halted";
  gate: number | null; current: number | null; halt: string | null; halt_step: number | null;
  round: number; max_rounds: number; steps: HStep[]; trace: { at: number; code: string; step: number | null; agent: string | null }[];
  /// every pane the run opened, fallbacks' replaced ones too (FR-86)
  panes?: number[];
  /// FR-94: the task's criteria and what proves each (daemon-computed)
  criteria?: Criterion[];
};
export type View = "done" | "now" | "gate" | "failed" | "todo";

/// FR-73 / P4: the test command is the `Test:` line of the workspace brief; the brief stays the single source
export function testCommand(brief?: string | null): string {
  const m = /^[ \t]*Test:[ \t]*(\S.*)$/im.exec(brief ?? "");
  return m ? m[1].trim().slice(0, 500) : "";
}

export const live = (s?: Snap | null) => !!s && s.state !== "done" && s.state !== "halted";

/// where each stage stands on the flow view
export function stepView(s: Snap, i: number): View {
  if (s.state === "done") return "done";
  if (s.state === "gate") return i < (s.gate ?? 0) ? "done" : i === s.gate ? "gate" : "todo";
  if (s.state === "halted") {
    // a stage that never got a step recorded (stopped before any) fails at the first one
    const at = s.halt_step ?? Math.max(0, ...s.trace.map((e) => e.step ?? 0));
    return i < at ? "done" : i === at ? "failed" : "todo";
  }
  const cur = s.current ?? 0;
  return i < cur ? "done" : i === cur ? "now" : "todo";
}

/// FR-99 P3: the stage a stopped run is stuck at: the daemon's `halt_step`, else the last check that did not pass
export function stuckStep(s: Snap): number | null {
  if (s.halt_step != null) return s.halt_step;
  const e = [...s.trace].reverse().find((x) => (x.code === "fail" || x.code === "shell_fail") && x.step != null);
  return e?.step ?? null;
}

/// the file with what that stage found ("Lihat temuan"): a test stage's log, else the stage's own output; none = no button
export function findingFile(s: Snap): string | null {
  const i = stuckStep(s);
  const st = i == null ? undefined : s.steps[i];
  return !st || st.human ? null : st.shell ? "test.log" : st.output || null;
}

/// "Jalanin ulang" can begin at any stage but Ship; the default is Execute (else the first)
export const restartStages = (s: Snap) => s.steps.flatMap((st, i) => (st.human ? [] : [i]));
export const restartDefault = (s: Snap) => {
  const i = s.steps.findIndex((st) => st.id === "execute");
  return i >= 0 ? i : (restartStages(s)[0] ?? 0);
};

/// stages that have a pane, in recipe order; the tab shows these
export const stagePanes = (s: Snap) => s.steps.flatMap((st, i) => (st.pane == null ? [] : [{ pane: st.pane, step: i, name: st.pane_name ?? "", agent: st.pane_agent ?? "" }]));

export const canStart = (items?: { ok: boolean; blocking: boolean }[]) => !!items && items.every((i) => i.ok || !i.blocking);

/// "m:ss" since the run started, from trace seconds
export const clock = (secs: number) => `${Math.floor(secs / 60)}:${String(Math.floor(secs % 60)).padStart(2, "0")}`;

/// FR-77 (H5): an Atur pakai AI draft the page started; `ready` = the agent wrote the file
export type AiDraft = { kind: "recipe" | "config"; draft: string; id: string; ask: string; pane: number; who: string; ready?: boolean };
/// what "Rumuskan" asks the page to start: a recipe or config.toml draft, or an AGENTS.md/CLAUDE.md edit by the agent itself
export type AiAsk = { target: "recipe" | "config" | "agent"; id: string; ask: string; profile: string; section: string; file: string };

/// FR-94 (TECH 47.2): the task text with its criteria as the bullets the daemon reads under a "Kriteria" heading. A criterion
/// that talks about tests names `test.log` as its proof; any other has none yet, so it shows "untested" until it has one.
export function withCriteria(task: string, criteria: string[]): string {
  const items = criteria.map((c) => c.replace(/\s+/g, " ").trim()).filter(Boolean).slice(0, 20);
  if (!items.length) return task;
  const line = (c: string) => `- ${c.replace(/\[bukti:/gi, "(bukti:")}${/\b(tes|test|lulus)\b/i.test(c) ? " [bukti: test.log]" : ""}`;
  return `${task.trimEnd()}\n\n## Kriteria\n${items.map(line).join("\n")}\n`;
}

export type Criterion = { text: string; proof: string[]; status: "proven" | "failed" | "untested" };
/// counts for the report header: "1 proven · 1 failed · 2 untested"
export const criteriaCount = (c: Criterion[]) => ({ proven: c.filter((x) => x.status === "proven").length, failed: c.filter((x) => x.status === "failed").length, untested: c.filter((x) => x.status === "untested").length });

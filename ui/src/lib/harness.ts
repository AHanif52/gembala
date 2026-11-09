// Harness UI helpers (FR-26, FR-73). Pure: the daemon decides, these only shape what the screens show.

export type HStep = {
  id: string; name: string; kind: string; human: boolean; shell: boolean; gate: boolean; agents: string[]; output: string;
  pane: number | null; pane_name: string | null; pane_agent: string | null; message: string | null;
};
export type Snap = {
  run: string; recipe: string; name: string; workspace: string; cwd: string; started_at: number;
  state: "idle" | "running" | "gate" | "taken_over" | "done" | "halted";
  gate: number | null; current: number | null; halt: string | null; halt_step: number | null;
  round: number; max_rounds: number; steps: HStep[]; trace: { at: number; code: string; step: number | null; agent: string | null }[];
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

/// stages that have a pane, in recipe order; the tab shows these
export const stagePanes = (s: Snap) => s.steps.flatMap((st, i) => (st.pane == null ? [] : [{ pane: st.pane, step: i, name: st.pane_name ?? "", agent: st.pane_agent ?? "" }]));

export const canStart = (items?: { ok: boolean; blocking: boolean }[]) => !!items && items.every((i) => i.ok || !i.blocking);

/// "m:ss" since the run started, from trace seconds
export const clock = (secs: number) => `${Math.floor(secs / 60)}:${String(Math.floor(secs % 60)).padStart(2, "0")}`;

/// FR-77 (H5): an Atur pakai AI draft the page started; `ready` = the agent wrote the file
export type AiDraft = { kind: "recipe" | "config"; draft: string; id: string; ask: string; pane: number; who: string; ready?: boolean };
/// what "Rumuskan" asks the page to start: a recipe or config.toml draft, or an AGENTS.md/CLAUDE.md edit by the agent itself
export type AiAsk = { target: "recipe" | "config" | "agent"; id: string; ask: string; profile: string; section: string; file: string };

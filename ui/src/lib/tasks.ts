// FR-92: the task panel's view of `task.list`. Pure, so `node --test` can check it (TECH 50).

export type KidState = "running" | "waiting" | "done" | "failed" | "held" | "limit" | "stuck";

export type Kid = {
  id: string;
  from: string;
  to: string;
  /// 0 = no such pane (closed, or not opened yet)
  to_pane: number;
  agent: string;
  title: string;
  at: number;
  state: KidState;
  /// why it failed, a code from the daemon (`pane_closed`, `expired`, a refusal code), or "gate" for a harness stage waiting for the user
  why: string | null;
  reset_at?: number | null;
  result: string | null;
  /// what the answer says itself: done / partial / blocked / unspecified
  outcome: string | null;
  stage?: string;
};

export type Task = {
  id: string;
  /// `flow` = a harness run in progress
  kind: "task" | "flow";
  title: string;
  owner: { name: string; pane: number; agent: string };
  at: number;
  /// unix seconds when it was finished, null while open
  done: number | null;
  implicit?: boolean;
  waiting_on?: string[];
  kids: Kid[];
  state?: string;
  workspace?: string;
};

/// the pill class of a state (mockup L1/L3)
export const PILL: Record<KidState, string> = { running: "run", waiting: "wt", done: "ok", failed: "bad", held: "hold", limit: "lim", stuck: "wt" };

export const isOpen = (t: Task) => t.done == null;
export const openCount = (ts: Task[]) => ts.filter(isOpen).length;
export const split = (ts: Task[]) => ({ open: ts.filter(isOpen), done: ts.filter((t) => !isOpen(t)) });

/// kids the owner has no answer from yet (a failed one still counts: the owner has to hear about it), and how many of them
/// are failed, held or at a limit (the three L3 words)
export function tally(t: Task) {
  const n = (s: KidState) => t.kids.filter((k) => k.state === s).length;
  const live = t.kids.filter((k) => k.state !== "done" && k.state !== "stuck");
  return { waiting: live.length, failed: n("failed"), held: n("held"), limit: n("limit"), names: live.map((k) => k.to) };
}

/// the kid's own sentence under its name: the title while it works, the reason when it did not make it
export const whyKey = (k: Kid) => (k.why === "gate" ? "task.why.gate" : k.state === "failed" ? `task.why.${k.why ?? "other"}` : "");

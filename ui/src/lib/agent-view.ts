/// PLAN-CC-PANE phase 3: agent-neutral view of a structured pane. An adapter (commandcode.ts) turns CLI events into
/// these actions; the component only renders. Mutates in place so a Svelte `$state` view updates fine-grained.

export type ToolStatus = "queued" | "running" | "done" | "error" | "denied" | "blocked" | "unknown";
export type Tool = { id: string; name: string; input: unknown; status: ToolStatus; output?: string; partial?: string; note?: string };
export type Notice = { kind: "notice"; level: "info" | "warn" | "error"; key: string; params?: Record<string, string | number> };
export type Block = { kind: "text"; text: string } | { kind: "thinking"; text: string; live?: boolean } | { kind: "tool"; id: string } | Notice;
export type Usage = { input: number | null; output: number | null; cacheRead: number | null; cacheWrite: number | null };
export type Result = { usage: Usage; durationMs: number | null; error: string | null };
export type Turn =
  | { role: "user"; text: string; at: number }
  | { role: "agent"; runId: string; at: number; blocks: Block[]; draft: Block[]; state: string; result?: Result; exitCode?: number | null; stderr?: string; diagnostic?: string | null };
/// `status`: idle | starting | thinking | writing | tool | done | interrupted | error | auth | rate_limit | … (daemon state)
export type View = {
  epoch: string | null;
  seq: number;
  status: string;
  runId: string | null;
  sessionId: string | null;
  model: string | null;
  context: number | null; // size of the last request (prompt + output), not a session sum
  usage: Usage | null; // sum of each run's final `result` usage
  turns: Turn[];
  tools: Record<string, Tool>;
  truncated: boolean;
  access: "restricted" | "yolo"; // the daemon's effective policy, never decided here
};
export type Action =
  | { t: "user"; text: string; at: number }
  | { t: "run"; runId: string; at: number }
  | { t: "text"; delta: string }
  | { t: "thinking"; delta: string }
  | { t: "thinking_end"; text: string }
  | { t: "message"; blocks: Block[]; final: boolean }
  | ({ t: "tool"; id: string } & Partial<Omit<Tool, "id">>)
  | { t: "restart" }
  | { t: "request"; model?: string | null; context?: number | null }
  | { t: "session"; id: string }
  | ({ t: "notice" } & Omit<Notice, "kind">)
  | { t: "result"; usage: Usage; durationMs: number | null; error: string | null; finalText: string | null }
  | { t: "end"; state: string; exitCode: number | null }
  | { t: "access"; mode: "restricted" | "yolo" };

export const BUSY = ["starting", "thinking", "writing", "tool"];
const ACTIVE: ToolStatus[] = ["queued", "running"];

export const emptyView = (): View => ({ epoch: null, seq: 0, status: "idle", runId: null, sessionId: null, model: null, context: null, usage: null, turns: [], tools: {}, truncated: false, access: "restricted" });

/// the turn of the current run; events of a run the view never saw start (attached mid-run) get a turn of their own
function agentTurn(v: View): Extract<Turn, { role: "agent" }> {
  const last = v.turns.at(-1);
  if (last?.role === "agent" && last.runId === (v.runId ?? "")) return last;
  const turn: Turn = { role: "agent", runId: v.runId ?? "", at: Date.now(), blocks: [], draft: [], state: "starting" };
  v.turns.push(turn);
  return v.turns.at(-1) as Extract<Turn, { role: "agent" }>;
}

const hasTool = (blocks: Block[], id: string) => blocks.some((b) => b.kind === "tool" && b.id === id);
const sum = (a: number | null, b: number | null) => (a === null && b === null ? null : (a ?? 0) + (b ?? 0));

export function apply(v: View, a: Action): void {
  if (a.t === "user") return void v.turns.push({ role: "user", text: a.text, at: a.at });
  if (a.t === "run") {
    v.runId = a.runId;
    v.status = "starting";
    v.turns.push({ role: "agent", runId: a.runId, at: a.at, blocks: [], draft: [], state: "starting" });
    return;
  }
  if (a.t === "session") return void (v.sessionId = a.id);
  if (a.t === "access") return void (v.access = a.mode);
  if (a.t === "request") {
    if (a.model) v.model = a.model;
    if (a.context !== undefined && a.context !== null) v.context = a.context;
    return;
  }
  const turn = agentTurn(v);
  switch (a.t) {
    case "text":
    case "thinking": {
      const kind = a.t;
      const last = turn.draft.at(-1);
      if (last?.kind === kind) last.text += a.delta;
      else turn.draft.push(kind === "text" ? { kind, text: a.delta } : { kind, text: a.delta, live: true });
      if (BUSY.includes(v.status)) v.status = kind === "text" ? "writing" : "thinking";
      return;
    }
    case "thinking_end": {
      const block = turn.draft.findLast((b) => b.kind === "thinking");
      if (block?.kind === "thinking") Object.assign(block, { text: a.text || block.text, live: false });
      else turn.draft.push({ kind: "thinking", text: a.text });
      return;
    }
    case "message": {
      // `content` is a snapshot of the whole message: it replaces the draft, never appends to it
      const blocks = a.blocks.filter((b) => b.kind !== "tool" || !hasTool(turn.blocks, b.id));
      if (a.final) (turn.blocks.push(...blocks), (turn.draft = []));
      else turn.draft = blocks;
      return;
    }
    case "tool": {
      const { t: _, id, ...fields } = a;
      const tool = (v.tools[id] ??= { id, name: "", input: null, status: "queued" });
      for (const [k, x] of Object.entries(fields)) if (x !== undefined) (tool as Record<string, unknown>)[k] = x;
      if (!hasTool(turn.blocks, id) && !hasTool(turn.draft, id)) turn.blocks.push({ kind: "tool", id });
      if (BUSY.includes(v.status) && a.status) v.status = ACTIVE.includes(a.status) ? "tool" : "thinking";
      return;
    }
    case "restart":
      // a discarded stream drops only the visible draft; finished tools and earlier rounds stay
      turn.draft = turn.draft.filter((b) => b.kind === "tool");
      return;
    case "notice": {
      const { t: _, ...n } = a;
      turn.blocks.push({ kind: "notice", ...n });
      return;
    }
    case "result": {
      turn.result = { usage: a.usage, durationMs: a.durationMs, error: a.error };
      const u = v.usage;
      v.usage = u
        ? { input: sum(u.input, a.usage.input), output: sum(u.output, a.usage.output), cacheRead: sum(u.cacheRead, a.usage.cacheRead), cacheWrite: sum(u.cacheWrite, a.usage.cacheWrite) }
        : { ...a.usage };
      // finalText only when no assistant text was shown at all (no duplicate of the last message)
      const shown = [...turn.blocks, ...turn.draft].some((b) => b.kind === "text" && b.text.trim());
      if (!shown && a.finalText) turn.blocks.push({ kind: "text", text: a.finalText });
      return;
    }
    case "end":
      turn.blocks.push(...turn.draft.filter((b) => b.kind !== "tool" || !hasTool(turn.blocks, b.id)));
      turn.draft = [];
      for (const b of turn.blocks) if (b.kind === "thinking") b.live = false;
      // tools still waiting when the run stopped: the outcome is unknown, never "done"
      if (a.state !== "done") for (const b of turn.blocks) if (b.kind === "tool" && ACTIVE.includes(v.tools[b.id]?.status)) v.tools[b.id].status = "unknown";
      turn.state = a.state;
      turn.exitCode = a.exitCode;
      v.status = a.state;
      return;
  }
}

/// statusline numbers: missing data is "—", never 0
export const num = (n: number | null | undefined, lang: string) => (typeof n === "number" ? n.toLocaleString(lang === "id" ? "id-ID" : "en-US") : "—");

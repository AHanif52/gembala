/// PLAN-CC-PANE phase 3: Command Code `cc.event` envelopes and `cc.attach` snapshots → agent-view actions.
/// Field names follow the CLI 1.88.5 bundle (fixtures in core/tests/fixtures/commandcode); unknown events stay visible
/// as "not supported yet" and never change access or success.
import { apply, emptyView, type Action, type Block, type ToolStatus, type Usage, type View } from "./agent-view.ts";

export type CcEvent = { pane: number; epoch: string; runId: string; seq: number; kind: "agent" | "result" | "state"; payload: any };

const MAX = 64 * 1024;
const clip = (s: string) => (s.length > MAX ? `${s.slice(0, MAX)}…` : s);
const str = (x: unknown) => (typeof x === "string" ? x : "");

/// tool result/error content: a string, or text blocks; anything else as bounded JSON
export function text(x: unknown): string {
  if (typeof x === "string") return clip(x);
  if (Array.isArray(x) && x.every((b) => typeof b?.text === "string")) return clip(x.map((b) => b.text).join("\n"));
  if (x === undefined || x === null) return "";
  return clip(JSON.stringify(x, null, 2) ?? "");
}

const count = (x: unknown) => (typeof x === "number" && Number.isFinite(x) ? x : null);
export const usage = (u: any): Usage => ({ input: count(u?.inputTokens), output: count(u?.outputTokens), cacheRead: count(u?.cacheReadTokens), cacheWrite: count(u?.cacheWriteTokens) });

/// prompt + output of one model request; null when the CLI sent no numbers
export function requestSize(u: any): number | null {
  const parts = [u?.inputTokens, u?.cacheReadTokens, u?.cacheWriteTokens, u?.outputTokens].map(count);
  return parts.every((p) => p === null) ? null : parts.reduce<number>((a, b) => a + (b ?? 0), 0);
}

/// message `content` array → blocks, plus the tool facts its tool_use / tool_result items carry
function content(items: unknown): { blocks: Block[]; tools: Action[] } {
  const blocks: Block[] = [];
  const tools: Action[] = [];
  for (const b of Array.isArray(items) ? items : []) {
    if (b?.type === "text" && typeof b.text === "string") blocks.push({ kind: "text", text: clip(b.text) });
    else if (b?.type === "thinking") blocks.push({ kind: "thinking", text: clip(str(b.thinking) || str(b.text)) });
    else if (b?.type === "tool_use" && typeof b.id === "string") {
      blocks.push({ kind: "tool", id: b.id });
      tools.push({ t: "tool", id: b.id, name: str(b.name), input: b.input ?? null });
    } else if (b?.type === "tool_result" && typeof b.tool_use_id === "string")
      tools.push({ t: "tool", id: b.tool_use_id, output: text(b.content), status: b.is_error ? "error" : "done" });
  }
  return { blocks, tools };
}

const tool = (e: any, status?: ToolStatus, extra: Record<string, unknown> = {}): Action[] =>
  typeof e.toolCallId === "string" ? [{ t: "tool", id: e.toolCallId, ...(e.toolName ? { name: e.toolName } : {}), ...(status ? { status } : {}), ...extra }] : [];

/// one CLI AgentEvent → actions
export function agentActions(e: any): Action[] {
  switch (e?.type) {
    case "run_start":
      return typeof e.sessionId === "string" ? [{ t: "session", id: e.sessionId }] : [];
    case "message_start":
    case "stream_restart":
      return [{ t: "restart" }];
    case "text_delta":
      return [{ t: "text", delta: str(e.delta) }];
    case "thinking_start":
    case "thinking_delta":
      return [{ t: "thinking", delta: str(e.delta) }];
    case "thinking_end":
      return [{ t: "thinking_end", text: clip(str(e.text)) }];
    case "message_update":
    case "message_end": {
      const c = content(e.content);
      // message first keeps text/tool order; the reducer dedups the card when `tool_queued` follows
      return [{ t: "message", blocks: c.blocks, final: e.type === "message_end" }, ...c.tools];
    }
    case "model_request_start":
      return [{ t: "request", model: str(e.model) || null }];
    case "model_request_end":
      return [{ t: "request", model: str(e.model) || null, context: requestSize(e.usage) }];
    case "tool_queued":
      return tool(e, "queued", { input: e.input ?? null });
    case "tool_running":
      return tool(e, "running", e.description ? { note: clip(str(e.description)) } : {});
    case "tool_update":
      return tool(e, undefined, { partial: text(e.partial) });
    case "tool_completed":
      return tool(e, "done", { output: text(e.result) });
    case "tool_errored":
      return tool(e, "error", { output: text(e.error) });
    case "tool_denied":
      return tool(e, "denied");
    case "tool_hook_blocked":
      return tool(e, "blocked", { output: text(e.hookOutput) });
    case "subagent_start":
    case "subagent_progress":
    case "subagent_stop":
      return tool(e, undefined, { note: `${str(e.subagentType) || "subagent"}${typeof e.tokensUsed === "number" ? ` · ${e.tokensUsed}` : ""}` });
    case "api_retry":
      return [{ t: "notice", level: "warn", key: "cc.n.retry", params: { n: Number(e.attempt) || 1 } }];
    case "compaction_done":
      return [{ t: "notice", level: "info", key: "cc.n.compacted" }];
    case "compaction_outcome":
      return e.outcome === "failed" ? [{ t: "notice", level: "warn", key: "cc.n.compactFailed" }] : [];
    case "notice":
      return [{ t: "notice", level: e.level === "error" ? "error" : e.level === "warn" || e.level === "warning" ? "warn" : "info", key: "cc.n.text", params: { text: clip(str(e.message)) } }];
    case "mod_error":
      return [{ t: "notice", level: "error", key: "cc.n.mod", params: { mod: str(e.modId), text: clip(text(e.error)) } }];
    case "skill_loaded":
      return [{ t: "notice", level: "info", key: "cc.n.skill", params: { name: str(e.name) } }];
    case "permission_mode_changed":
      return [{ t: "notice", level: "info", key: "cc.n.mode", params: { mode: str(e.mode) } }];
    case "interrupted":
    case "run_error":
      return [{ t: "notice", level: "error", key: "cc.n.text", params: { text: clip(text(e.error) || e.type) } }];
    // run/round boundaries, CLI-internal recovery and diagnostics: nothing on screen
    case "run_end":
    case "turn_start":
    case "turn_end":
    case "tool_hooks":
    case "tool_input_coerced":
    case "tool_input_repaired":
    case "compaction_start":
    case "continuation_recovery":
    case "config_setting_changed":
    case "session_titled":
    case "model_trace":
      return [];
    default:
      return [{ t: "notice", level: "info", key: "cc.n.unknown", params: { type: clip(str(e?.type) || "?").slice(0, 64) } }];
  }
}

export function actions(ev: CcEvent): Action[] {
  const p = ev.payload ?? {};
  if (ev.kind === "agent") return agentActions(p);
  if (ev.kind === "result") return [{ t: "result", usage: usage(p.usage), durationMs: count(p.durationMs), error: p.error ? text(p.error) : null, finalText: typeof p.finalText === "string" ? p.finalText : null }];
  // only an explicit "yolo" from the daemon widens the view; anything else reads as restricted
  const access: Action[] = "access" in p ? [{ t: "access", mode: p.access === "yolo" ? "yolo" : "restricted" }] : [];
  if (typeof p.sessionId === "string") access.push({ t: "session", id: p.sessionId });
  if (p.state === "working") return [...access, { t: "run", runId: ev.runId, at: Date.now() }];
  // only the run's own end carries `exitCode`; a state without it is a policy or session change between runs (grant,
  // revoke, resume re-sends the last state), not the end of a run
  if (!("exitCode" in p)) return access;
  return [...access, { t: "end", state: str(p.state) || "error", exitCode: count(p.exitCode) }];
}

/// "ok" applied, "dup" already seen, "resync" = other daemon epoch or a gap: take a full snapshot
export function onEvent(v: View, ev: CcEvent): "ok" | "dup" | "resync" {
  if (ev.epoch !== v.epoch) return "resync";
  if (ev.seq <= v.seq) return "dup";
  if (ev.seq !== v.seq + 1) return "resync";
  v.seq = ev.seq;
  for (const a of actions(ev)) apply(v, a);
  return "ok";
}

/// `cc.attach` snapshot (daemon-reduced state, survives ring eviction) → view. It has no user prompts or run borders:
/// earlier output shows as one block of history.
export function fromSnapshot(s: any, epoch: string, through: number): View {
  const v = emptyView();
  v.epoch = epoch;
  v.seq = through;
  v.sessionId = typeof s?.sessionId === "string" ? s.sessionId : null;
  v.truncated = s?.truncated === true;
  v.access = s?.access === "yolo" ? "yolo" : "restricted";
  const running = s?.state === "working";
  v.runId = typeof s?.runId === "string" ? s.runId : null;
  for (const [id, t] of Object.entries<any>(s?.tools ?? {})) {
    const status: ToolStatus =
      { tool_queued: "queued", tool_running: "running", tool_completed: "done", tool_errored: "error", tool_denied: "denied", tool_hook_blocked: "blocked" }[str(t?.type)] as ToolStatus ?? "queued";
    v.tools[id] = { id, name: str(t?.toolName), input: t?.input ?? null, status, output: text(t?.result ?? t?.error ?? t?.hookOutput) || undefined, partial: t?.partial ? text(t.partial) : undefined };
  }
  const messages: any[] = Array.isArray(s?.messages) ? s.messages : [];
  if (!messages.length && !running && !s?.result && !Object.keys(v.tools).length) return v;
  v.turns.push({ role: "agent", runId: v.runId ?? "", at: Date.now(), blocks: [], draft: [], state: running ? "starting" : "done" });
  v.status = running ? "starting" : "done";
  for (const m of messages) for (const a of agentActions({ type: "message_end", content: m })) apply(v, a);
  if (running) {
    for (const a of agentActions({ type: "message_update", content: s.content ?? [] })) apply(v, a);
    if (s.thinking) apply(v, { t: "thinking", delta: s.thinking });
    if (s.text) apply(v, { t: "text", delta: s.text });
  }
  // tools no message referenced yet still get their card
  for (const id of Object.keys(v.tools)) apply(v, { t: "tool", id });
  if (s?.result) apply(v, { t: "result", usage: usage(s.result.usage), durationMs: count(s.result.durationMs), error: s.result.error ? text(s.result.error) : null, finalText: str(s.result.finalText) || null });
  if (!running && typeof s?.state === "string" && s.state !== "idle") apply(v, { t: "end", state: s.state, exitCode: count(s.exitCode) });
  return v;
}

/// `cc.resume` history (committed transcript, active branch only) → turns of `v`, keeping its epoch/seq so the live
/// stream still lines up. User messages that only carry tool results don't become a "You" bubble.
export function loadHistory(v: View, sessionId: string, messages: any[]): void {
  v.turns = [];
  v.tools = {};
  v.sessionId = sessionId;
  v.runId = null;
  v.status = "idle";
  let agent = false; // an agent turn is open for the messages that follow
  for (const m of Array.isArray(messages) ? messages : []) {
    const items: any[] = Array.isArray(m?.content) ? m.content : [];
    const said = items.filter((b) => b?.type === "text" && typeof b.text === "string").map((b) => b.text).join("\n");
    if (m?.role === "user" && said.trim()) {
      v.turns.push({ role: "user", text: clip(said), at: 0 });
      agent = false;
    }
    const c = content(m?.role === "user" ? items.filter((b) => b?.type === "tool_result") : items);
    if (!c.blocks.length && !c.tools.length) continue;
    if (!agent) v.turns.push({ role: "agent", runId: "", at: 0, blocks: [], draft: [], state: "done" });
    agent = true;
    for (const a of [{ t: "message", blocks: c.blocks, final: true } as Action, ...c.tools]) apply(v, a);
  }
  v.status = "idle";
}

/// edit_file / write_file input → proposed before/after text (diff of the INPUT, not proof the file changed)
export function proposal(name: string, input: any): { path: string; before: string; after: string } | null {
  if (!input || typeof input !== "object") return null;
  const path = str(input.file_path) || str(input.path);
  if (name === "edit_file" && typeof input.old_string === "string" && typeof input.new_string === "string") return { path, before: input.old_string, after: input.new_string };
  if (name === "write_file" && typeof input.content === "string") return { path, before: "", after: input.content };
  return null;
}

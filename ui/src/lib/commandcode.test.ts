// @ts-nocheck -- the real mod API is exercised without calling a model.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { onEvent, fromSnapshot, agentActions, proposal, requestSize, loadHistory } from "./commandcode.ts";
import { emptyView } from "./agent-view.ts";

test("Command Code mod reports ordered status and never answers permission prompts", async () => {
  const dir = await mkdtemp(join(tmpdir(), "gembala-commandcode-"));
  try {
    const log = join(dir, "events.jsonl");
    const exe = join(dir, "gembala");
    await writeFile(exe, `#!${process.execPath}
import fs from 'node:fs';
let payload = '';
process.stdin.on('data', c => payload += c);
process.stdin.on('end', () => fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({args: process.argv.slice(2), payload: JSON.parse(payload)}) + '\\n'));
`, { mode: 0o700 });
    const source = await readFile(new URL("../../../core/src/commandcode.mjs", import.meta.url), "utf8");
    const mod = join(dir, "gembala.mjs");
    await writeFile(mod, source.replace("__EXE__", JSON.stringify(exe)).replace("__HINT__", JSON.stringify("Gembala hint")));
    const hooks = {};
    const events = {};
    const { default: load } = await import(pathToFileURL(mod).href);
    load({ hooks: h => Object.assign(hooks, h), on: (name, f) => events[name] = f });
    assert.equal(hooks.appendSystemPrompt(), "Gembala hint");
    await hooks.onSessionStart();
    events.run_start({ sessionId: "session-123" });
    assert.equal(hooks.onPermissionRequest({ toolName: "shell_command" }), undefined);
    events.tool_running({});
    await hooks.onRunEnd({ result: { finalText: "Done" } });
    const lines = (await readFile(log, "utf8")).trim().split("\n").map(JSON.parse);
    assert.deepEqual(lines.map(l => l.args), ["start", "busy", "permission", "busy", "end"].map(event => ["hook", "commandcode", event, "-"]));
    assert.equal(lines[4].payload.session_id, "session-123");
    assert.equal(lines[4].payload.last_assistant_message, "Done");
    await hooks.onSessionStart();
    const last = JSON.parse((await readFile(log, "utf8")).trim().split("\n").at(-1));
    assert.equal(last.payload.session_id, undefined);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

// PLAN-CC-PANE phase 3 adapter: cc.event envelopes from the daemon (shape of core/src/daemon/commandcode.rs)
const ev = (seq, kind, payload, epoch = "e1", runId = "r1") => ({ pane: 1, epoch, runId, seq, kind, payload });

test("cc.event: fixture stream renders once, dedups seq, asks for a snapshot on gap or new epoch", () => {
  const v = emptyView();
  v.epoch = "e1";
  const stream = [
    ev(1, "state", { state: "working", access: "restricted" }),
    ev(2, "agent", { type: "run_start", sessionId: "fixture-session" }),
    ev(3, "agent", { type: "text_delta", delta: "Halo 🌱", extra: "ignored" }),
    ev(4, "agent", { type: "future_event", payload: { ignored: true } }),
    ev(5, "agent", { type: "message_end", content: [{ type: "text", text: "Halo 🌱" }] }),
    ev(6, "result", { subtype: "success", sessionId: "fixture-session", usage: { inputTokens: 12, outputTokens: 3, cacheReadTokens: 0, cacheWriteTokens: 0 }, durationMs: 25, finalText: "Halo 🌱" }),
    ev(7, "state", { state: "done", exitCode: 0 }),
  ];
  for (const e of stream) assert.equal(onEvent(v, e), "ok");
  assert.equal(onEvent(v, stream[3]), "dup");
  const turn = v.turns.at(-1);
  assert.deepEqual(turn.blocks.filter((b) => b.kind === "text"), [{ kind: "text", text: "Halo 🌱" }]);
  assert.equal(turn.blocks.find((b) => b.kind === "notice").params.type, "future_event", "unknown event shown, not dropped");
  assert.equal(v.sessionId, "fixture-session");
  assert.equal(v.status, "done");
  assert.deepEqual(v.usage, { input: 12, output: 3, cacheRead: 0, cacheWrite: 0 });
  assert.equal(onEvent(v, ev(9, "agent", { type: "text_delta", delta: "x" })), "resync");
  assert.equal(onEvent(v, ev(8, "agent", { type: "text_delta", delta: "x" }, "e2")), "resync");
  assert.equal(v.seq, 7);
});

test("cc.event: denied/errored tools are terminal, edit input is only a proposal", () => {
  const out = agentActions({ type: "tool_denied", toolCallId: "t1", toolName: "edit_file" });
  assert.deepEqual(out, [{ t: "tool", id: "t1", name: "edit_file", status: "denied" }]);
  assert.deepEqual(agentActions({ type: "tool_completed", toolCallId: "t2", result: [{ type: "text", text: "ok" }] }), [{ t: "tool", id: "t2", status: "done", output: "ok" }]);
  assert.deepEqual(proposal("edit_file", { file_path: "a.ts", old_string: "x", new_string: "y" }), { path: "a.ts", before: "x", after: "y" });
  assert.deepEqual(proposal("write_file", { path: "b.ts", content: "z" }), { path: "b.ts", before: "", after: "z" });
  assert.equal(proposal("read_file", { path: "c" }), null);
  assert.equal(requestSize({}), null, "missing usage is unknown, not 0");
  assert.equal(requestSize({ inputTokens: 100, cacheReadTokens: 50, outputTokens: 7 }), 157);
});

test("cc.attach snapshot rebuilds a view that keeps reduced state after ring eviction", () => {
  const v = fromSnapshot({
    state: "working", runId: "r9", sessionId: "s", truncated: true,
    messages: [[{ type: "text", text: "old" }, { type: "tool_use", id: "t1", name: "read_file", input: { path: "a" } }]],
    tools: { t1: { type: "tool_completed", toolName: "read_file", input: { path: "a" }, result: "body" }, t2: { type: "tool_running", toolName: "grep" } },
    text: "now", thinking: "", content: null,
  }, "e1", 40);
  assert.equal(v.seq, 40);
  assert.equal(v.status, "writing");
  assert.equal(v.truncated, true);
  const turn = v.turns.at(-1);
  assert.deepEqual(turn.blocks.filter((b) => b.kind === "tool").map((b) => b.id), ["t1", "t2"]);
  assert.equal(v.tools.t1.status, "done");
  assert.deepEqual(turn.draft, [{ kind: "text", text: "now" }]);
  assert.equal(onEvent(v, ev(41, "state", { state: "interrupted", exitCode: 130 }, "e1", "r9")), "ok");
  assert.equal(v.tools.t2.status, "unknown");
});

test("cc.resume history: user prompts, tool results paired by id, live seq kept", () => {
  const v = emptyView();
  v.epoch = "e1";
  v.seq = 12;
  loadHistory(v, "fixture-session", [
    { role: "user", content: [{ type: "text", text: "Read fixture" }] },
    { role: "assistant", content: [{ type: "thinking", thinking: "Inspect fixture" }, { type: "tool_use", id: "tool-1", name: "read_file", input: { path: "a" } }] },
    { role: "user", content: [{ type: "tool_result", tool_use_id: "tool-1", content: [{ type: "text", text: "fixture content" }], is_error: false }] },
    { role: "assistant", content: [{ type: "text", text: "Done" }] },
  ]);
  assert.deepEqual(v.turns.map((t) => t.role), ["user", "agent"], "tool-result-only user message is not a bubble");
  assert.deepEqual(v.turns[1].blocks.map((b) => b.kind), ["thinking", "tool", "text"]);
  assert.equal(v.tools["tool-1"].status, "done");
  assert.equal(v.tools["tool-1"].output, "fixture content");
  assert.equal(v.sessionId, "fixture-session");
  assert.equal(v.seq, 12);
  assert.equal(v.status, "idle");
});

test("access comes only from the daemon: yolo on grant, restricted on revoke or anything else", () => {
  const v = emptyView();
  v.epoch = "e1";
  assert.equal(v.access, "restricted");
  assert.equal(onEvent(v, ev(1, "state", { state: "idle", access: "yolo" })), "ok");
  assert.equal(v.access, "yolo");
  assert.equal(v.turns.length, 0, "a grant between runs is not a run");
  assert.equal(onEvent(v, ev(2, "state", { state: "done", sessionId: "s2" })), "ok");
  assert.equal(v.turns.length, 0, "resume re-sending the last state is not a run end");
  assert.equal(v.sessionId, "s2");
  v.seq = 1;
  onEvent(v, ev(2, "state", { state: "working", access: "yolo" }));
  onEvent(v, ev(3, "state", { access: "weird" }));
  assert.equal(v.access, "restricted");
  assert.equal(fromSnapshot({ state: "idle", access: "yolo" }, "e1", 0).access, "yolo");
  assert.equal(fromSnapshot({ state: "idle", access: "YOLO" }, "e1", 0).access, "restricted");
});

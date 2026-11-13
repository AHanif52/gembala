// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { apply, emptyView, num, type View } from "./agent-view.ts";

const run = (v: View, id = "r1") => apply(v, { t: "run", runId: id, at: 0 });
const agent = (v: View) => v.turns.at(-1) as Extract<View["turns"][number], { role: "agent" }>;

test("delta + snapshot + end don't duplicate the message", () => {
  const v = emptyView();
  run(v);
  apply(v, { t: "text", delta: "Hal" });
  apply(v, { t: "text", delta: "o" });
  assert.deepEqual(agent(v).draft, [{ kind: "text", text: "Halo" }]);
  apply(v, { t: "message", blocks: [{ kind: "text", text: "Halo" }], final: false });
  apply(v, { t: "message", blocks: [{ kind: "text", text: "Halo!" }], final: true });
  assert.deepEqual(agent(v).blocks, [{ kind: "text", text: "Halo!" }]);
  assert.deepEqual(agent(v).draft, []);
  apply(v, { t: "result", usage: { input: 5, output: 2, cacheRead: null, cacheWrite: null }, durationMs: 10, error: null, finalText: "Halo!" });
  apply(v, { t: "end", state: "done", exitCode: 0 });
  assert.equal(agent(v).blocks.filter((b) => b.kind === "text").length, 1, "finalText not appended again");
  assert.equal(v.status, "done");
});

test("parallel tools are told apart by id and a queued tool after tool_use is one card", () => {
  const v = emptyView();
  run(v);
  apply(v, { t: "tool", id: "a", name: "read_file", input: { path: "x" } });
  apply(v, { t: "message", blocks: [{ kind: "tool", id: "a" }, { kind: "tool", id: "b" }], final: true });
  apply(v, { t: "tool", id: "b", name: "read_file", status: "queued" });
  apply(v, { t: "tool", id: "a", status: "running" });
  apply(v, { t: "tool", id: "a", status: "done", output: "A" });
  assert.deepEqual(agent(v).blocks, [{ kind: "tool", id: "a" }, { kind: "tool", id: "b" }]);
  assert.equal(v.tools.a.status, "done");
  assert.equal(v.tools.b.status, "queued");
  assert.deepEqual(v.tools.a.input, { path: "x" }, "status updates keep the original input");
});

test("stream restart drops only the active draft", () => {
  const v = emptyView();
  run(v);
  apply(v, { t: "message", blocks: [{ kind: "text", text: "round 1" }], final: true });
  apply(v, { t: "tool", id: "t", status: "done" });
  apply(v, { t: "thinking", delta: "half" });
  apply(v, { t: "text", delta: "half" });
  apply(v, { t: "restart" });
  assert.deepEqual(agent(v).draft, []);
  assert.deepEqual(agent(v).blocks, [{ kind: "text", text: "round 1" }, { kind: "tool", id: "t" }]);
});

test("usage: run results replace per-request numbers, session sums only results, missing stays null", () => {
  const v = emptyView();
  run(v);
  apply(v, { t: "request", model: "m", context: 900 });
  apply(v, { t: "request", context: 1200 });
  assert.equal(v.context, 1200, "last request, not a sum");
  assert.equal(v.usage, null);
  apply(v, { t: "result", usage: { input: 10, output: 3, cacheRead: null, cacheWrite: 1 }, durationMs: 1, error: null, finalText: null });
  run(v, "r2");
  apply(v, { t: "result", usage: { input: 5, output: 2, cacheRead: null, cacheWrite: null }, durationMs: 1, error: null, finalText: null });
  assert.deepEqual(v.usage, { input: 15, output: 5, cacheRead: null, cacheWrite: 1 });
  assert.equal(num(null, "id"), "—");
  assert.equal(num(8240, "id"), "8.240");
  assert.equal(num(8240, "en"), "8,240");
});

test("a stopped run leaves active tools unknown and never done", () => {
  const v = emptyView();
  run(v);
  apply(v, { t: "tool", id: "w", name: "write_file", status: "running" });
  apply(v, { t: "tool", id: "d", name: "edit_file", status: "denied" });
  apply(v, { t: "end", state: "interrupted", exitCode: 130 });
  assert.equal(v.tools.w.status, "unknown");
  assert.equal(v.tools.d.status, "denied");
  assert.equal(v.status, "interrupted");
});

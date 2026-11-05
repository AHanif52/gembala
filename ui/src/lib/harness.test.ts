// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { canStart, clock, live, stagePanes, stepView, testCommand } from "./harness.ts";

const step = (id, pane = null) => ({ id, name: id, kind: "work", human: false, shell: false, gate: false, agents: [], output: "", pane, pane_name: pane ? `p${pane}` : null, pane_agent: pane ? "claude" : null, message: null });
const snap = (over) => ({ run: "r", recipe: "t", name: "T", workspace: "w", cwd: "/x", started_at: 0, state: "running", gate: null, current: 0, halt: null, halt_step: null, round: 0, max_rounds: 2, steps: [step("a", 1), step("b"), step("c")], trace: [], ...over });

test("Test: line of the workspace brief", () => {
  assert.equal(testCommand("Goal: x\nTest: npm test -- --run\nOther"), "npm test -- --run");
  assert.equal(testCommand("  test:   cargo test  "), "cargo test");
  assert.equal(testCommand("Test:"), "");
  assert.equal(testCommand("run the Test: later"), "");
  assert.equal(testCommand(undefined), "");
  assert.equal(testCommand("Test: " + "x".repeat(900)).length, 500);
});

test("stage views follow the run state", () => {
  const v = (s) => s.steps.map((_, i) => stepView(s, i));
  assert.deepEqual(v(snap({ current: 1 })), ["done", "now", "todo"]);
  assert.deepEqual(v(snap({ state: "gate", gate: 1, current: null })), ["done", "gate", "todo"]);
  assert.deepEqual(v(snap({ state: "done", current: null })), ["done", "done", "done"]);
  assert.deepEqual(v(snap({ state: "halted", halt: "exhausted", halt_step: 1, current: null })), ["done", "failed", "todo"]);
  // stopped with no step recorded: the furthest stage in the trail
  assert.deepEqual(v(snap({ state: "halted", halt: "stopped", current: null, trace: [{ at: 0, code: "send", step: 2, agent: "claude" }] })), ["done", "done", "failed"]);
  assert.deepEqual(v(snap({ state: "halted", halt: "stopped", current: null })), ["failed", "todo", "todo"]);
});

test("panes of a run and who may start", () => {
  assert.deepEqual(stagePanes(snap({})), [{ pane: 1, step: 0, name: "p1", agent: "claude" }]);
  assert.equal(live(snap({})), true);
  assert.equal(live(snap({ state: "done" })), false);
  assert.equal(live(null), false);
  assert.equal(canStart([{ ok: true, blocking: true }, { ok: false, blocking: false }]), true);
  assert.equal(canStart([{ ok: false, blocking: true }]), false);
  assert.equal(canStart(undefined), false);
  assert.equal(clock(65), "1:05");
});

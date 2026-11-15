// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { canStart, clock, findingFile, live, restartDefault, restartStages, stagePanes, stepView, testCommand, stuckStep } from "./harness.ts";

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

test("stopped run: where it is stuck, what to read, where a restart may begin", () => {
  const steps = [step("plan"), { ...step("execute"), output: "" }, { ...step("test"), kind: "check", shell: true }, { ...step("scan"), kind: "check", output: "scan.md" }, { ...step("ship"), human: true }];
  const halted = (o) => snap({ state: "halted", steps, ...o });
  assert.equal(stuckStep(halted({ halt_step: 3 })), 3);
  // max rounds has no halt_step: the last check that failed
  const trace = [{ at: 1, code: "fail", step: 3, agent: "omp" }, { at: 2, code: "fix", step: 3, agent: "claude" }, { at: 3, code: "shell_fail", step: 2, agent: null }];
  assert.equal(stuckStep(halted({ halt: "max_rounds", trace })), 2);
  assert.equal(stuckStep(halted({ trace: [] })), null);
  assert.equal(findingFile(halted({ halt_step: 3 })), "scan.md");
  assert.equal(findingFile(halted({ halt_step: 2 })), "test.log", "a test stage's findings are the log");
  assert.equal(findingFile(halted({ halt_step: 1 })), null, "a stage with no output file has nothing to read");
  assert.equal(findingFile(halted({ halt_step: 4 })), null, "Ship");
  assert.equal(findingFile(halted({ trace: [] })), null);
  assert.deepEqual(restartStages(halted({})), [0, 1, 2, 3], "Ship is never a place to begin");
  assert.equal(restartDefault(halted({})), 1, "Execute");
  assert.equal(restartDefault(halted({ steps: [step("a"), step("b")] })), 0, "no Execute = the first");
});

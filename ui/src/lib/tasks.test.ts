// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { openCount, split, tally, whyKey } from "./tasks.ts";

const kid = (state, extra = {}) => ({ id: "m", from: "a", to: "b", to_pane: 1, agent: "omp", title: "x", at: 1, state, why: null, result: null, outcome: null, ...extra });
const task = (kids, done = null) => ({ id: "t", kind: "task", title: "x", owner: { name: "a", pane: 1, agent: "claude" }, at: 1, done, kids });

test("open tasks are the ones without a done time; done ones go to the fold", () => {
  const ts = [task([]), task([], 9), task([kid("running")])];
  assert.equal(openCount(ts), 2);
  assert.deepEqual([split(ts).open.length, split(ts).done.length], [2, 1]);
});

test("tally: what the owner still waits for, and the three words that need a look", () => {
  const t = task([kid("limit", { to: "c" }), kid("held", { to: "d" }), kid("failed", { to: "e" }), kid("done"), kid("stuck"), kid("running", { to: "f" })]);
  assert.deepEqual(tally(t), { waiting: 4, failed: 1, held: 1, limit: 1, names: ["c", "d", "e", "f"] });
  assert.equal(tally(task([kid("done")])).waiting, 0);
});

test("whyKey: a reason only for a failure or a gate", () => {
  assert.equal(whyKey(kid("failed", { why: "pane_closed" })), "task.why.pane_closed");
  assert.equal(whyKey(kid("failed")), "task.why.other");
  assert.equal(whyKey(kid("waiting", { why: "gate" })), "task.why.gate");
  assert.equal(whyKey(kid("running")), "");
});

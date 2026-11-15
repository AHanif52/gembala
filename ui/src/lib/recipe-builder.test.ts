// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { openRecipe, patchRecipe, addAgent, addStep, canMove, diagsFor, diagsRecipe, dropIndex, modelOf, moveAgent, moveStep, removeAgent, removeStep, setModel, setPrimary, skipped, slugId } from "./recipe-builder.ts";

const st = (id, o = {}) => ({ id, kind: "work", name: id, need: "", instruction: "", agents: ["claude"], model: "", models: {}, effort: "", output: "", inputs: [], pass: "reply", timeout_secs: 900, gate: false, human: false, shell: false, max_cost: null, ...o });
const ship = st("ship", { human: true, agents: [], gate: true, timeout_secs: 0 });
const flow = () => [st("plan"), st("execute"), st("test", { shell: true, agents: [] }), st("audit", { kind: "check" }), ship, st("report")];
const ids = (a) => a.map((s) => s.id).join(",");
const LIB = ["claude", "codex", "omp", "opencode", "gemini", "commandcode"].map((agent) => ({ agent, installed: agent !== "omp", flags: {} }));

test("Ship never moves, and only a report-like step may follow it", () => {
  const f = flow();
  assert.equal(canMove(f, 4, 1), false, "Ship is locked");
  assert.equal(canMove(f, 1, 4), true, "plain work step after Ship = same rule as the daemon (report-like)");
  assert.equal(canMove([st("plan", { gate: true }), ship, st("r")], 0, 2), false, "gated step after Ship");
  assert.equal(canMove(f, 3, 4), false, "check after Ship");
  assert.equal(canMove(f, 2, 5), false, "shell step after Ship");
  assert.equal(canMove(f, 5, 0), true, "report may go before Ship");
  assert.equal(ids(moveStep(f, 3, 1)), "plan,audit,execute,test,ship,report");
  assert.equal(moveStep(f, 4, 0), f, "same array when refused");
  assert.equal(canMove(f, 1, 1), false);
  assert.equal(canMove(f, 1, 9), false);
});

test("drag slot from card middles", () => {
  assert.equal(dropIndex([10, 30, 50], 5), 0);
  assert.equal(dropIndex([10, 30, 50], 35), 2);
  assert.equal(dropIndex([10, 30, 50], 99), 3);
});

test("library cards go before Ship once, Ship cannot be removed", () => {
  const f = flow();
  const out = addStep(f, st("scan-security", { kind: "check" }));
  assert.equal(ids(out), "plan,execute,test,audit,scan-security,ship,report");
  assert.equal(addStep(out, st("scan-security")), out, "already in the flow");
  assert.equal(ids(removeStep(f, "audit")), "plan,execute,test,ship,report");
  assert.equal(removeStep(f, "ship").length, f.length);
  assert.equal(ids(addStep([st("a")], st("b"))), "a,b", "no Ship yet = at the end");
});

test("chain: max 3, no duplicates, only installed agents, never empty", () => {
  let s = st("execute");
  assert.equal(addAgent(s, "claude", LIB), s, "same agent twice");
  assert.equal(addAgent(s, "omp", LIB), s, "not installed");
  s = addAgent(addAgent(s, "codex", LIB), "gemini", LIB);
  assert.deepEqual(s.agents, ["claude", "codex", "gemini"]);
  assert.equal(addAgent(s, "commandcode", LIB), s, "4th refused");
  assert.equal(addAgent(ship, "codex", LIB), ship, "Ship has no agent");
  assert.equal(addAgent(st("test", { shell: true, agents: [] }), "codex", LIB).agents.length, 0);
  assert.deepEqual(removeAgent(s, "codex").agents, ["claude", "gemini"]);
  assert.equal(removeAgent(st("x"), "claude").agents.length, 1, "last agent stays");
});

test("card model follows whoever is first", () => {
  let s = st("plan", { agents: ["claude", "codex"], model: "opus", models: { codex: "gpt-6.1" } });
  assert.equal(modelOf(s, "claude"), "opus");
  assert.equal(modelOf(s, "codex"), "gpt-6.1");
  const swapped = moveAgent(s, 1, 0);
  assert.deepEqual(swapped.agents, ["codex", "claude"]);
  assert.equal(swapped.model, "gpt-6.1");
  assert.deepEqual(swapped.models, { claude: "opus" });
  const dropped = removeAgent(s, "claude");
  assert.equal(dropped.model, "gpt-6.1");
  assert.deepEqual(dropped.models, {});
  assert.deepEqual(setModel(s, "codex", "").models, {});
  assert.equal(setModel(s, "claude", " sonnet ").model, "sonnet");
  assert.equal(moveAgent(s, 0, 5), s);
});

test("check agents that also run Execute are marked skipped", () => {
  const f = [st("plan"), st("execute", { agents: ["claude", "codex"] }), st("audit", { kind: "check", agents: ["codex", "gemini", "claude"] }), ship];
  assert.deepEqual(skipped(f, 2), ["codex", "claude"]);
  assert.deepEqual(skipped(f, 1), []);
  assert.deepEqual(skipped([st("audit", { kind: "check" })], 0), []);
});

test("diagnostics land on their card or on the recipe", () => {
  const d = [{ code: "input_missing", args: { at: "step.execute.inputs", got: "plan.md" } }, { code: "no_work", args: { at: "step" } }, { code: "x" }];
  assert.equal(diagsFor(d, "execute").length, 1);
  assert.equal(diagsFor(d, "plan").length, 0);
  assert.equal(diagsRecipe(d).length, 2);
});

test("new recipe ids", () => {
  assert.equal(slugId("Fix Dock Bounce!", []), "fix-dock-bounce");
  assert.equal(slugId("fitur baru", ["fitur-baru"]), "fitur-baru-2");
  assert.equal(slugId("???", []), "resep");
});

test("primary picked from the card", () => {
  const s = st("plan", { agents: ["claude", "codex"], model: "opus", models: { codex: "gpt-6.1" } });
  assert.deepEqual(setPrimary(s, "codex").agents, ["codex", "claude"]);
  assert.equal(setPrimary(s, "codex").model, "gpt-6.1");
  const r = setPrimary(s, "gemini");
  assert.deepEqual(r.agents, ["gemini", "codex"]);
  assert.equal(r.model, "", "the replaced agent's model goes with it");
  assert.deepEqual(r.models, { codex: "gpt-6.1" });
  assert.equal(setPrimary(s, "claude"), s);
});

const rt = { key: "k1", ws: "w", rid: "fix", tpl: "", name: "Fix", st: "approved" };
test("openRecipe focuses the open tab of an existing recipe", () => {
  const r = openRecipe([rt], "w", "fix", "", "k2");
  assert.equal(r.tabs.length, 1);
  assert.equal(r.key, "k1");
});
test("openRecipe makes a tab per other recipe and per new recipe", () => {
  assert.equal(openRecipe([rt], "w", "other", "", "k2").tabs.length, 2);
  const n = openRecipe([rt], "w", "", "tpl", "k3");
  assert.deepEqual(n.tabs.map((x) => x.key), ["k1", "k3"]);
  assert.equal(n.tabs[1].st, "unsaved");
  assert.equal(openRecipe(n.tabs, "w", "", "tpl", "k4").tabs.length, 3);
});
test("patchRecipe patches one tab and keeps references when nothing changes", () => {
  const tabs = [rt, { ...rt, key: "k2", rid: "b" }];
  assert.equal(patchRecipe(tabs, "k2", { st: "dirty" })[1].st, "dirty");
  assert.equal(patchRecipe(tabs, "k2", { st: "dirty" })[0], rt);
  assert.equal(patchRecipe(tabs, "k1", { st: "approved" })[0], rt);
});

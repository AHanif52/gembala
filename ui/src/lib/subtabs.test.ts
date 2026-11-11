// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { strict as assert } from "node:assert";
import { test } from "node:test";
import { subtabRows } from "./subtabs.ts";

test("helpers stay below owner, quiet helpers fold, blocked helper opens list", () => {
  const owner = { id: "owner", helpersOpen: false, doneOpen: false };
  const tabs = [{ id: "other" }, { id: "quiet", parent: "owner" }, owner, { id: "wait", parent: "owner" }];
  const rows = subtabRows(tabs, (t) => t.id === "quiet", (t) => t.id === "wait");
  assert.deepEqual(rows.map((r) => `${r.kind}:${r.tab.id}`), ["tab:other", "tab:owner", "summary:owner", "tab:wait", "fold:owner"]);
  owner.doneOpen = true;
  assert.equal(subtabRows(tabs, (t) => t.id === "quiet", (t) => t.id === "wait").at(-1)?.tab.id, "quiet");
});

test("collapsed owner keeps its selected helper reachable", () => {
  const tabs = [{ id: "owner", helpersOpen: false }, { id: "done", parent: "owner" }];
  assert.deepEqual(subtabRows(tabs, () => true, () => false).map((r) => r.kind), ["tab", "summary"]);
  assert.deepEqual(subtabRows(tabs, () => true, () => false, "done").map((r) => r.kind), ["tab", "summary", "fold", "tab"]);
});

test("harness stages appear as separate rows under their owner", () => {
  const tabs = [
    { id: "owner", helpersOpen: true },
    { id: "stage-work", parent: "owner" },
    { id: "stage-review", parent: "owner" },
    { id: "unowned-stage" },
  ];
  assert.deepEqual(subtabRows(tabs, () => false, () => false).filter((r) => r.kind === "tab").map((r) => [r.tab.id, !!r.child]), [
    ["owner", false], ["stage-work", true], ["stage-review", true], ["unowned-stage", false],
  ]);
});

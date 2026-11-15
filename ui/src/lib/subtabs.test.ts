// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { strict as assert } from "node:assert";
import { test } from "node:test";
import { cleanGrid, gridInsert, gridLines, gridMove, gridRects, helperDots, helpersShown, splitRows, subtabRows } from "./subtabs.ts";

const kinds = (rows) => rows.map((r) => `${r.kind}:${r.tab.id}`);
const none = () => false;

test("one helper is a thin row under its owner, no summary", () => {
  const tabs = [{ id: "other" }, { id: "owner" }, { id: "h1", parent: "owner" }];
  assert.deepEqual(kinds(subtabRows(tabs, () => true, none)), ["tab:other", "tab:owner", "tab:h1"]);
});

test("two or more helpers: collapsed summary by default, quiet ones never show a row", () => {
  const tabs = [{ id: "owner" }, { id: "a", parent: "owner" }, { id: "b", parent: "owner" }];
  assert.deepEqual(kinds(subtabRows(tabs, () => true, none)), ["tab:owner", "summary:owner"]);
});

test("opens on its own for blocked, unseen done or the selected helper; quiet ones fold into one rest row", () => {
  const tabs = [{ id: "owner" }, { id: "wait", parent: "owner" }, { id: "q1", parent: "owner" }, { id: "q2", parent: "owner" }];
  const quiet = (t) => t.id.startsWith("q");
  const rows = subtabRows(tabs, quiet, (t) => t.id === "wait");
  assert.deepEqual(kinds(rows), ["tab:owner", "summary:owner", "tab:wait", "rest:owner"]);
  assert.equal(rows.at(-1).count, 2);
  assert.deepEqual(rows.at(-1).rest.map((t) => t.id), ["q1", "q2"]);
  assert.deepEqual(kinds(subtabRows(tabs, quiet, none, undefined, (t) => t.id === "wait")), ["tab:owner", "summary:owner", "tab:wait", "rest:owner"]);
  const sel = subtabRows(tabs, quiet, none, "q2");
  assert.deepEqual(kinds(sel), ["tab:owner", "summary:owner", "tab:wait", "tab:q2", "rest:owner"]);
  assert.deepEqual(sel.at(-1).rest.map((t) => t.id), ["q1"]);
});

test("manual open/close beats automatic", () => {
  const mk = (helpersOpen) => [{ id: "owner", helpersOpen }, { id: "w", parent: "owner" }, { id: "x", parent: "owner" }];
  assert.deepEqual(kinds(subtabRows(mk(false), none, (t) => t.id === "w")), ["tab:owner", "summary:owner"]);
  assert.deepEqual(kinds(subtabRows(mk(true), () => true, none)), ["tab:owner", "summary:owner", "rest:owner"]);
  const auto = mk(undefined);
  assert.equal(helpersShown(auto[0], auto.slice(1), (t) => t.id === "w", none), true);
  assert.equal(helpersShown(auto[0], auto.slice(1), none, none), false);
  assert.equal(helpersShown({ id: "o", helpersOpen: false }, [{ id: "w" }], () => true, none), false);
});

test("harness stages appear as separate rows under their owner", () => {
  const tabs = [{ id: "owner", helpersOpen: true }, { id: "stage-work", parent: "owner" }, { id: "stage-review", parent: "owner" }, { id: "unowned-stage" }];
  assert.deepEqual(subtabRows(tabs, none, none).filter((r) => r.kind === "tab").map((r) => [r.tab.id, !!r.child]), [
    ["owner", false], ["stage-work", true], ["stage-review", true], ["unowned-stage", false],
  ]);
});

test("summary icons: blocked, working, done, idle; max 4 and +N", () => {
  const kids = ["i1", "d1", "w1", "b1", "w2", "i2"].map((id) => ({ id }));
  const dot = (t) => ({ i: "idle", d: "done", w: "working", b: "blocked" })[t.id[0]];
  const { shown, more } = helperDots(kids, dot);
  assert.deepEqual(shown.map((s) => s.tab.id), ["b1", "w1", "w2", "d1"]);
  assert.equal(more, 2);
  assert.equal(helperDots(kids.slice(0, 2), dot).more, 0);
});

const owner = { id: "o" };
const kids = [{ id: "a", parent: "o" }, { id: "b", parent: "o" }, { id: "c", parent: "o" }, { id: "d", parent: "o" }, { id: "x", parent: "other" }];
const all = [owner, ...kids];

test("cleanGrid drops missing, foreign and duplicate ids, keeps the owner, caps at 4, <2 = none", () => {
  assert.equal(cleanGrid({ id: "o" }, all), undefined);
  assert.deepEqual(cleanGrid({ id: "o", grid: ["o", "a", "gone", "x", "a", "b"] }, all), ["o", "a", "b"]);
  assert.deepEqual(cleanGrid({ id: "o", grid: ["a", "b"] }, all), ["o", "a", "b"]);
  assert.deepEqual(cleanGrid({ id: "o", grid: ["a", "b", "c", "d", "o"] }, all), ["a", "b", "c", "o"]);
  assert.equal(cleanGrid({ id: "o", grid: ["o", "gone"] }, all), undefined);
  assert.equal(cleanGrid({ id: "o", grid: ["o"] }, all), undefined);
});

const area = (rs) => rs.reduce((s, r) => s + r.w * r.h, 0);

test("gridRects: 2/3/4 panes, columns and rows, ratio, always fills the box", () => {
  assert.deepEqual(gridRects(2), [{ x: 0, y: 0, w: 0.5, h: 1 }, { x: 0.5, y: 0, w: 0.5, h: 1 }]);
  assert.deepEqual(gridRects(2, true, { y: 0.25 }), [{ x: 0, y: 0, w: 1, h: 0.25 }, { x: 0, y: 0.25, w: 1, h: 0.75 }]);
  assert.deepEqual(gridRects(3, false, { x: 0.75, y: 0.25 }), [{ x: 0, y: 0, w: 0.75, h: 1 }, { x: 0.75, y: 0, w: 0.25, h: 0.25 }, { x: 0.75, y: 0.25, w: 0.25, h: 0.75 }]);
  assert.deepEqual(gridRects(3, true)[0], { x: 0, y: 0, w: 1, h: 0.5 });
  assert.equal(gridRects(3, true).length, 3);
  assert.deepEqual(gridRects(4, false, { x: 0.25, y: 0.75 })[3], { x: 0.25, y: 0.75, w: 0.75, h: 0.25 });
  for (const n of [2, 3, 4]) for (const rows of [false, true]) assert.ok(Math.abs(area(gridRects(n, rows, { x: 0.37, y: 0.64 })) - 1) < 1e-9);
  // out of range or junk ratios are clamped to 0.2..0.8
  assert.equal(gridRects(2, false, { x: 0.01 })[0].w, 0.2);
  assert.equal(gridRects(2, false, { x: 5 })[0].w, 0.8);
  assert.equal(gridRects(2, false, { x: NaN })[0].w, 0.5);
});

test("gridLines: one divider for 2, two for 3 and 4", () => {
  assert.deepEqual(gridLines(2), [{ axis: "x", x: 0.5, y: 0, len: 1 }]);
  assert.deepEqual(gridLines(2, true), [{ axis: "y", x: 0, y: 0.5, len: 1 }]);
  assert.equal(gridLines(3).length, 2);
  assert.deepEqual(gridLines(3)[1], { axis: "y", x: 0.5, y: 0.5, len: 0.5 });
  assert.equal(gridLines(4).length, 2);
  assert.equal(gridLines(1).length, 0);
});

test("gridInsert: into a slot, new grid from owner, full = refused, foreign helper refused", () => {
  assert.deepEqual(gridInsert(owner, all, "a", 1), ["o", "a"]);
  assert.deepEqual(gridInsert(owner, all, "a", 0), ["a", "o"]);
  assert.deepEqual(gridInsert({ id: "o", grid: ["o", "a"] }, all, "b", 1), ["o", "b", "a"]);
  assert.equal(gridInsert({ id: "o", grid: ["o", "a", "b", "c"] }, all, "d", 2), null);
  assert.equal(gridInsert(owner, all, "x", 1), null);
  assert.deepEqual(gridInsert({ id: "o", grid: ["o", "a"] }, all, "a", 0), ["o", "a"]);
});

test("gridMove: swap two slots, or reorder like a chip drag", () => {
  assert.deepEqual(gridMove(["o", "a", "b"], 0, 2, true), ["b", "a", "o"]);
  assert.deepEqual(gridMove(["o", "a", "b"], 2, 0, false), ["b", "o", "a"]);
  assert.deepEqual(gridMove(["o", "a"], 1, 1, true), ["o", "a"]);
  assert.deepEqual(gridMove(["o", "a"], 0, 5, true), ["o", "a"]);
});

test("split tab: agents get rows, idle shells fold, only-shells stays one row", () => {
  const agent = (p) => p === 1 || p === 7;
  const busy = (p) => p === 9;
  assert.deepEqual(splitRows([1, 6], agent, busy), { rows: [1], folded: [6] });
  assert.deepEqual(splitRows([1, 9, 6, 7], agent, busy), { rows: [1, 9, 7], folded: [6] });
  assert.equal(splitRows([6, 9], agent, busy), null);
  assert.equal(splitRows([1], agent, busy), null);
});

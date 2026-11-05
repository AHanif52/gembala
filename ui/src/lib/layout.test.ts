// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { dividers, neighbor, nudge, panes, place, rects, remove, replace, setRatio, split, swap, valid, type Node } from "./layout.ts";

// [1 | 2/3] : 1 left, 2 top right, 3 bottom right
const t3 = (): Node => split(split({ pane: 1 }, 1, "row", 2), 2, "col", 3);

test("split membagi dua dan menjaga urutan", () => {
  const n = t3();
  assert.deepEqual(panes(n), [1, 2, 3]);
  assert.deepEqual(rects(n).get(1), { x: 0, y: 0, w: 0.5, h: 1 });
  assert.deepEqual(rects(n).get(3), { x: 0.5, y: 0.5, w: 0.5, h: 0.5 });
});

test("remove menaikkan saudara, pane terakhir jadi null", () => {
  assert.deepEqual(remove(t3(), 2), { dir: "row", ratio: 0.5, a: { pane: 1 }, b: { pane: 3 } });
  assert.equal(remove({ pane: 1 }, 1), null);
  assert.deepEqual(remove(t3(), 9), t3());
});

test("place memindah pane ke sisi pane lain", () => {
  // 3 (bottom right) to the left of 1: [3 | 1 | 2]
  const n = place(t3(), 3, 1, "left");
  assert.deepEqual(panes(n), [3, 1, 2]);
  assert.equal(neighbor(n, 3, "right"), 1);
  // 1 below 2: 2 on top of 1, 3 below it
  const m = place(t3(), 1, 2, "down");
  assert.deepEqual(panes(m), [2, 1, 3]);
  assert.deepEqual(rects(m).get(2), { x: 0, y: 0, w: 1, h: 0.25 });
  // onto itself, or the only pane = unchanged
  assert.deepEqual(place(t3(), 2, 2, "up"), t3());
  assert.deepEqual(place({ pane: 1 }, 1, 9, "up"), { pane: 1 });
});

test("neighbor mengikuti geometri", () => {
  const n = t3();
  assert.equal(neighbor(n, 1, "right"), 2); // equal overlap, the first one
  assert.equal(neighbor(n, 3, "left"), 1);
  assert.equal(neighbor(n, 2, "down"), 3);
  assert.equal(neighbor(n, 3, "up"), 2);
  assert.equal(neighbor(n, 1, "left"), null);
});

test("dividers dan setRatio dengan batas", () => {
  const d = dividers(t3());
  assert.deepEqual(d.map((x) => [x.path, x.dir, x.at]), [["", "row", 0.5], ["b", "col", 0.5]]);
  const n = setRatio(t3(), "b", 0.99);
  assert.equal((n as any).b.ratio, 0.9);
});

test("nudge menggeser pemisah terdekat", () => {
  let n = nudge(t3(), 1, "right", 0.1);
  assert.ok(Math.abs((n as any).ratio - 0.6) < 1e-9);
  n = nudge(t3(), 3, "up", 0.1); // top/bottom divider on the right side
  assert.ok(Math.abs((n as any).b.ratio - 0.4) < 1e-9);
  assert.deepEqual(nudge({ pane: 1 }, 1, "left"), { pane: 1 });
});

test("replace menukar id pane, valid menolak bentuk salah", () => {
  assert.deepEqual(panes(replace(t3(), 2, -7)), [1, -7, 3]);
  assert.ok(valid(t3()));
  assert.ok(!valid({ dir: "row", ratio: 2, a: { pane: 1 }, b: { pane: 2 } }));
  assert.ok(!valid({ dir: "x", ratio: 0.5, a: { pane: 1 }, b: { pane: 2 } }));
  assert.ok(!valid({ pane: "1" }));
  assert.ok(!valid(null));
});

test("swap menukar posisi dua pane, rasio tetap", () => {
  const n = swap(t3(), 1, 3);
  assert.deepEqual(panes(n), [3, 2, 1]);
  assert.equal((n as any).ratio, (t3() as any).ratio);
});

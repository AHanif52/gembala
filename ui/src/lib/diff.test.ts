// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { lineDiff } from "./diff.ts";

test("diff baris resep", () => {
  const d = lineDiff("a\nb\nc\n", "a\nx\nc\nd\n").map((l) => l.t + l.s);
  assert.deepEqual(d, [" a", "+x", "-b", " c", "+d"]);
  assert.deepEqual(lineDiff("", "a\n"), [{ t: "+", s: "a" }]);
  assert.deepEqual(lineDiff("a", "a"), [{ t: " ", s: "a" }]);
});

// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { newer, noteLines, upgradeCmd } from "./update.ts";

test("versi lebih baru", () => {
  assert.equal(newer("0.1.1", "0.1.0"), true);
  assert.equal(newer("0.1.10", "0.1.9"), true);
  assert.equal(newer("0.2", "0.1.9"), true);
  assert.equal(newer("0.1.0", "0.1.0"), false);
  assert.equal(newer("0.0.9", "0.1.0"), false);
  assert.equal(newer("1.0.0-beta", "0.1.0"), false);
  assert.equal(newer(undefined, "0.1.0"), false);
});

test("perintah dan catatan rilis", () => {
  assert.equal(upgradeCmd({ brew: true }), "brew upgrade gembala");
  assert.equal(upgradeCmd(null), "git pull && scripts/pasang.sh");
  assert.deepEqual(noteLines("## Baru\n- **Notif** update\n\n* `x`\n1\n2\n3\n4"), ["Baru", "Notif update", "x", "1", "2"]);
});

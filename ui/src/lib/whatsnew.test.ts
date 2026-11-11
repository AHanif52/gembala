// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { sections } from "./whatsnew.ts";

const md = `## 0.1.10\n\n### English\n- New release\n\n### Indonesia\n- Rilis baru\n## 0.1.9\n- Older release\n## 0.1.8\n\n### English\n- Old English\n\n### Indonesia\n`;

test("versions between seen and current, newest first; old notes serve both languages", () => {
  assert.deepEqual(sections(md, "0.1.8", "0.1.10", "id"), [
    { version: "0.1.10", bullets: ["Rilis baru"] },
    { version: "0.1.9", bullets: ["Older release"] },
  ]);
  assert.deepEqual(sections(md, "0.1.8", "0.1.10", "en")[1].bullets, ["Older release"]);
});

test("downgrade returns no notes and empty Indonesian falls back to English", () => {
  assert.deepEqual(sections(md, "0.1.10", "0.1.9", "en"), []);
  assert.deepEqual(sections(md, "0.1.7", "0.1.8", "id"), [{ version: "0.1.8", bullets: ["Old English"] }]);
});

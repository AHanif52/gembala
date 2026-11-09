// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { codexStatus } from "./codex-status.ts";

test("footer Codex lengkap: bar = yang kepake, kayak status Claude", () => {
  assert.deepEqual(codexStatus("gpt-6.1-sol xhigh · Context 94% left · 258K window · 5h 99% left · weekly 100% left · 6K used · Workspace"), {
    model: "gpt-6.1-sol", effort: "xhigh", context: 6, fiveHour: 1, weekly: 0, window: "258K window", tokens: "6K used", dir: "Workspace",
  });
});

test("tanpa jatah 5 jam (API key / belum kebaca) tetap tampil", () => {
  assert.deepEqual(codexStatus("gpt-6.1-sol minimal · Context 75% left · 475K window · weekly 94% left · ~/app"), {
    model: "gpt-6.1-sol", effort: "minimal", context: 25, weekly: 6, window: "475K window", dir: "~/app",
  });
  assert.equal(codexStatus("gpt-6.1-sol · Context 10% used")?.context, 10);
});

test("bukan footer Codex diabaikan", () => {
  for (const line of ["Context 94% left · weekly 100% left", "ls -la · Context 5% left", "gpt-6 · 258K window", "gpt-6 high · Context 140% left", ""]) {
    assert.equal(codexStatus(line), null, line);
  }
});

// @ts-nocheck -- run by `npm test` (node --test), not part of the app build
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildRings, orderRings, relativeTime, tightest, toneOf } from "./usage.ts";

const now = 1_700_000_000;
const window = (kind, used_pct, resets_at = now + 3600) => ({ kind, used_pct, resets_at, model: null });
const row = (agent, login, windows, limited = false) => ({ agent, login, login_name: login, windows, limited, source: "codex_footer", read_at: now });

test("Command Code without allowance data shows an unknown ring", () => {
  const rings = buildRings(["commandcode"], [], [], [], now);
  assert.equal(rings.length, 1);
  assert.equal(rings[0].key, "commandcode:utama");
  assert.equal(rings[0].tone, "unknown");
  assert.equal(rings[0].left, null);
});

test("tightest live window sets ring color; expired data stays gray", () => {
  const codex = row("codex", "utama", [window("5h", 34), window("week", 96)]);
  assert.equal(tightest(codex, now).kind, "week");
  const rings = buildRings(["codex", "omp"], [], [], [codex, row("claude", "kantor", [window("5h", 82, now - 1)])], now);
  assert.equal(rings.find((r) => r.key === "codex:utama").tone, "red");
  assert.equal(rings.find((r) => r.key === "claude:kantor").stale, true);
  assert.equal(rings.find((r) => r.key === "omp:utama").tone, "unknown");
  assert.equal(toneOf(20, false), "yellow");
  assert.equal(toneOf(5, false), "red");
});

test("order only changes when urgency band changes", () => {
  const before = buildRings(["claude", "codex", "omp"], [], [], [row("claude", "utama", [window("5h", 40)]), row("codex", "utama", [window("5h", 60)])], now);
  const first = orderRings(before);
  const newer = buildRings(["claude", "codex", "omp"], [], [], [row("claude", "utama", [window("5h", 45)]), row("codex", "utama", [window("5h", 30)])], now);
  assert.deepEqual(orderRings(newer, first.map((x) => x.key)).map((x) => x.key), first.map((x) => x.key));
  assert.equal(relativeTime(now + 2 * 3600 + 54 * 60, now, "id"), "2 jam 54 mnt lagi");
});

test("configured active login keeps its own ring and limited signal wins", () => {
  const limited = row("claude", "kantor", [window("week", 40)], true);
  limited.login_name = "";
  const rings = buildRings(["claude"], [{ id: "kantor", agent: "claude", name: "Kantor" }], [{ agent: "claude", login: "kantor" }], [limited], now);
  assert.equal(rings.find((r) => r.key === "claude:kantor").loginName, "Kantor");
  assert.equal(rings.find((r) => r.key === "claude:kantor").tone, "red");
  assert.equal(rings.find((r) => r.key === "claude:utama").tone, "unknown");
});

// @ts-nocheck -- dijalankan `npm test` (node --test), bukan bagian build app
import { test } from "node:test";
import assert from "node:assert/strict";
import { activeAddons } from "./config.ts";

test("FR-78: add-on aktif ikut agent dan workspace pane", () => {
  const a = {
    plugins: [
      { id: "caveman", path: "~/p/c", scope: "all", workspaces: [] },
      { id: "ponytail", path: "~/p/p", scope: "workspaces", workspaces: ["w1"] },
      { id: "mati", path: "~/p/m", scope: "off", workspaces: ["w1"] },
    ],
    mcp: [{ workspace: "w1", file: "mcp/w1.json", strict: true, servers: ["context7", "pencil"] }],
  };
  assert.deepEqual(activeAddons(a, "claude", "w1"), ["caveman", "ponytail", "MCP: context7, pencil (only these)"]);
  assert.deepEqual(activeAddons(a, "claude", "w2"), ["caveman"]);
  assert.deepEqual(activeAddons(a, "codex", "w1"), ["MCP: context7, pencil"]);
  assert.deepEqual(activeAddons(a, "omp", "w1"), []);
  assert.deepEqual(activeAddons(a, "", "w1"), []);
  assert.deepEqual(activeAddons(undefined, "claude", "w1"), []);
});
